// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Find and replace in the Source view.
//!
//! - **The popover** is the Reading view's ([`FindBar`]), with the replace
//!   row: Mod+F opens the find row (filled with the selection when that is
//!   one line of text), Ctrl+H (Cmd+Alt+F on macOS) both rows. It sits at
//!   the top right of the view, outside the element that has the editor's
//!   focus, so the editor's own keys and handlers never act on the fields;
//!   the commands that also work from the fields (find, replace, next,
//!   previous, Esc) are handled by the element around both.
//! - **What is searched**: the editor's text, with the matching rules of
//!   find in page ([`crate::find::source`]); matches are byte ranges of the
//!   text.
//! - **Incremental**: every change of the query or of an option, and every
//!   edit of the text while the popover is open, starts a new search. Texts
//!   of up to [`SYNC_BYTES`] are searched on the UI thread; larger ones on
//!   the background executor, where a newer search stops an older one at
//!   its next match. Results for an older text revision are dropped.
//! - **Capped** at [`MAX_MATCHES`] for the count ("10,000+"), the
//!   highlights, next and previous and select all; Replace all replaces
//!   every match.
//! - **The current match** is selected in the editor (the caret at its
//!   end), and the view scrolls to it. A change of the query keeps the
//!   place: the first match at or after the selection's start. Next and
//!   previous go from the selection, so a caret moved by hand is where
//!   they start; both wrap around. An edit of the text re-counts without
//!   moving the selection.
//! - **Replace** replaces the selected match and selects the next one;
//!   when the selection is not a match (the caret moved, the text changed),
//!   it selects the current match first. **Replace all** is one
//!   transaction, so one undo step. With the regular expression on, `$1` and `${name}`
//!   expand capture groups.
//! - **Select all matches** (Alt+Enter in a field) closes the popover and
//!   leaves one selection per match, the current one primary.
//! - **Highlights**: matches on the rows on screen get an `accent.tint`
//!   background under the selections, the current one `accent.line` over
//!   them (as in the Reading view).
//! - **Search hits**: a note opened from a result of the Search panel
//!   shows that result's matches as the current match is shown
//!   ([`SearchHit`]) until the next edit, click, Esc, find, or another
//!   search.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, ParentElement as _, Stateful,
    Styled as _, Subscription, Task, Window, div, px,
};
use gv_editor::{EditKind, SelectionSpec};
use gv_text::Revision;

use super::EditorView;
use crate::find::source::{self, MAX_MATCHES, Replacer};
use crate::find::{
    CloseFind, Find, FindBar, FindBarEvent, FindNext, FindPrevious, FindStatus, Replace,
};
use crate::inline::Token;
use crate::metrics::spacing::find as find_spacing;

/// With `GLYPHVANE_DEBUG_REPLACE=QUERY->REPLACEMENT` the first Source view
/// that shows a document opens find and replace with those texts (for
/// screenshots of the popover).
pub const DEBUG_REPLACE_ENV: &str = "GLYPHVANE_DEBUG_REPLACE";

/// One per process: the first Source view that shows a document takes it.
static DEBUG_REPLACE_TAKEN: AtomicBool = AtomicBool::new(false);

/// Texts of at most this many bytes are searched on the UI thread (a few
/// milliseconds at most).
pub(crate) const SYNC_BYTES: usize = 512 * 1024;

/// The token of a match highlight (the Reading view's).
pub(crate) const MATCH: Token = Token::AccentTint;
/// The token of the current match's highlight.
pub(crate) const CURRENT_MATCH: Token = Token::AccentLine;

/// The matches of one text revision.
#[derive(Debug, Clone, Default)]
pub(crate) struct FindMarks {
    /// The revision they are for.
    pub revision: Option<Revision>,
    /// The text searched, for expanding capture groups.
    hay: Option<Arc<str>>,
    pub matches: Arc<Vec<Range<usize>>>,
    /// More matches than [`MAX_MATCHES`].
    pub more: bool,
    pub current: Option<usize>,
}

impl FindMarks {
    fn status(&self) -> FindStatus {
        FindStatus::Matches {
            current: self.current,
            total: self.matches.len(),
            more: self.more,
        }
    }

    /// The matches that touch `range` of revision `revision`, each with
    /// whether it is the current one.
    pub(crate) fn touching(
        &self,
        revision: Revision,
        range: Range<usize>,
    ) -> Vec<(Range<usize>, bool)> {
        if self.revision != Some(revision) {
            return Vec::new();
        }
        let from = self.matches.partition_point(|m| m.end < range.start);
        self.matches[from..]
            .iter()
            .enumerate()
            .take_while(|(_, m)| m.start <= range.end)
            .map(|(k, m)| (m.clone(), Some(from + k) == self.current))
            .collect()
    }
}

/// The matches of a Search panel result the note was opened at, painted
/// as the current find match is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchHit {
    /// Byte ranges of the text.
    pub ranges: Vec<Range<usize>>,
    /// The text revision they are for; `None` until the text is loaded.
    pub revision: Option<Revision>,
}

/// Where a search puts the current match.
#[derive(Debug, Clone, Copy)]
struct Place {
    /// The first match at or after this offset (wrapping).
    from: usize,
    /// Select it in the editor and scroll to it.
    select: bool,
}

/// Find and replace of one Source view.
#[derive(Default)]
pub(crate) struct FindState {
    bar: Option<Entity<FindBar>>,
    open: bool,
    pub(crate) marks: FindMarks,
    /// The Search panel result the note was opened at.
    search_hit: Option<SearchHit>,
    /// Bumped per search; a search that sees a newer value stops.
    generation: Arc<AtomicU64>,
    /// The revision the latest search is for.
    searched: Option<Revision>,
    /// Where the search after the next edit puts the current match.
    after_edit: Option<Place>,
    task: Option<Task<()>>,
    _events: Option<Subscription>,
}

impl FindState {
    /// The Search panel result's ranges that touch `range` of revision
    /// `revision`, as current matches.
    pub(crate) fn search_hit_touching(
        &self,
        revision: Revision,
        range: &Range<usize>,
    ) -> Vec<(Range<usize>, bool)> {
        self.search_hit
            .as_ref()
            .filter(|h| h.revision == Some(revision))
            .map(|h| {
                h.ranges
                    .iter()
                    .filter(|r| r.end >= range.start && r.start <= range.end)
                    .map(|r| (r.clone(), true))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl std::fmt::Debug for FindState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FindState")
            .field("open", &self.open)
            .field("marks", &self.marks)
            .field("searched", &self.searched)
            .finish_non_exhaustive()
    }
}

impl EditorView {
    /// The find popover, if it was ever opened.
    pub fn find_bar(&self) -> Option<&Entity<FindBar>> {
        self.find.bar.as_ref()
    }

    /// Whether the find popover is open.
    pub fn is_find_open(&self) -> bool {
        self.find.open
    }

    /// The matches of the current text (empty while a search runs).
    #[cfg(test)]
    pub(crate) fn find_marks(&self) -> &FindMarks {
        &self.find.marks
    }

    /// Highlights `ranges` of the text (a Search panel result's matches),
    /// now when the text is loaded, else once it is.
    pub fn show_search_hit(&mut self, ranges: Vec<Range<usize>>, cx: &mut Context<'_, Self>) {
        self.find.search_hit = (!ranges.is_empty()).then_some(SearchHit {
            ranges,
            revision: None,
        });
        self.attach_search_hit();
        cx.notify();
    }

    /// Drops the highlight of a Search panel result. Returns whether one
    /// was shown.
    pub fn clear_search_hit(&mut self, cx: &mut Context<'_, Self>) -> bool {
        let revision = self.doc.as_ref().map(|d| d.editor.text().revision());
        let shown = self
            .find
            .search_hit
            .take()
            .is_some_and(|h| h.revision.is_some() && h.revision == revision);
        if shown {
            cx.notify();
        }
        shown
    }

    /// Ties a waiting Search panel result to the loaded text, when its
    /// ranges are in it.
    pub(super) fn attach_search_hit(&mut self) {
        let Some(text) = self.doc.as_ref().map(|d| d.editor.text()) else {
            return;
        };
        let Some(hit) = &mut self.find.search_hit else {
            return;
        };
        if hit.revision.is_some() {
            return;
        }
        if hit
            .ranges
            .iter()
            .all(|r| text.check_range(r.clone()).is_ok())
        {
            hit.revision = Some(text.revision());
        } else {
            self.find.search_hit = None;
        }
    }

    /// The ranges of the Search panel result highlighted now.
    #[cfg(test)]
    pub(crate) fn search_hit(&self) -> Option<Vec<Range<usize>>> {
        let revision = self.doc.as_ref()?.editor.text().revision();
        self.find
            .search_hit
            .as_ref()
            .filter(|h| h.revision == Some(revision))
            .map(|h| h.ranges.clone())
    }

    /// A new document: matches of the old one no longer apply.
    pub(super) fn reset_find(&mut self) {
        self.find.generation.fetch_add(1, Ordering::Relaxed);
        self.find.task = None;
        self.find.marks = FindMarks::default();
        self.find.searched = None;
        self.find.after_edit = None;
    }

    fn ensure_find_bar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<FindBar> {
        if let Some(bar) = &self.find.bar {
            return bar.clone();
        }
        let bar = cx.new(|cx| FindBar::for_editor(window, cx));
        self.find._events = Some(cx.subscribe_in(
            &bar,
            window,
            |this, _, event: &FindBarEvent, window, cx| match event {
                FindBarEvent::Changed => this.find_changed(cx),
                FindBarEvent::Next => this.find_step(true, cx),
                FindBarEvent::Previous => this.find_step(false, cx),
                FindBarEvent::Close => {
                    this.close_find(window, cx);
                }
                FindBarEvent::Replace => this.replace_current(window, cx),
                FindBarEvent::ReplaceAll => this.replace_all(window, cx),
                FindBarEvent::SelectAll => this.select_all_matches(window, cx),
            },
        ));
        self.find.bar = Some(bar.clone());
        bar
    }

    /// The handlers of the element around the editor and the popover.
    pub(super) fn find_handlers(
        &mut self,
        host: Stateful<gpui::Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<gpui::Div> {
        host.on_action(cx.listener(|this, _: &Find, w, cx| this.open_find(false, w, cx)))
            .on_action(cx.listener(|this, _: &Replace, w, cx| this.open_find(true, w, cx)))
            .on_action(cx.listener(|this, _: &FindNext, w, cx| this.next_or_open(true, w, cx)))
            .on_action(cx.listener(|this, _: &FindPrevious, w, cx| {
                this.next_or_open(false, w, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseFind, w, cx| {
                if !this.close_find(w, cx) {
                    cx.propagate();
                }
            }))
    }

    /// The popover, at the top right of the view, while it is open.
    pub(super) fn find_popover(&self) -> Option<gpui::Div> {
        let bar = self.find.bar.clone().filter(|_| self.find.open)?;
        Some(
            div()
                .absolute()
                .top(px(find_spacing::INSET_TOP))
                .right(px(find_spacing::INSET_RIGHT))
                .child(bar),
        )
    }

    /// The debug query of [`DEBUG_REPLACE_ENV`], once per process, when a
    /// document shows.
    pub(super) fn debug_replace(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        static TEXTS: OnceLock<Option<(String, String)>> = OnceLock::new();
        if DEBUG_REPLACE_TAKEN.load(Ordering::Relaxed) {
            return;
        }
        let Some((query, replacement)) = TEXTS.get_or_init(|| {
            let value = std::env::var(DEBUG_REPLACE_ENV).ok()?;
            let (query, replacement) = value.split_once("->").unwrap_or((&value, ""));
            Some((query.to_owned(), replacement.to_owned()))
        }) else {
            return;
        };
        if DEBUG_REPLACE_TAKEN.swap(true, Ordering::Relaxed) {
            return;
        }
        let bar = self.ensure_find_bar(window, cx);
        self.find.open = true;
        bar.update(cx, |bar, cx| {
            bar.set_replacing(true, cx);
            bar.set_query(query, window, cx);
            bar.set_replacement(replacement, window, cx);
            bar.focus_field(window, cx);
        });
        let from = self.selection_start();
        self.search(Place { from, select: true }, cx);
    }

    /// The start of the primary selection.
    fn selection_start(&self) -> usize {
        self.editor()
            .map_or(0, |e| e.selections().primary().start())
    }

    /// Mod+F (`replace` false) and Ctrl+H: opens the popover (or keeps it
    /// open), fills the find field with the selection when that is one line
    /// of text, and gives the keyboard to the find field with its text
    /// selected; find and replace with a query gives it to the replace
    /// field. Mod+F keeps the replace row of an open popover.
    pub(super) fn open_find(
        &mut self,
        replace: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.find.search_hit = None;
        let bar = self.ensure_find_bar(window, cx);
        let prefill = self.editor().and_then(|editor| {
            if editor.selections().len() != 1 {
                return None;
            }
            let range = editor.selections().primary().range();
            let text = editor.text().text_for_range(range).ok()?;
            (!text.is_empty() && !text.contains(['\n', '\r'])).then_some(text)
        });
        let was_open = std::mem::replace(&mut self.find.open, true);
        bar.update(cx, |bar, cx| {
            if let Some(text) = &prefill {
                bar.set_query(text, window, cx);
            }
            bar.set_replacing(replace || (was_open && bar.is_replacing()), cx);
            if replace && !bar.query(cx).is_empty() {
                bar.focus_replace(window, cx);
            } else {
                bar.focus_field(window, cx);
            }
        });
        if prefill.is_some() || !was_open {
            let from = self.selection_start();
            self.search(Place { from, select: true }, cx);
        }
        cx.notify();
    }

    /// Esc or the close button: closes the popover, drops the highlights
    /// and gives the keyboard back to the editor. The query, the
    /// replacement and the options stay for the next time. Returns whether
    /// it was open.
    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> bool {
        if !self.find.open {
            return false;
        }
        self.find.open = false;
        self.reset_find();
        if let Some(bar) = &self.find.bar {
            bar.update(cx, |bar, cx| bar.set_replacing(false, cx));
        }
        self.focus.focus(window, cx);
        cx.notify();
        true
    }

    /// F3 and Shift+F3 (Cmd+G, Cmd+Shift+G): the next or previous match;
    /// with the popover closed, they open it first.
    fn next_or_open(&mut self, forward: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.find.open {
            self.find_step(forward, cx);
        } else {
            self.open_find(false, window, cx);
        }
    }

    /// The query or an option changed: search again from the selection.
    pub(super) fn find_changed(&mut self, cx: &mut Context<'_, Self>) {
        let from = self.selection_start();
        self.search(Place { from, select: true }, cx);
    }

    /// After every change of the text or the selections (the view's
    /// `after_edit`): a new revision of the text is searched again.
    pub(super) fn find_after_edit(&mut self, cx: &mut Context<'_, Self>) {
        if !self.find.open {
            return;
        }
        let Some(revision) = self.editor().map(|e| e.text().revision()) else {
            return;
        };
        if self.find.searched == Some(revision) {
            return;
        }
        let place = self.find.after_edit.take().unwrap_or(Place {
            from: self.selection_start(),
            select: false,
        });
        self.search(place, cx);
    }

    /// Whether the matches are those of the current text.
    fn marks_current(&self) -> bool {
        let revision = self.editor().map(|e| e.text().revision());
        revision.is_some() && self.find.marks.revision == revision
    }

    /// Selects match `k` in the editor and scrolls to it.
    fn select_match(&mut self, k: usize, cx: &mut Context<'_, Self>) {
        let Some(range) = self.find.marks.matches.get(k).cloned() else {
            return;
        };
        self.find.marks.current = Some(k);
        if let Some(doc) = self.doc.as_mut() {
            doc.editor.select_ranges([range]);
            self.autoscroll = true;
        }
        cx.notify();
    }

    /// Next or previous from the selection, wrapping around.
    fn find_step(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        if !self.marks_current() {
            return;
        }
        let Some(selection) = self.editor().map(|e| e.selections().primary().range()) else {
            return;
        };
        let matches = &self.find.marks.matches;
        let k = if forward {
            source::at_or_after(matches, selection.end)
        } else {
            source::before(matches, selection.start)
        };
        let Some(k) = k else {
            return;
        };
        self.select_match(k, cx);
        self.show_find_status(cx);
    }

    /// The selected match, if the primary selection is one.
    fn selected_match(&self) -> Option<Range<usize>> {
        let marks = &self.find.marks;
        let selected = self.editor()?.selections().primary().range();
        let k = marks
            .matches
            .binary_search_by_key(&selected.start, |m| m.start)
            .ok()?;
        (marks.matches[k] == selected).then_some(selected)
    }

    /// The query, the options and the replacement of the popover; `None`
    /// in viewer mode, where nothing is replaced.
    fn replace_texts(&self, cx: &Context<'_, Self>) -> Option<(String, Replacer)> {
        if self.read_only {
            return None;
        }
        let bar = self.find.bar.as_ref()?.read(cx);
        let query = bar.query(cx);
        let replacer = Replacer::new(&query, bar.options(), &bar.replacement(cx)).ok()?;
        Some((query, replacer))
    }

    /// Replace: the selected match, then the next one is selected. When
    /// the selection is not a match, the current match is selected first.
    fn replace_current(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.marks_current() {
            return;
        }
        let Some(hay) = self.find.marks.hay.clone() else {
            return;
        };
        let Some(range) = self.selected_match() else {
            if let Some(k) = self.find.marks.current {
                self.select_match(k, cx);
                self.show_find_status(cx);
            }
            return;
        };
        let Some((_, replacer)) = self.replace_texts(cx) else {
            return;
        };
        let text = replacer.replacement(&hay, range.clone());
        let place = Place {
            from: range.start + text.len(),
            select: true,
        };
        self.find.after_edit = Some(place);
        self.with_editor(window, cx, |editor, _| {
            if let Err(err) = editor.edit([(range, text)], EditKind::Command) {
                tracing::warn!("replace: {err}");
            }
        });
        // The text did not change (the same text): go on to the next match.
        if let Some(place) = self.find.after_edit.take() {
            if let Some(k) = source::at_or_after(&self.find.marks.matches, place.from) {
                self.select_match(k, cx);
            }
            self.show_find_status(cx);
        }
    }

    /// Replace all: every match (not only the counted ones), in one
    /// transaction.
    fn replace_all(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(bar) = self.find.bar.clone() else {
            return;
        };
        let (query, options) = bar.read_with(cx, |bar, cx| (bar.query(cx), bar.options()));
        let Ok(Some(finder)) = source::finder(&query, options) else {
            return;
        };
        let Some((_, replacer)) = self.replace_texts(cx) else {
            return;
        };
        let Some(text) = self.editor().map(|e| e.text().clone()) else {
            return;
        };
        let hay = text.text_for_range(0..text.len()).unwrap_or_default();
        let edits = replacer.edits(&finder, &hay);
        if edits.is_empty() {
            return;
        }
        self.find.after_edit = Some(Place {
            from: self.selection_start(),
            select: false,
        });
        self.with_editor(window, cx, |editor, _| {
            if let Err(err) = editor.edit(edits, EditKind::Command) {
                tracing::warn!("replace all: {err}");
            }
        });
        self.find.after_edit = None;
    }

    /// Alt+Enter: one selection per match, the current one primary; the
    /// popover closes.
    fn select_all_matches(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.marks_current() || self.find.marks.matches.is_empty() {
            return;
        }
        let marks = &self.find.marks;
        let specs: Vec<SelectionSpec> = marks
            .matches
            .iter()
            .map(|m| SelectionSpec::range(m.clone()))
            .collect();
        let primary = marks.current.unwrap_or(0);
        self.close_find(window, cx);
        self.with_editor(window, cx, |editor, _| {
            editor.set_selections(&specs, primary)
        });
    }

    fn show_find_status(&self, cx: &mut Context<'_, Self>) {
        let status = self.find.marks.status();
        self.set_find_status(status, cx);
    }

    fn set_find_status(&self, status: FindStatus, cx: &mut Context<'_, Self>) {
        if let Some(bar) = &self.find.bar {
            bar.update(cx, |bar, cx| bar.set_status(status, cx));
        }
    }

    /// Starts a search for the popover's query in the current text.
    fn search(&mut self, place: Place, cx: &mut Context<'_, Self>) {
        let Some(bar) = self.find.bar.clone() else {
            return;
        };
        if !self.find.open {
            return;
        }
        let Some(text) = self.editor().map(|e| e.text().clone()) else {
            return;
        };
        let revision = text.revision();
        let (query, options) = bar.read_with(cx, |bar, cx| (bar.query(cx), bar.options()));
        let mine = self.find.generation.fetch_add(1, Ordering::Relaxed) + 1;
        // It stops at its next match.
        self.find.task = None;
        self.find.searched = Some(revision);
        let finder = match source::finder(&query, options) {
            Ok(Some(f)) => f,
            Ok(None) => {
                self.show_nothing(revision, FindStatus::Empty, cx);
                return;
            }
            Err(err) => {
                tracing::debug!("find: {err}");
                self.show_nothing(revision, FindStatus::Invalid, cx);
                return;
            }
        };
        let generation = self.find.generation.clone();
        let sync = text.len() <= SYNC_BYTES;
        let job = move || {
            let hay: Arc<str> = Arc::from(text.text_for_range(0..text.len()).unwrap_or_default());
            let found = source::find_all(&finder, &hay, MAX_MATCHES, || {
                generation.load(Ordering::Relaxed) != mine
            });
            (hay, found)
        };
        if sync {
            let (hay, found) = job();
            self.finish_search(mine, revision, hay, found, place, cx);
            return;
        }
        self.find.task = Some(cx.spawn(async move |this, cx| {
            let (hay, found) = cx.background_spawn(async move { job() }).await;
            this.update(cx, |view, cx| {
                view.finish_search(mine, revision, hay, found, place, cx);
            })
            .ok();
        }));
    }

    /// No matches to show: an empty or invalid query.
    fn show_nothing(&mut self, revision: Revision, status: FindStatus, cx: &mut Context<'_, Self>) {
        self.find.marks = FindMarks {
            revision: Some(revision),
            ..FindMarks::default()
        };
        self.set_find_status(status, cx);
        cx.notify();
    }

    fn finish_search(
        &mut self,
        mine: u64,
        revision: Revision,
        hay: Arc<str>,
        found: Option<(Vec<Range<usize>>, bool)>,
        place: Place,
        cx: &mut Context<'_, Self>,
    ) {
        if mine != self.find.generation.load(Ordering::Relaxed) || !self.find.open {
            return;
        }
        if self.editor().map(|e| e.text().revision()) != Some(revision) {
            return;
        }
        let Some((matches, more)) = found else {
            return;
        };
        self.find.task = None;
        let current = source::at_or_after(&matches, place.from);
        self.find.marks = FindMarks {
            revision: Some(revision),
            hay: Some(hay),
            matches: Arc::new(matches),
            more,
            current,
        };
        if place.select
            && let Some(k) = current
        {
            self.select_match(k, cx);
        }
        self.show_find_status(cx);
        cx.notify();
    }
}
