// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Cells: one value (or the error in its place) as an element.
//!
//! - Text, numbers, dates and durations as the base displays them
//!   ([`Value::display`]), on one line, cut with an ellipsis.
//! - Links, files and URLs in the accent colour, clickable: a click opens
//!   (Mod-click a new tab, a middle click a background tab). An
//!   unresolved link is underlined in the secondary colour.
//! - Lists as chips (at most [`MAX_CHIPS`], then "+N").
//! - Booleans as a check box.
//! - `html()` as its text only: tags are removed, the contents of
//!   `script`, `style` and similar elements dropped, entities decoded
//!   ([`html_text`]). No markup is ever rendered.
//! - `image()` as a thumbnail (through the covers' loader), `icon()` as
//!   the icon of that name from the app's icon set (Lucide names, with or
//!   without a `lucide-` prefix, [`icon_path`]); a name the set does not
//!   have shows as text.
//! - An error as "Error: ..." in the error colour, the whole message in a
//!   tooltip.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, ElementId, InteractiveElement as _,
    IntoElement, MouseButton, ObjectFit, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img, px,
};
use gv_bases::{CellValue, Cover, Value};
use gv_model::LinkKind;

use super::cards::CoverShown;
use super::{BasesView, Colors};
use crate::metrics::{radius, text};
use crate::reader::Disposition;

/// Chips shown of a list before "+N".
pub(crate) const MAX_CHIPS: usize = 12;

/// Height of a thumbnail in a line.
const THUMB: f32 = 20.0;

/// Size of an `icon()` in a line.
const ICON: f32 = 14.0;

/// The asset path of `icon()` value `name` in the app's icon set: a Lucide
/// name (`arrow-up`, `lucide-arrow-up`, any case); `None` when the set
/// does not have it.
pub(crate) fn icon_path(name: &str) -> Option<SharedString> {
    static SERVED: OnceLock<HashSet<SharedString>> = OnceLock::new();
    let served = SERVED.get_or_init(|| {
        use gpui::AssetSource as _;
        crate::icons::Assets
            .list("icons/")
            .unwrap_or_default()
            .into_iter()
            .collect()
    });
    let name = name.trim().to_ascii_lowercase();
    let name = name.strip_prefix("lucide-").unwrap_or(&name);
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return None;
    }
    let path = SharedString::from(format!("icons/{name}.svg"));
    served.contains(&path).then_some(path)
}

/// A tooltip (inverted colours, as the window's tooltips).
pub(crate) struct Tooltip {
    text: SharedString,
    colors: Colors,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        div()
            .max_w(px(420.0))
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(radius::CONTROL))
            .bg(self.colors.tooltip_bg)
            .text_color(self.colors.tooltip_fg)
            .text_size(px(text::UI_SMALL.size))
            .child(self.text.clone())
    }
}

/// A builder of tooltips showing `text`.
pub(crate) fn tooltip(
    text: SharedString,
    colors: Colors,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    move |_, cx| {
        let (text, colors) = (text.clone(), colors);
        cx.new(|_| Tooltip { text, colors }).into()
    }
}

/// How a click opens: Mod for a new tab.
fn disposition(e: &ClickEvent, cx: &App) -> Disposition {
    if e.modifiers().secondary(cx) {
        Disposition::NewTab
    } else {
        Disposition::InPlace
    }
}

/// What a click on a value does.
#[derive(Debug, Clone)]
enum Target {
    File(Arc<str>),
    Link(Arc<str>),
    Url(Arc<str>),
}

/// Where a cell is, for element ids: (line, column).
pub(crate) type Place = (usize, usize);

impl BasesView {
    /// The element of a cell. `row_path` is the row's file: the file
    /// name columns open it. `place` makes the ids unique.
    pub(crate) fn cell(
        &mut self,
        value: &CellValue,
        row_path: Option<&Arc<str>>,
        opens_row: bool,
        place: Place,
        colors: &Colors,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        match value {
            Ok(v) => {
                if opens_row && let (Some(path), Value::String(_)) = (row_path, v) {
                    let label: SharedString = v.display().into();
                    return self.clickable(
                        label,
                        Target::File(path.clone()),
                        false,
                        (place, 0),
                        colors,
                        cx,
                    );
                }
                self.value(v, place, 0, colors, cx)
            }
            Err(e) => {
                let full: SharedString = e.to_string().into();
                div()
                    .id(ElementId::from(("bases-error", place.0 * 4096 + place.1)))
                    .debug_selector(|| "bases-error-cell".into())
                    .min_w_0()
                    .truncate()
                    .text_color(colors.error)
                    .child(format!("Error: {full}"))
                    .tooltip(tooltip(full, *colors))
                    .into_any_element()
            }
        }
    }

    fn value(
        &mut self,
        v: &Value,
        place: Place,
        item: usize,
        colors: &Colors,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        match v {
            Value::Null => div().into_any_element(),
            Value::Bool(b) => checkbox(*b, colors),
            Value::List(items) => {
                let mut row = div().flex().items_center().gap(px(4.0)).min_w_0();
                for (i, item) in items.iter().take(MAX_CHIPS).enumerate() {
                    let inner = self.value(item, place, i + 1, colors, cx);
                    row = row.child(
                        div()
                            .flex_none()
                            .max_w(px(220.0))
                            .px(px(6.0))
                            .h(px(18.0))
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .rounded(px(radius::CHIP))
                            .bg(colors.chip)
                            .text_size(px(text::UI_SMALL.size))
                            .child(inner),
                    );
                }
                if items.len() > MAX_CHIPS {
                    row = row.child(
                        div()
                            .flex_none()
                            .text_size(px(text::UI_SMALL.size))
                            .text_color(colors.tertiary)
                            .child(format!("+{}", items.len() - MAX_CHIPS)),
                    );
                }
                row.into_any_element()
            }
            Value::Link(l) => {
                let label: SharedString = l.label().to_owned().into();
                self.clickable(
                    label,
                    Target::Link(l.text.clone()),
                    l.path.is_none(),
                    (place, item),
                    colors,
                    cx,
                )
            }
            Value::File(path) => {
                let name = path.rsplit('/').next().unwrap_or(path).to_owned();
                self.clickable(
                    name.into(),
                    Target::File(path.clone()),
                    false,
                    (place, item),
                    colors,
                    cx,
                )
            }
            Value::Url(u) => {
                let label: SharedString = v.display().into();
                self.clickable(
                    label,
                    Target::Url(u.url.clone()),
                    false,
                    (place, item),
                    colors,
                    cx,
                )
            }
            Value::Html(html) => plain(html_text(html), colors.text),
            Value::Image(_) => match Cover::of(v) {
                Some(cover) => self.thumbnail(&cover, (place, item), colors, cx),
                None => plain(v.display(), colors.text),
            },
            Value::Icon(name) => match icon_path(name) {
                Some(path) => {
                    let selector = format!("bases-icon:{name}");
                    div()
                        .flex_none()
                        .debug_selector(move || selector)
                        .text_color(colors.secondary)
                        .child(gpui::component::Icon::default().path(path).size(px(ICON)))
                        .into_any_element()
                }
                None => plain(name.to_string(), colors.secondary),
            },
            _ => plain(v.display(), colors.text),
        }
    }

    fn clickable(
        &mut self,
        label: SharedString,
        target: Target,
        unresolved: bool,
        id: (Place, usize),
        colors: &Colors,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let ((line, col), item) = id;
        let key = format!("bases-link:{line}:{col}:{item}");
        let selector = key.clone();
        let middle = target.clone();
        let mut el = div()
            .id(ElementId::Name(key.into()))
            .debug_selector(move || selector)
            .min_w_0()
            .truncate()
            .cursor_pointer()
            .text_color(if unresolved {
                colors.secondary
            } else {
                colors.accent
            })
            .child(label)
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                // The card around it does not open its file too.
                cx.stop_propagation();
                let d = disposition(e, cx);
                this.activate(&target, d, cx);
            }))
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.activate(&middle, Disposition::Background, cx);
                }),
            );
        if unresolved {
            el = el.underline();
        }
        el.into_any_element()
    }

    fn activate(&mut self, target: &Target, d: Disposition, cx: &mut Context<'_, Self>) {
        match target {
            Target::File(path) => self.open_file(path, d, cx),
            Target::Link(text) => self.follow(text.clone(), LinkKind::Wikilink, d, cx),
            Target::Url(url) => self.follow(url.clone(), LinkKind::UriAutolink, d, cx),
        }
    }

    /// A small image in a line.
    fn thumbnail(
        &mut self,
        cover: &Cover,
        id: (Place, usize),
        colors: &Colors,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let fit = (THUMB * 4.0, THUMB * 2.0);
        match self.cover_shown(cover, fit, cx) {
            CoverShown::Image(image) => img(gpui::ImageSource::Render(image))
                .h(px(THUMB))
                .max_w(px(THUMB * 4.0))
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            CoverShown::Color(c) => div()
                .size(px(THUMB * 0.7))
                .rounded(px(radius::CHIP))
                .bg(c)
                .into_any_element(),
            CoverShown::Pending => div().size(px(THUMB)).into_any_element(),
            CoverShown::Message(m) => plain(m.to_string(), colors.tertiary),
            CoverShown::Waiting { message, host } => {
                let ((line, col), item) = id;
                let at = (line, (1 << 30) | (col << 10) | item);
                self.waiting_cover(message, host, at, colors, cx)
            }
        }
    }
}

/// What assistive technology reads for a cell: the text the cell shows
/// (a check box as "checked" or "not checked", a list's items joined, an
/// image as "image", `html()` as its text), or the error.
pub(crate) fn cell_text(value: &CellValue) -> String {
    match value {
        Ok(v) => value_text(v),
        Err(e) => format!("Error: {e}"),
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(true) => "checked".to_owned(),
        Value::Bool(false) => "not checked".to_owned(),
        Value::List(items) => items
            .iter()
            .map(value_text)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Value::Link(l) => l.label().to_owned(),
        Value::File(path) => path.rsplit('/').next().unwrap_or(path).to_owned(),
        Value::Html(html) => html_text(html),
        Value::Image(_) => "image".to_owned(),
        Value::Icon(name) => format!("{name} icon"),
        _ => v.display(),
    }
}

fn plain(text: String, color: gpui::Hsla) -> AnyElement {
    div()
        .min_w_0()
        .truncate()
        .text_color(color)
        .child(text)
        .into_any_element()
}

fn checkbox(on: bool, colors: &Colors) -> AnyElement {
    let mut b = div()
        .flex_none()
        .size(px(crate::metrics::size::CHECKBOX))
        .rounded(px(3.0))
        .border_1()
        .flex()
        .items_center()
        .justify_center();
    b = if on {
        b.bg(colors.accent)
            .border_color(colors.accent)
            .child(div().size(px(6.0)).rounded(px(1.0)).bg(colors.page))
    } else {
        b.border_color(colors.border_strong)
    };
    b.into_any_element()
}

/// Elements whose content is never text.
const DROPPED: &[&str] = &[
    "script", "style", "template", "iframe", "object", "embed", "noscript", "svg", "math", "head",
    "title", "textarea", "select",
];

/// Tags after which a space separates the text.
const BREAKS: &[&str] = &[
    "br",
    "p",
    "div",
    "li",
    "tr",
    "td",
    "th",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "pre",
    "hr",
    "dt",
    "dd",
];

/// The text of an HTML fragment: tags removed, the contents of
/// [`DROPPED`] elements left out, entities decoded, white space collapsed.
/// What `html()` cells show: markup is never rendered.
pub(crate) fn html_text(html: &str) -> String {
    let mut out = String::new();
    let mut skip: Option<String> = None;
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        if skip.is_none() {
            out.push_str(&decode_entities(&rest[..lt]));
        }
        rest = &rest[lt..];
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
            continue;
        }
        let Some(gt) = rest.find('>') else {
            // An unfinished tag: drop the rest.
            rest = "";
            break;
        };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let end = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if let Some(open) = &skip {
            if end && *open == name {
                skip = None;
            }
            continue;
        }
        if !end && DROPPED.contains(&name.as_str()) && !tag.ends_with('/') {
            skip = Some(name);
            continue;
        }
        if BREAKS.contains(&name.as_str()) {
            out.push(' ');
        }
    }
    if skip.is_none() {
        out.push_str(&decode_entities(rest));
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decodes the common named entities and numeric references.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&e| e <= 10).and_then(|e| {
            let name = &rest[1..e];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => name.strip_prefix('#').and_then(|n| {
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => n.parse().ok(),
                    };
                    code.and_then(char::from_u32)
                }),
            };
            ch.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}
