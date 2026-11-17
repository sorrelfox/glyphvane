// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The cards layout and its cover images.
//!
//! - Cards are `cardSize` wide ([`DEFAULT_CARD`] without it); as many fit
//!   a line as the view is wide. The lines (group headers and lines of
//!   cards) are a virtual list, built again when the width changes the
//!   number of cards per line.
//! - The cover is `imageAspectRatio` × the card's width high, the image
//!   filling it (`imageFit: cover`, cropped) or fitting in it (`contain`).
//!   A hex colour value fills it; a card without a cover value leaves it
//!   blank. Views without `image` have no cover.
//! - Covers from the vault are decoded on the low-priority pool by the
//!   reader's media engine (SVGs may only load files of the vault).
//!   Remote covers follow the remote-content setting: fetched (and decoded
//!   with no file access) where it allows them without asking; otherwise
//!   the cover says why it is not shown. A cover waiting for approval (Ask
//!   mode) offers *Load once*, as the Reading view's infobar does: the
//!   site's images load for this base (the base file, or the note that
//!   embeds it) for the rest of the session; nothing is written anywhere.
//!   Blocked covers offer nothing.
//! - A click on a card opens its file (Mod-click a new tab, a middle
//!   click a background tab).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClickEvent, Context, FontWeight, Global, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, ObjectFit, ParentElement as _, RenderImage, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Task, canvas, div, img, list,
    px,
};
use gv_bases::{Cover, ImageFit, Row, Value, ViewResult};
use gv_media::{CancelFlag, DecodeRequest};

use super::table::group_label;
use super::{BasesView, Colors};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};
use crate::metrics::{radius, text};
use crate::reader::Disposition;
use crate::reader::remote::{Decision, Policy, RemoteShown};

/// A card without `cardSize`.
pub(crate) const DEFAULT_CARD: f32 = 200.0;
const MIN_CARD: f32 = 100.0;
const MAX_CARD: f32 = 800.0;
/// Space between cards and around the grid.
const GAP: f32 = 16.0;
/// Card padding.
const PAD: f32 = 10.0;
/// A property line.
const LINE: f32 = 20.0;
/// The title (first property) line.
const TITLE: f32 = 22.0;
/// Covers decoded at this many device pixels per px.
const DECODE_SCALE: f32 = 2.0;

/// What a cover shows now.
#[derive(Clone)]
pub(crate) enum CoverShown {
    Pending,
    Image(Arc<RenderImage>),
    Color(Hsla),
    /// Why there is no image.
    Message(SharedString),
    /// A remote image waiting for approval: why, and the site *Load once*
    /// approves.
    Waiting {
        message: SharedString,
        host: SharedString,
    },
}

/// *Load once* approvals of remote covers in this session: (base, site).
/// Never written anywhere.
#[derive(Default)]
struct Approvals(HashSet<(PathBuf, String)>);

impl Global for Approvals {}

fn approved(cx: &App, doc: &std::path::Path, host: &str) -> bool {
    cx.try_global::<Approvals>()
        .is_some_and(|a| a.0.contains(&(doc.to_path_buf(), host.to_owned())))
}

/// Covers loaded or loading, by path or URL.
#[derive(Default)]
pub(crate) struct Covers {
    shown: HashMap<Arc<str>, CoverShown>,
    tasks: HashMap<Arc<str>, (CancelFlag, Task<()>)>,
    /// Decodes started (tests).
    pub started: usize,
}

impl Covers {
    pub fn clear(&mut self) {
        for (cancel, _) in self.tasks.values() {
            cancel.cancel();
        }
        self.tasks.clear();
        self.shown.clear();
    }

    /// Forgets the covers of `host`'s URLs, so they are asked for again.
    fn forget_host(&mut self, host: &str) {
        self.shown.retain(|key, _| {
            !(key.contains("://") && gv_media::remote::host_of(key).as_deref() == Some(host))
        });
    }
}

/// One line of the cards layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardLine {
    Group(usize),
    /// Cards `start..end` of group `g`.
    Cards(usize, usize, usize),
}

/// The lines of `result` with `columns` cards a line.
pub(crate) fn card_lines(result: &ViewResult, columns: usize) -> Vec<CardLine> {
    let columns = columns.max(1);
    let mut out = Vec::new();
    for (g, group) in result.groups.iter().enumerate() {
        if result.grouped {
            out.push(CardLine::Group(g));
        }
        let mut start = 0;
        while start < group.rows.len() {
            let end = (start + columns).min(group.rows.len());
            out.push(CardLine::Cards(g, start, end));
            start = end;
        }
    }
    out
}

/// A hex colour (`#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`) from a value.
fn hex_color(s: &str) -> Option<Hsla> {
    let hex = s.strip_prefix('#')?;
    let digit = |i: usize, n: usize| u8::from_str_radix(hex.get(i..i + n)?, 16).ok();
    let (r, g, b, a) = match hex.len() {
        3 | 4 => {
            let one = |i| digit(i, 1).map(|v| v * 17);
            (
                one(0)?,
                one(1)?,
                one(2)?,
                if hex.len() == 4 { one(3)? } else { 255 },
            )
        }
        6 | 8 => (
            digit(0, 2)?,
            digit(2, 2)?,
            digit(4, 2)?,
            if hex.len() == 8 { digit(6, 2)? } else { 255 },
        ),
        _ => return None,
    };
    Some(
        gpui::Rgba {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: f32::from(a) / 255.0,
        }
        .into(),
    )
}

impl BasesView {
    /// What `cover` shows, starting its load the first time (`fit`: the
    /// box in px).
    pub(crate) fn cover_shown(
        &mut self,
        cover: &Cover,
        fit: (f32, f32),
        cx: &mut Context<'_, Self>,
    ) -> CoverShown {
        let key: Arc<str> = match cover {
            Cover::Color(c) => {
                return hex_color(c).map_or(CoverShown::Pending, CoverShown::Color);
            }
            Cover::Path(p) | Cover::Url(p) => p.clone(),
        };
        if let Some(shown) = self.covers.shown.get(&key) {
            return shown.clone();
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let request = DecodeRequest {
            fit: Some((
                (fit.0 * DECODE_SCALE).max(1.0) as u32,
                (fit.1 * DECODE_SCALE).max(1.0) as u32,
            )),
            animate: false,
        };
        let cancel = CancelFlag::new();
        let flag = cancel.clone();
        let job: Box<dyn FnOnce() -> CoverShown + Send> = match cover {
            Cover::Path(p) => {
                let root = self.root();
                let path = self.file_path(p);
                Box::new(move || {
                    let engine = crate::reader::media::media_engine(&root);
                    decoded(engine.load_path(&path, request, &flag))
                })
            }
            Cover::Url(url) => {
                let policy = Policy::of_app(cx);
                let Some(host) = gv_media::remote::host_of(url) else {
                    let shown = CoverShown::Message("The image address is not valid.".into());
                    self.covers.shown.insert(key, shown.clone());
                    return shown;
                };
                let doc = self.approval_doc();
                match policy.decide(&host, approved(cx, &doc, &host)) {
                    Decision::Load { private_ok } => {
                        let fetcher = crate::reader::remote::shared_fetcher(cx);
                        let url = url.to_string();
                        Box::new(move || {
                            let path = match crate::reader::remote::fetched(&url) {
                                Some((path, _)) => path,
                                None => match crate::reader::remote::fetch_image(
                                    &fetcher, &url, private_ok, &policy, &flag,
                                ) {
                                    Ok(path) => path,
                                    Err(e) => return CoverShown::Message(e.card_message().into()),
                                },
                            };
                            let engine = crate::reader::media::remote_engine();
                            decoded(engine.load_path(&path, request, &flag))
                        })
                    }
                    Decision::Ask => {
                        let shown = CoverShown::Waiting {
                            message: RemoteShown::Waiting { host: host.clone() }
                                .card_message("")
                                .into(),
                            host: host.into(),
                        };
                        self.covers.shown.insert(key, shown.clone());
                        return shown;
                    }
                    Decision::Blocked { by_setting } => {
                        let shown = CoverShown::Message(
                            RemoteShown::Blocked { host, by_setting }
                                .card_message("")
                                .into(),
                        );
                        self.covers.shown.insert(key, shown.clone());
                        return shown;
                    }
                }
            }
            Cover::Color(_) => unreachable!("handled above"),
        };
        self.covers.started += 1;
        self.covers.shown.insert(key.clone(), CoverShown::Pending);
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let shown = cx
                .background_executor()
                .spawn_with_priority(gpui::Priority::Low, async move { job() })
                .await;
            this.update(cx, |view, cx| {
                if view.covers.tasks.remove(&task_key).is_some() {
                    view.covers.shown.insert(task_key, shown);
                    cx.notify();
                }
            })
            .ok();
        });
        self.covers.tasks.insert(key, (cancel, task));
        CoverShown::Pending
    }

    /// The document *Load once* approves for: the base file, or the note
    /// embedding the base.
    fn approval_doc(&self) -> PathBuf {
        match (&self.source, &self.this) {
            (Some(super::Source::File(p)), _) => p.clone(),
            (_, Some(this)) => self.file_path(this),
            _ => self.root(),
        }
    }

    /// *Load once* for `host`'s covers: approved for this base for the
    /// session, and fetched now.
    pub(crate) fn load_once(&mut self, host: &str, cx: &mut Context<'_, Self>) {
        let doc = self.approval_doc();
        cx.default_global::<Approvals>()
            .0
            .insert((doc, host.to_owned()));
        self.covers.forget_host(host);
        cx.notify();
    }

    /// A waiting cover's line and its *Load once* button.
    pub(crate) fn waiting_cover(
        &mut self,
        message: SharedString,
        host: SharedString,
        id: (usize, usize),
        colors: &Colors,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = crate::appearance::Appearance::theme(cx);
        let selector = format!("bases-load-once:{}:{}", id.0, id.1);
        let debug = selector.clone();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .min_w_0()
            .text_size(px(text::UI_SMALL.size))
            .text_color(colors.tertiary)
            .child(div().min_w_0().child(message))
            .child(
                div()
                    .id(gpui::ElementId::Name(selector.into()))
                    .debug_selector(move || debug)
                    .a11y_button(format!("Load once from {host}"))
                    .keyboard_control(theme)
                    .flex_none()
                    .px(px(6.0))
                    .rounded(px(radius::CONTROL))
                    .border_1()
                    .border_color(colors.border_strong)
                    .bg(colors.page)
                    .text_color(colors.text)
                    .cursor_pointer()
                    .child("Load once")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        // The card under it does not open its file.
                        cx.stop_propagation();
                        this.load_once(&host, cx);
                    }))
                    .accessible(),
            )
            .into_any_element()
    }

    /// The card's width, cover height and height for `result`.
    fn card_geometry(result: &ViewResult) -> (f32, f32, f32) {
        let layout = result.cards.as_ref();
        #[allow(clippy::cast_possible_truncation)]
        let width = layout
            .and_then(|c| c.card_size)
            .map_or(DEFAULT_CARD, |w| w as f32)
            .clamp(MIN_CARD, MAX_CARD);
        #[allow(clippy::cast_possible_truncation)]
        let cover = layout
            .filter(|c| c.image.is_some())
            .map_or(0.0, |c| (c.aspect_ratio as f32).clamp(0.1, 4.0) * width);
        let props = result.columns.len().max(1);
        #[allow(clippy::cast_precision_loss)]
        let height = cover + 2.0 * PAD + TITLE + (props - 1) as f32 * LINE;
        (width, cover, height)
    }

    pub(crate) fn render_cards(
        &mut self,
        result: &Arc<ViewResult>,
        _colors: &Colors,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let (card_w, _, _) = Self::card_geometry(result);
        let width = self.width.get();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let columns = if width > 0.0 {
            (((width - GAP) / (card_w + GAP)).floor() as usize).max(1)
        } else {
            1
        };
        if columns != self.card_columns {
            self.card_columns = columns;
            self.card_lines = Rc::new(card_lines(result, columns));
            self.cards_list.reset(self.card_lines.len());
        }
        let measured = self.width.clone();
        let entity = cx.entity_id();
        div()
            .id("bases-cards")
            .a11y_group(Role::List, self.view_label())
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let w = f32::from(bounds.size.width);
                        if (measured.get() - w).abs() > 0.5 {
                            measured.set(w);
                            cx.notify(entity);
                        }
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(
                list(
                    self.cards_list.clone(),
                    cx.processor(|this, ix: usize, _, cx| this.card_line(ix, cx)),
                )
                .size_full(),
            )
            .accessible()
            .into_any_element()
    }

    fn card_line(&mut self, ix: usize, cx: &mut Context<'_, Self>) -> AnyElement {
        let Some(result) = self.result.clone() else {
            return div().into_any_element();
        };
        let colors = Colors::of(crate::appearance::Appearance::theme(cx));
        let Some(line) = self.card_lines.get(ix).copied() else {
            return div().into_any_element();
        };
        match line {
            CardLine::Group(g) => {
                let group = &result.groups[g];
                let n = group.rows.len();
                let name = format!(
                    "Group {}, {n} {}",
                    group_label(&group.key),
                    if n == 1 { "file" } else { "files" }
                );
                div()
                    .id(("bases-card-group", ix))
                    .role(Role::Heading)
                    .aria_label(name)
                    .debug_selector(move || format!("bases-group:{g}"))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(GAP))
                    .pt(px(GAP))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(group_label(&group.key))
                    .child(
                        div()
                            .text_size(px(text::UI_SMALL.size))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(colors.tertiary)
                            .child(group.rows.len().to_string()),
                    )
                    .accessible()
                    .into_any_element()
            }
            CardLine::Cards(g, start, end) => {
                let mut el = div().flex().gap(px(GAP)).px(px(GAP)).pt(px(GAP));
                let before: usize = result.groups[..g].iter().map(|g| g.rows.len()).sum();
                for r in start..end {
                    let row = &result.groups[g].rows[r];
                    el = el.child(self.card(&result, row, (ix, r), before + r, &colors, cx));
                }
                el.into_any_element()
            }
        }
    }

    fn card(
        &mut self,
        result: &ViewResult,
        row: &Row,
        place: (usize, usize),
        ordinal: usize,
        colors: &Colors,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        self.rows_drawn.set(self.rows_drawn.get() + 1);
        let (width, cover_h, height) = Self::card_geometry(result);
        let fit = result.cards.as_ref().map_or(ImageFit::Cover, |c| c.fit);
        let path = row.path.clone();
        let middle = path.clone();
        let (line, r) = place;
        let texts: Vec<String> = row.cells.iter().map(super::cells::cell_text).collect();
        let title = texts
            .first()
            .filter(|t| !t.is_empty())
            .cloned()
            .unwrap_or_else(|| row.path.rsplit('/').next().unwrap_or(&row.path).to_owned());
        let description: Vec<String> = result
            .columns
            .iter()
            .zip(&texts)
            .skip(1)
            .filter(|(_, t)| !t.is_empty())
            .map(|(c, t)| format!("{}: {t}", c.label))
            .collect();
        let theme = crate::appearance::Appearance::theme(cx);
        let mut card = div()
            .id(gpui::ElementId::Name(
                format!("bases-card:{line}:{r}").into(),
            ))
            .debug_selector(move || format!("bases-card:{r}"))
            .a11y_list_item(title, ordinal, result.shown)
            .aria_description(description.join("; "))
            .keyboard_control(theme)
            .flex_none()
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(radius::BLOCK))
            .border_1()
            .border_color(colors.border_strong)
            .bg(colors.page)
            .cursor_pointer()
            .hover({
                let hover = colors.accent_tint;
                move |s| s.border_color(hover)
            })
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                let d = if e.modifiers().secondary(cx) {
                    Disposition::NewTab
                } else {
                    Disposition::InPlace
                };
                this.open_file(&path, d, cx);
            }))
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _, _, cx| {
                    this.open_file(&middle, Disposition::Background, cx);
                }),
            );
        if cover_h > 0.0 {
            let shown = row
                .cover
                .as_ref()
                .and_then(|c| c.as_ref().ok())
                .and_then(Cover::of)
                .map(|c| self.cover_shown(&c, (width, cover_h), cx));
            let mut frame = div()
                .flex_none()
                .w_full()
                .h(px(cover_h))
                .overflow_hidden()
                .bg(colors.chip)
                .flex()
                .items_center()
                .justify_center();
            frame = match shown {
                Some(CoverShown::Image(image)) => frame.child(
                    img(gpui::ImageSource::Render(image))
                        .size_full()
                        .object_fit(match fit {
                            ImageFit::Cover => ObjectFit::Cover,
                            ImageFit::Contain => ObjectFit::Contain,
                        }),
                ),
                Some(CoverShown::Color(c)) => frame.bg(c),
                Some(CoverShown::Message(m)) => frame.p(px(PAD)).child(
                    div()
                        .text_size(px(text::UI_SMALL.size))
                        .text_color(colors.tertiary)
                        .child(m),
                ),
                Some(CoverShown::Waiting { message, host }) => frame
                    .p(px(PAD))
                    .child(self.waiting_cover(message, host, (line, r), colors, cx)),
                Some(CoverShown::Pending) | None => frame,
            };
            card = card.child(frame);
        }
        let mut body = div().flex().flex_col().p(px(PAD)).min_w_0();
        for (c, col) in result.columns.iter().enumerate() {
            let value = row.cells.get(c).cloned().unwrap_or(Ok(Value::Null));
            let first = c == 0;
            let cell = self.cell(
                &value,
                Some(&row.path),
                first || super::table::opens_row(&col.property),
                (place.0, place.1 * 1024 + c),
                colors,
                cx,
            );
            let mut line = div()
                .h(px(if first { TITLE } else { LINE }))
                .flex()
                .items_center()
                .overflow_hidden()
                .min_w_0();
            line = if first {
                line.font_weight(FontWeight::SEMIBOLD)
            } else {
                line.text_size(px(text::UI_SMALL.size))
                    .text_color(colors.secondary)
            };
            body = body.child(line.child(cell));
        }
        card.child(body).accessible().into_any_element()
    }
}

use std::rc::Rc;

/// A decode's result as a cover.
fn decoded(result: gv_media::LoadResult) -> CoverShown {
    match result {
        Ok(d) => crate::reader::media::render_image(&d).map_or_else(
            || CoverShown::Message("The image has no frames.".into()),
            CoverShown::Image,
        ),
        Err(e) => CoverShown::Message(format!("Cannot show the image: {e}").into()),
    }
}
