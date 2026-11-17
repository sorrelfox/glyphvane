// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Hover previews of links (FR-VIEW-13; 09 § Hover preview; SPEC §
//! Popovers, Hover preview).
//!
//! - **Triggers:** the pointer resting on a link in the Reading view for
//!   `preview.hover_preview_delay_ms` (300 ms by default); in the Source
//!   view, resting on it with Mod held, or pressing Mod while it rests
//!   there; `links.preview` (*Preview link under caret*) on the link the
//!   link menu was opened on or the one under the Source view's caret,
//!   which gives the preview the keyboard.
//! - **Contents by target:** a note in a Reading view of its own (the
//!   Split view's smaller type), scrolled to the heading or block the link
//!   names; an image; another file (a PDF among them) says it opens in the
//!   default app; a web address shows the address and its domain, never
//!   fetched; an unresolved link says so and offers *Create note*; an
//!   ambiguous link lists its candidates; a pending link says the vault
//!   is still being indexed.
//! - **The popover** (320 wide): a head 28 high with the file icon, the
//!   title in weight 600 and the folder at the right; the body; a footer
//!   26 high with key hints. The body of a note is
//!   [`spacing::BODY_HEIGHT`] high and scrolls (not designed).
//! - **Keys:** Enter opens the target, Mod+Enter in a new tab, Esc closes
//!   (Esc works without the keyboard in the preview too). A click on the
//!   head opens the target; links clicked inside the preview are followed
//!   from the previewed note.
//! - **Dismissal:** a preview opened by the pointer closes
//!   [`CLOSE_DELAY`] after the pointer left both its link and the popover;
//!   one opened by the keyboard stays until Esc or a click outside. A
//!   click outside every preview closes them all, and so does another tab
//!   becoming active.
//! - **Nesting:** a link in a preview gets its own preview, two levels
//!   deep at most.
//!
//! Not built: pinning a preview as a floating window (Should), the page
//! of a PDF, cutting a heading's section at its end (the preview scrolls
//! on into the rest of the note).

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, DispatchPhase, Div, Entity,
    EntityInputHandler as _, FocusHandle, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent, ParentElement as _,
    Pixels, Point, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity, Window, anchored, canvas, deferred, div, img, point, px,
};

use super::Workspace;
use crate::appearance::Appearance;
use crate::editor::EditorView;
use crate::icons::Icon;
use crate::keymap::actions::links as commands;
use crate::menu::{panel, row};
use crate::metrics::{HAIRLINE, font, icon, size, text};
use crate::reader::link_info::LinkAt;
use crate::reader::{Disposition, Navigation, NewNote, Reader, ReaderEvent, ScrollTarget};
use crate::settings::AppSettings;
use crate::theme::Theme;

/// How long a preview stays after the pointer left it and its link.
pub(super) const CLOSE_DELAY: Duration = Duration::from_millis(300);

/// How soon after the pointer moved the previews look where it is: the
/// views under it have drawn by then and know which link it is on.
const SETTLE_DELAY: Duration = Duration::from_millis(20);

/// Previews open inside previews this deep at most.
pub(super) const MAX_DEPTH: usize = 2;

/// The popover's measures: designed (SPEC § Popovers) unless noted.
mod spacing {
    pub const HEAD_HEIGHT: f32 = 28.0;
    pub const HEAD_PAD_X: f32 = 10.0;
    pub const HEAD_GAP: f32 = 6.0;
    pub const BODY_PAD_TOP: f32 = 10.0;
    pub const BODY_PAD_X: f32 = 12.0;
    pub const BODY_PAD_BOTTOM: f32 = 12.0;
    pub const FOOTER_HEIGHT: f32 = 26.0;
    pub const FOOTER_PAD_X: f32 = 12.0;
    pub const FOOTER_GAP: f32 = 12.0;
    /// Body text: 13/20.
    pub const BODY_SIZE: f32 = 13.0;
    pub const BODY_LINE: f32 = 20.0;
    /// Body title: 16/22, weight 700.
    pub const TITLE_SIZE: f32 = 16.0;
    pub const TITLE_LINE: f32 = 22.0;
    /// Not designed: the height of a note's body, the largest image, the
    /// gap under the pointer, the gap between body paragraphs.
    pub const BODY_HEIGHT: f32 = 240.0;
    pub const IMAGE_MAX_HEIGHT: f32 = 200.0;
    pub const OFFSET_Y: f32 = 14.0;
    pub const PARAGRAPH_GAP: f32 = 6.0;
}

/// The view a previewed link is in.
#[derive(Clone)]
enum Source {
    Reader(WeakEntity<Reader>),
    Editor(WeakEntity<EditorView>),
    /// `links.preview`: the link menu's link or the caret's.
    Command,
}

/// What a preview shows under its head.
enum Body {
    /// A note, in a Reading view of its own.
    Note(Entity<Reader>),
    Image(PathBuf),
    /// Lines of text: a file that opens elsewhere, a web address, why the
    /// link goes nowhere.
    Text(Vec<SharedString>),
    /// No such note: what following it would create.
    Unresolved {
        text: SharedString,
        create: Option<NewNote>,
    },
    /// The notes an ambiguous link matches.
    Candidates(Vec<(SharedString, PathBuf)>),
}

/// One open preview.
pub(super) struct Preview {
    link: LinkAt,
    at: Point<Pixels>,
    source: Source,
    /// The tab the link's document is in.
    tab: usize,
    icon: Icon,
    title: SharedString,
    folder: SharedString,
    body: Body,
    /// Opened by the keyboard: stays until Esc or a click outside.
    keyboard: bool,
    focus: FocusHandle,
    /// Where it was drawn, in window coordinates.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _subscriptions: Vec<Subscription>,
}

impl Preview {
    fn reader(&self) -> Option<&Entity<Reader>> {
        match &self.body {
            Body::Note(reader) => Some(reader),
            _ => None,
        }
    }
}

/// The window's open previews, the outermost first.
#[derive(Default)]
pub(super) struct HoverPreviews {
    stack: Vec<Preview>,
    /// Where the keyboard was before a preview took it.
    restore: Option<FocusHandle>,
    _settle: Option<Task<()>>,
    _close: Option<Task<()>>,
}

/// The delay before a Reading view link's preview opens.
pub(super) fn hover_delay(cx: &App) -> Duration {
    let ms = cx
        .try_global::<AppSettings>()
        .map_or(300, |s| s.settings().preview.hover_preview_delay_ms);
    Duration::from_millis(u64::from(ms))
}

/// Whether `path` is an image the preview draws.
fn is_image(path: &Path) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif" | "ico"
    )
}

/// The host of a web address (`https://example.com/a` → `example.com`).
fn domain_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    (!host.is_empty()).then(|| host.to_owned())
}

impl Workspace {
    // ----- opening -----------------------------------------------------------------

    /// A Reading view notified: when the pointer has rested on one of its
    /// links for the delay, that link's preview opens. Views of the active
    /// tab open the outer preview, a preview's view the one inside it.
    pub(super) fn check_reader_preview(
        &mut self,
        reader: &Entity<Reader>,
        cx: &mut Context<'_, Self>,
    ) {
        if reader.read(cx).path().is_none() {
            return;
        }
        let Some((link, at)) = reader.update(cx, |r, _| r.take_hover_preview()) else {
            return;
        };
        let tab = self.active_tab();
        let outer =
            tab.reader.as_ref() == Some(reader) || tab.split_reader(cx).as_ref() == Some(reader);
        let level = if outer {
            0
        } else {
            match self
                .previews
                .stack
                .iter()
                .position(|p| p.reader() == Some(reader))
            {
                Some(i) => i + 1,
                None => return,
            }
        };
        let source = Source::Reader(reader.downgrade());
        self.hover_preview(level, link, at, source, cx);
    }

    /// A Source view notified: a link the pointer rests on with Mod held
    /// gets its preview.
    pub(super) fn check_editor_preview(
        &mut self,
        editor: &Entity<EditorView>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.shown_editor() != Some(editor) {
            return;
        }
        let Some((link, at)) = editor.update(cx, |e, _| e.take_hover_preview()) else {
            return;
        };
        self.hover_preview(0, link, at, Source::Editor(editor.downgrade()), cx);
    }

    /// Mod pressed while the pointer rests on a link of the Source view:
    /// its preview, at once.
    fn preview_on_modifiers(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !event.modifiers.secondary(cx) || !self.previews.stack.is_empty() {
            return;
        }
        let Some(editor) = self.shown_editor().cloned() else {
            return;
        };
        let Some(link) = editor.read(cx).hovered_link() else {
            return;
        };
        let at = window.mouse_position();
        self.hover_preview(0, link, at, Source::Editor(editor.downgrade()), cx);
    }

    /// Opens the preview of `link` at `level` (closing what is above),
    /// unless a menu or link popover is open.
    fn hover_preview(
        &mut self,
        level: usize,
        link: LinkAt,
        at: Point<Pixels>,
        source: Source,
        cx: &mut Context<'_, Self>,
    ) {
        if level >= MAX_DEPTH || level > self.previews.stack.len() {
            return;
        }
        if self.menus.popover.read(cx).is_open() || self.link_ux.popover.is_some() {
            return;
        }
        self.open_preview(level, link, at, source, false, cx);
    }

    /// `links.preview`: the preview of the link menu's link or the
    /// caret's, below the caret, with the keyboard.
    fn preview_command(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(link) = self.command_link(window, cx) else {
            return;
        };
        let from_menu = crate::menu::context_target(cx) == crate::menu::ContextTarget::Link;
        let caret = (!from_menu)
            .then(|| self.shown_editor().cloned())
            .flatten()
            .and_then(|editor| {
                editor.update(cx, |e, cx| {
                    let range = e.selected_text_range(false, window, cx)?.range;
                    let start = range.start..range.start;
                    e.bounds_for_range(start, Bounds::default(), window, cx)
                })
            });
        let at = caret.map_or_else(|| window.mouse_position(), |b| b.bottom_left());
        let restore = window.focused(cx);
        self.open_preview(0, link, at, Source::Command, true, cx);
        if let Some(preview) = self.previews.stack.first() {
            self.previews.restore = restore;
            preview.focus.focus(window, cx);
        }
    }

    /// Opens the preview of `link` at `level`, replacing that level and
    /// those above it.
    fn open_preview(
        &mut self,
        level: usize,
        link: LinkAt,
        at: Point<Pixels>,
        source: Source,
        keyboard: bool,
        cx: &mut Context<'_, Self>,
    ) {
        self.previews.stack.truncate(level);
        self.previews._close = None;
        let tab = match &source {
            // A preview's links act in the tab of the outer preview.
            Source::Reader(_) if level > 0 => self.previews.stack[0].tab,
            _ => self.link_tab_of(&link.file),
        };
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let nav = crate::reader::navigation(
            &link.file,
            snapshot.as_ref(),
            link.profile,
            link.kind,
            &link.destination,
        );
        let root = self.link_vault_root(cx);
        let named = |path: &Path| {
            let title = path
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            let folder = path
                .parent()
                .filter(|p| root.as_deref() != Some(*p))
                .and_then(Path::file_name)
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            (SharedString::from(title), SharedString::from(folder))
        };
        let mut subscriptions = Vec::new();
        let (icon, (title, folder), body) = match nav {
            Navigation::Open { path, fragment } => {
                let scroll = fragment.map(ScrollTarget::Fragment);
                let reader = self.preview_reader(level, &path, scroll, &mut subscriptions, cx);
                (Icon::Note, named(&path), Body::Note(reader))
            }
            Navigation::Here { fragment } => {
                let path = link.file.clone();
                let scroll = fragment.map(ScrollTarget::Fragment);
                let reader = self.preview_reader(level, &path, scroll, &mut subscriptions, cx);
                (Icon::Note, named(&path), Body::Note(reader))
            }
            Navigation::OpenFile(path) if is_image(&path) => {
                (Icon::ImageFile, named(&path), Body::Image(path))
            }
            Navigation::OpenFile(path) => {
                let name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let (icon, what) = if super::canvas_tabs::is_canvas(&path) {
                    (Icon::Canvas, "A canvas. Enter opens it in a tab.")
                } else if super::bases_tabs::is_base(&path) {
                    (Icon::Base, "A base. Enter opens it in a tab.")
                } else {
                    (Icon::OtherFile, "Opens in the default app.")
                };
                (
                    icon,
                    named(&path),
                    Body::Text(vec![name.into(), what.into()]),
                )
            }
            Navigation::External(url) => {
                let domain = domain_of(&url).unwrap_or_else(|| url.clone());
                (
                    Icon::OtherFile,
                    (domain.clone().into(), SharedString::default()),
                    Body::Text(vec![url.into(), "Opens in your browser.".into()]),
                )
            }
            Navigation::Confirm { url, why } => {
                let domain = domain_of(&url).unwrap_or_else(|| url.clone());
                (
                    Icon::OtherFile,
                    (domain.into(), SharedString::default()),
                    Body::Text(vec![url.into(), why.into()]),
                )
            }
            Navigation::Ambiguous {
                candidates, path, ..
            } => {
                let rows = candidates
                    .iter()
                    .map(|c| {
                        let label = match &root {
                            Some(root) => c.strip_prefix(root).unwrap_or(c).display().to_string(),
                            None => c.display().to_string(),
                        };
                        (SharedString::from(label), c.clone())
                    })
                    .collect::<Vec<_>>();
                let name = crate::reader::link_info::link_name(link.kind, &link.destination);
                let _ = path;
                (
                    Icon::Note,
                    (
                        format!("{} notes match \u{201c}{name}\u{201d}", rows.len()).into(),
                        SharedString::default(),
                    ),
                    Body::Candidates(rows),
                )
            }
            Navigation::Unresolved { name, create } => {
                let text = match &create {
                    Some(_) => format!("No note \u{201c}{name}\u{201d} exists yet."),
                    None => format!("No note \u{201c}{name}\u{201d} exists."),
                };
                (
                    Icon::NotCreated,
                    (name.into(), SharedString::default()),
                    Body::Unresolved {
                        text: text.into(),
                        create,
                    },
                )
            }
            Navigation::Pending => (
                Icon::Note,
                (
                    crate::reader::link_info::link_name(link.kind, &link.destination).into(),
                    SharedString::default(),
                ),
                Body::Text(vec![
                    "The vault is still being indexed: where this link goes is not known yet."
                        .into(),
                ]),
            ),
            Navigation::Refused(why) => (
                Icon::OtherFile,
                (
                    crate::reader::link_info::link_name(link.kind, &link.destination).into(),
                    SharedString::default(),
                ),
                Body::Text(vec![why.into()]),
            ),
        };
        self.previews.stack.push(Preview {
            link,
            at,
            source,
            tab,
            icon,
            title,
            folder,
            body,
            keyboard,
            focus: cx.focus_handle(),
            bounds: Rc::default(),
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// The tab whose document is `file`: the active tab when it shows it.
    fn link_tab_of(&self, file: &Path) -> usize {
        if self.tabs[self.active].path.as_deref() == Some(file) {
            return self.active;
        }
        self.tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(file))
            .unwrap_or(self.active)
    }

    /// The Reading view of a note's preview at `level`.
    fn preview_reader(
        &mut self,
        level: usize,
        path: &Path,
        scroll: Option<ScrollTarget>,
        subscriptions: &mut Vec<Subscription>,
        cx: &mut Context<'_, Self>,
    ) -> Entity<Reader> {
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let content = self.vault.as_ref().map(|v| v.read(cx).content());
        let index = self.link_tab_of(path);
        let profile = self.tab_profile(index, cx).compiled;
        let nested = level + 1 < MAX_DEPTH;
        let delay = hover_delay(cx);
        let path = path.to_path_buf();
        let reader = cx.new(|cx| {
            let mut reader = Reader::for_split(cx);
            reader.fill_view(cx);
            reader.set_index(snapshot, cx);
            reader.set_profile(profile, cx);
            if let Some(content) = content {
                reader.set_content(content, cx);
            }
            reader.set_hover_previews(nested.then_some(delay));
            reader.open(path, scroll, cx);
            reader
        });
        subscriptions.push(
            cx.subscribe(&reader, |this, reader, event: &ReaderEvent, cx| {
                if let ReaderEvent::FollowLink(request) = event {
                    this.follow_preview_link(&reader, request.clone(), cx);
                }
            }),
        );
        if nested {
            subscriptions.push(cx.observe(&reader, |this, reader, cx| {
                this.check_reader_preview(&reader, cx);
            }));
        }
        reader
    }

    // ----- acting ------------------------------------------------------------------

    /// A link clicked inside a preview: followed from the previewed note,
    /// in the tab of the outer preview.
    fn follow_preview_link(
        &mut self,
        reader: &Entity<Reader>,
        request: crate::reader::LinkRequest,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(tab) = self.previews.stack.first().map(|p| p.tab) else {
            return;
        };
        let r = reader.read(cx);
        let Some(file) = r.path().map(Path::to_path_buf) else {
            return;
        };
        let construct = request.range.clone().and_then(|range| r.source_text(range));
        let link = LinkAt {
            file,
            kind: request.kind,
            image: false,
            destination: request.destination,
            range: request.range,
            construct,
            profile: r.profile(),
        };
        let disposition = request.disposition;
        let handle = self.window_handle;
        // The click is handled inside the preview's view: follow once it
        // is done.
        cx.spawn(async move |this, cx| {
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| {
                    ws.close_previews(window, cx);
                    ws.follow_link_at(tab, link, disposition, false, window, cx);
                })
            });
        })
        .detach();
    }

    /// Enter (Mod+Enter: a new tab) or a click on the head of the preview
    /// at `level`: follows its link.
    fn open_previewed(
        &mut self,
        level: usize,
        disposition: Disposition,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(preview) = self.previews.stack.get(level) else {
            return;
        };
        let (tab, link) = (preview.tab, preview.link.clone());
        self.close_previews(window, cx);
        self.follow_link_at(tab, link, disposition, false, window, cx);
    }

    /// *Create note* in an unresolved link's preview.
    fn create_from_preview(
        &mut self,
        level: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(preview) = self.previews.stack.get(level) else {
            return;
        };
        let Body::Unresolved {
            create: Some(note), ..
        } = &preview.body
        else {
            return;
        };
        let (note, link) = (note.clone(), preview.link.clone());
        self.close_previews(window, cx);
        if let Some(rewrite) = &note.rewrite {
            self.rewrite_link_text(&link, rewrite.clone(), window, cx);
        }
        self.create_note_for_link(&note, Disposition::InPlace, cx);
    }

    /// A candidate of an ambiguous link's preview: opened in the link's
    /// tab.
    fn open_candidate(
        &mut self,
        level: usize,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(tab) = self.previews.stack.get(level).map(|p| p.tab) else {
            return;
        };
        self.close_previews(window, cx);
        self.activate_tab(tab, window, cx);
        self.navigate(tab, path, None, window, cx);
    }

    // ----- closing -----------------------------------------------------------------

    /// Closes every preview; the keyboard goes back where it was.
    pub(super) fn close_previews(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.truncate_previews(0, cx);
        if let Some(focus) = self.previews.restore.take() {
            focus.focus(window, cx);
        }
    }

    fn truncate_previews(&mut self, keep: usize, cx: &mut Context<'_, Self>) {
        if keep < self.previews.stack.len() {
            self.previews.stack.truncate(keep);
            cx.notify();
        }
        if keep == 0 {
            self.previews._close = None;
            self.previews._settle = None;
        }
    }

    /// How many previews, from the outermost, the pointer at `position`
    /// keeps: those it is over or whose link it rests on, and those below
    /// them. Previews opened by the keyboard stay.
    fn previews_kept(&self, position: Point<Pixels>, cx: &App) -> usize {
        let mut keep = 0;
        for (i, p) in self.previews.stack.iter().enumerate() {
            let over = p.bounds.get().is_some_and(|b| b.contains(&position));
            let on_link = match &p.source {
                Source::Reader(r) => r
                    .upgrade()
                    .and_then(|r| r.read(cx).hovered_link())
                    .is_some_and(|l| l == p.link),
                Source::Editor(e) => e
                    .upgrade()
                    .and_then(|e| e.read(cx).hovered_link())
                    .is_some_and(|l| l == p.link),
                Source::Command => false,
            };
            if p.keyboard || over || on_link {
                keep = i + 1;
            }
        }
        keep
    }

    /// The pointer moved: once the views under it have drawn, previews it
    /// left start closing.
    fn preview_pointer_moved(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.previews._settle = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SETTLE_DELAY).await;
            let _ = this.update_in(cx, |ws, window, cx| ws.settle_previews(window, cx));
        }));
    }

    /// Previews the pointer left close after [`CLOSE_DELAY`], unless it
    /// comes back.
    fn settle_previews(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.previews._settle = None;
        let position = window.mouse_position();
        if self.previews_kept(position, cx) >= self.previews.stack.len() {
            self.previews._close = None;
            return;
        }
        if self.previews._close.is_some() {
            return;
        }
        self.previews._close = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(CLOSE_DELAY).await;
            let _ = this.update_in(cx, |ws, window, cx| {
                ws.previews._close = None;
                let keep = ws.previews_kept(window.mouse_position(), cx);
                ws.truncate_previews(keep, cx);
            });
        }));
    }

    /// A press outside every preview closes them.
    fn preview_mouse_down(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let inside = self
            .previews
            .stack
            .iter()
            .any(|p| p.bounds.get().is_some_and(|b| b.contains(&position)));
        if !inside {
            self.close_previews(window, cx);
        }
    }

    /// Esc anywhere closes the innermost preview.
    fn preview_escape(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> bool {
        let Some(level) = self.previews.stack.len().checked_sub(1) else {
            return false;
        };
        if level == 0 {
            self.close_previews(window, cx);
        } else {
            self.truncate_previews(level, cx);
            self.previews.stack[level - 1].focus.focus(window, cx);
        }
        true
    }

    /// Drops the previews of a tab that is no longer active.
    pub(super) fn prune_previews(&mut self) {
        if self
            .previews
            .stack
            .first()
            .is_some_and(|p| p.tab != self.active || p.tab >= self.tabs.len())
        {
            self.previews.stack.clear();
            self.previews._close = None;
        }
    }

    /// The Reading view of the note previewed at `level` (tests).
    #[cfg(test)]
    pub(crate) fn preview_reader_for_tests(&self, level: usize) -> Option<Entity<Reader>> {
        self.previews.stack.get(level)?.reader().cloned()
    }

    /// The open previews' links, outermost first (tests).
    #[cfg(test)]
    pub(crate) fn previewed_links(&self) -> Vec<String> {
        self.previews
            .stack
            .iter()
            .map(|p| p.link.destination.to_string())
            .collect()
    }

    // ----- the commands and the window root -----------------------------------------

    /// `links.preview` where there is a link, and the keys and pointer
    /// events of open previews, on the window's root.
    pub(super) fn preview_actions(
        &self,
        root: Stateful<Div>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let has_link = self.command_link(window, cx).is_some();
        let open = !self.previews.stack.is_empty();
        root.when(has_link, |root| {
            root.on_action(cx.listener(|this, _: &commands::Preview, window, cx| {
                this.preview_command(window, cx);
            }))
        })
        .on_modifiers_changed(cx.listener(|this, e: &ModifiersChangedEvent, window, cx| {
            this.preview_on_modifiers(e, window, cx);
        }))
        .when(open, |root| {
            root.capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && this.preview_escape(window, cx) {
                    cx.stop_propagation();
                }
            }))
        })
    }

    /// The open previews, over the window.
    pub(super) fn render_previews(&self, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        if self.previews.stack.is_empty() {
            return Vec::new();
        }
        let theme = Appearance::theme(cx);
        let ws = cx.weak_entity();
        // Pointer moves and presses anywhere in the window.
        let watcher = canvas(
            |_, _, _| (),
            move |_, (), window, _| {
                let moved = ws.clone();
                window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = moved.update(cx, |ws, cx| ws.preview_pointer_moved(window, cx));
                    }
                });
                let pressed = ws.clone();
                window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        let _ = pressed.update(cx, |ws, cx| {
                            ws.preview_mouse_down(e.position, window, cx);
                        });
                    }
                });
            },
        )
        .absolute()
        .size_0()
        .into_any_element();
        let mut out = vec![watcher];
        for (level, preview) in self.previews.stack.iter().enumerate() {
            out.push(self.render_preview(level, preview, theme, cx));
        }
        out
    }

    fn render_preview(
        &self,
        level: usize,
        preview: &Preview,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let head = div()
            .id(("hover-preview-head", level))
            .debug_selector(move || format!("hover-preview-head-{level}"))
            .h(px(spacing::HEAD_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing::HEAD_GAP))
            .px(px(spacing::HEAD_PAD_X))
            .border_b(px(HAIRLINE))
            .border_color(theme.border.default)
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text.secondary)
                    .child(preview.icon.sized(icon::ROW)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight(text::weight::STRONG))
                    .child(preview.title.clone()),
            )
            .when(!preview.folder.is_empty(), |head| {
                head.child(
                    div()
                        .flex_none()
                        .max_w(px(size::HOVER_PREVIEW_WIDTH / 3.0))
                        .truncate()
                        .text_size(px(text::UI_SMALL.size))
                        .text_color(theme.text.secondary)
                        .child(preview.folder.clone()),
                )
            })
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
                let disposition = if e.modifiers().secondary(cx) {
                    Disposition::NewTab
                } else {
                    Disposition::InPlace
                };
                this.open_previewed(level, disposition, window, cx);
            }));
        let body = self.render_preview_body(level, preview, theme, cx);
        let bounds = preview.bounds.clone();
        let popover = panel(theme)
            .p_0()
            .w(px(size::HOVER_PREVIEW_WIDTH))
            .overflow_hidden()
            .child(head)
            .child(body)
            .child(footer(theme))
            .child(
                canvas(move |b, _, _| bounds.set(Some(b)), |_, (), _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            );
        let at = preview.at + point(px(0.0), px(spacing::OFFSET_Y));
        deferred(
            anchored().position(at).snap_to_window().child(
                div()
                    .id(("hover-preview", level))
                    .debug_selector(move || format!("hover-preview-{level}"))
                    .track_focus(&preview.focus)
                    .occlude()
                    .capture_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                        if e.keystroke.key == "enter" {
                            let disposition = if e.keystroke.modifiers.secondary(cx) {
                                Disposition::NewTab
                            } else {
                                Disposition::InPlace
                            };
                            cx.stop_propagation();
                            this.open_previewed(level, disposition, window, cx);
                        }
                    }))
                    .child(popover),
            ),
        )
        .with_priority(2 + level)
        .into_any_element()
    }

    fn render_preview_body(
        &self,
        level: usize,
        preview: &Preview,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let padded = || {
            div()
                .flex()
                .flex_col()
                .gap(px(spacing::PARAGRAPH_GAP))
                .pt(px(spacing::BODY_PAD_TOP))
                .px(px(spacing::BODY_PAD_X))
                .pb(px(spacing::BODY_PAD_BOTTOM))
                .text_size(px(spacing::BODY_SIZE))
                .line_height(px(spacing::BODY_LINE))
        };
        match &preview.body {
            Body::Note(reader) => div()
                .debug_selector(move || format!("hover-preview-note-{level}"))
                .h(px(spacing::BODY_HEIGHT))
                .w_full()
                .flex()
                .child(reader.clone())
                .into_any_element(),
            Body::Image(path) => padded()
                .debug_selector(move || format!("hover-preview-image-{level}"))
                .items_center()
                .child(
                    img(path.clone())
                        .max_w_full()
                        .max_h(px(spacing::IMAGE_MAX_HEIGHT)),
                )
                .into_any_element(),
            Body::Text(lines) => padded()
                .debug_selector(move || format!("hover-preview-text-{level}"))
                .children(lines.iter().enumerate().map(|(i, line)| {
                    div()
                        .when(i == 0, |d| d.text_color(theme.text.primary))
                        .when(i > 0, |d| d.text_color(theme.text.secondary))
                        .child(line.clone())
                }))
                .into_any_element(),
            Body::Unresolved { text, create } => {
                let can_create = create.is_some() && !AppSettings::viewer_mode(cx);
                padded()
                    .debug_selector(move || format!("hover-preview-unresolved-{level}"))
                    .child(
                        div()
                            .text_size(px(spacing::TITLE_SIZE))
                            .line_height(px(spacing::TITLE_LINE))
                            .font_weight(FontWeight(text::weight::BOLD))
                            .child(preview.title.clone()),
                    )
                    .child(div().text_color(theme.text.secondary).child(text.clone()))
                    .when(can_create, |body| {
                        body.child(div().flex().child(create_button(level, theme).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.create_from_preview(level, window, cx);
                            }),
                        )))
                    })
                    .into_any_element()
            }
            Body::Candidates(rows) => div()
                .debug_selector(move || format!("hover-preview-candidates-{level}"))
                .p(px(4.0))
                .flex()
                .flex_col()
                .children(rows.iter().enumerate().map(|(i, (label, path))| {
                    let path = path.clone();
                    row(theme, false)
                        .id(("hover-preview-candidate", i))
                        .debug_selector(move || format!("hover-preview-candidate-{i}"))
                        .hover(|s| s.bg(theme.state.hover))
                        .child(div().truncate().child(label.clone()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_candidate(level, path.clone(), window, cx);
                        }))
                }))
                .into_any_element(),
        }
    }
}

/// The footer's key hints: Enter opens, Mod+Enter in a new tab, Esc.
fn footer(theme: &Theme) -> Div {
    let key = |k: &'static str| {
        div()
            .font_family(font::EDITOR)
            .text_color(theme.text.primary)
            .child(k)
    };
    let hint =
        |k: &'static str, what: &'static str| div().flex().gap(px(4.0)).child(key(k)).child(what);
    let mod_enter = if cfg!(target_os = "macos") {
        "Cmd Enter"
    } else {
        "Ctrl Enter"
    };
    div()
        .h(px(spacing::FOOTER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(spacing::FOOTER_GAP))
        .px(px(spacing::FOOTER_PAD_X))
        .border_t(px(HAIRLINE))
        .border_color(theme.border.default)
        .text_size(px(text::UI_SMALL.size))
        .text_color(theme.text.secondary)
        .child(hint("Enter", "open"))
        .child(hint(mod_enter, "new tab"))
        .child(div().flex_1())
        .child(key("Esc"))
}

/// The primary button of an unresolved link's preview (SPEC § Controls).
fn create_button(level: usize, theme: &Theme) -> Stateful<Div> {
    div()
        .id(("hover-preview-create", level))
        .debug_selector(move || format!("hover-preview-create-{level}"))
        .h(px(size::CONTROL))
        .px(px(11.0))
        .flex()
        .items_center()
        .rounded(px(crate::metrics::radius::CONTROL))
        .bg(theme.accent.default)
        .text_color(theme.accent.on)
        .font_weight(FontWeight(text::weight::ACTIVE))
        .child("Create note")
}
