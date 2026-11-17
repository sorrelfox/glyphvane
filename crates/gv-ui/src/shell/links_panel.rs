// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Backlinks and *Links in this note* sections of the right panel, and
//! the link findings of the status bar (FR-VAULT-07, FR-NAV-14).
//!
//! - **Backlinks:** the links to the active note, grouped by source note: a
//!   row with the note's title and the heading the links are under, then
//!   one context line per link with the link's text marked. A click on a
//!   context line opens the source there; a click on a source row folds
//!   its lines away. Embeds, front matter links and links through an alias
//!   are labelled.
//! - **Unlinked mentions:** a folded row at the end of Backlinks with the
//!   number of plain-text mentions of the note's title, file name and
//!   aliases. Unfolded, it lists them like backlinks, each with a **Link**
//!   button that writes the link into the source's buffer as one undoable
//!   edit (the source opens in a background tab when it is not open).
//! - **Links in this note:** the active note's links with their statuses;
//!   the head counts errors and warnings like the Link checker. A click
//!   opens a resolved link's target, or shows an unresolved one in the
//!   note.
//! - **Status bar:** in Preview, the note's link findings (icon and count
//!   per severity); a click shows *Links in this note*.
//!
//! The data comes from the vault's content index. The queries run off the
//! window's thread whenever the active note or the index changes; the
//! context lines are read in pages after the list is shown, and the rows
//! are drawn lazily, so a hub note with tens of thousands of backlinks
//! stays responsive. Unlinked mentions read every note; after an index
//! change they wait [`MENTIONS_DELAY`] for more changes first.
//!
//! Not designed (built from the Outline's and the Files panel's parts):
//! the expanded *Links in this note*, the unlinked mentions' rows and Link
//! button, the empty states, the count shown for a folded source, the
//! selected row, and what the status bar's findings do on a click.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, HighlightStyle, InteractiveElement as _, IntoElement, ListAlignment, ListState,
    MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledText, Subscription, Task, Window, div, list, px,
};
use gv_vault::VaultPath;
use gv_vault::content::{ContentOptions, ContentSnapshot, DiskSource};
use gv_vault::profile::ProfileId;

use super::files_panel::{
    CollapseOrParent, ExpandOrChild, KEY_CONTEXT, OpenSelected, SelectNext, SelectPrevious,
};
use super::links_data::{
    self, Findings, Group, Outgoing, Severity, Snippet, backlink_groups, mention_groups,
};
use super::panels::section_head;
use super::vault::{ContentUpdated, VaultStatus, WindowVault};
use super::{OpenTarget, ViewMode, Workspace};
use crate::a11y::Role;
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::keymap::actions::workspace::ShowOutgoingLinks;
use crate::metrics::spacing::backlinks as spacing;
use crate::metrics::{HAIRLINE, icon, radius, size, text};
use crate::reader::ScrollTarget;
use crate::settings::AppSettings;
use crate::theme::Theme;

/// How long unlinked mentions wait after an index change before they are
/// looked for again (they read every note).
pub(super) const MENTIONS_DELAY: Duration = Duration::from_millis(400);

/// Source notes whose context lines are read in one go.
const SNIPPET_PAGE: usize = 256;

/// What the sections ask of their window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LinksPanelEvent {
    /// Open `path`, at the byte offset when there is one.
    Open {
        path: VaultPath,
        offset: Option<usize>,
    },
    /// Turn the words at `span` in `source` into a link to `target`.
    Link(LinkRequest),
}

/// An unlinked mention to turn into a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LinkRequest {
    pub source: VaultPath,
    pub span: Range<usize>,
    /// The title or alias the words match (ignoring case).
    pub term: Arc<str>,
    pub target: VaultPath,
    /// No other note has the target's name.
    pub unique_stem: bool,
}

/// Why the sections have nothing to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    /// The tab shows no file.
    NoDocument,
    /// The file is not in an indexed vault.
    NoVault,
    /// The vault's indexes are being built.
    Indexing,
    Ready,
}

/// One row of a section's list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A message where rows would be.
    Message(&'static str),
    /// A source of backlinks.
    Head(usize),
    /// A backlink's context line.
    Line(usize, usize),
    /// The unlinked mentions' row.
    MentionsHead,
    MentionHead(usize),
    MentionLine(usize, usize),
    /// A link of the active note.
    Out(usize),
}

impl Row {
    /// The height used before the row is measured.
    fn estimate(self) -> f32 {
        match self {
            Self::Line(..) | Self::MentionLine(..) => {
                spacing::LINE_PAD_TOP
                    + text::UI_SECTION.line
                    + spacing::LINE_PAD_BOTTOM
                    + spacing::LINE_GAP
            }
            _ => size::OUTLINE_ROW,
        }
    }

    fn selector(self) -> String {
        match self {
            Self::Message(_) => "links-message".into(),
            Self::Head(g) => format!("backlink-head-{g}"),
            Self::Line(g, i) => format!("backlink-line-{g}-{i}"),
            Self::MentionsHead => "unlinked-mentions-head".into(),
            Self::MentionHead(g) => format!("mention-head-{g}"),
            Self::MentionLine(g, i) => format!("mention-line-{g}-{i}"),
            Self::Out(i) => format!("outgoing-row-{i}"),
        }
    }
}

/// Links (or mentions) by source with their context lines, once read.
#[derive(Debug, Default)]
struct Listing {
    groups: Vec<Group>,
    snippets: Vec<Vec<Option<Snippet>>>,
    count: usize,
}

impl Listing {
    fn new(groups: Vec<Group>) -> Self {
        let snippets = groups.iter().map(|g| vec![None; g.items.len()]).collect();
        let count = groups.iter().map(|g| g.items.len()).sum();
        Self {
            groups,
            snippets,
            count,
        }
    }

    fn keys(&self) -> Vec<(VaultPath, Vec<Range<usize>>)> {
        self.groups
            .iter()
            .map(|g| {
                (
                    g.source.clone(),
                    g.items.iter().map(|i| i.span.clone()).collect(),
                )
            })
            .collect()
    }
}

/// How long the last queries took, for tests and logs.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Timings {
    /// Backlinks and outgoing links, off the window's thread.
    pub query: Option<Duration>,
    /// Every context line.
    pub snippets: Option<Duration>,
    /// Unlinked mentions.
    pub mentions: Option<Duration>,
}

/// The Backlinks and *Links in this note* sections.
pub(super) struct LinksPanel {
    focus: FocusHandle,
    vault: Option<Entity<WindowVault>>,
    _vault_events: Vec<Subscription>,
    /// The active tab's file.
    file: Option<PathBuf>,
    target: Option<VaultPath>,
    phase: Phase,
    /// The right panel is shown: the lists are kept up to date.
    visible: bool,
    /// Sources may be edited (viewer mode is off).
    can_link: bool,
    /// The snapshot the lists are from.
    generation: Option<u64>,
    backlinks: Listing,
    mentions: Option<Listing>,
    outgoing: Vec<Outgoing>,
    findings: Option<Findings>,
    backlinks_open: bool,
    mentions_open: bool,
    outgoing_open: bool,
    folded: HashSet<VaultPath>,
    folded_mentions: HashSet<VaultPath>,
    selected: Option<Row>,
    rows: Vec<Row>,
    out_rows: Vec<Row>,
    backlinks_list: ListState,
    outgoing_list: ListState,
    /// Increases with each query; late answers to older ones are dropped.
    job: u64,
    _query: Option<Task<()>>,
    _mentions: Option<Task<()>>,
    mentions_cancel: Arc<AtomicBool>,
    timings: Timings,
}

impl EventEmitter<LinksPanelEvent> for LinksPanel {}

impl Focusable for LinksPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl LinksPanel {
    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        let mut this = Self {
            focus: cx.focus_handle(),
            vault: None,
            _vault_events: Vec::new(),
            file: None,
            target: None,
            phase: Phase::NoDocument,
            visible: false,
            can_link: true,
            generation: None,
            backlinks: Listing::default(),
            mentions: None,
            outgoing: Vec::new(),
            findings: None,
            backlinks_open: true,
            mentions_open: false,
            outgoing_open: false,
            folded: HashSet::new(),
            folded_mentions: HashSet::new(),
            selected: None,
            rows: Vec::new(),
            out_rows: Vec::new(),
            backlinks_list: ListState::new(0, ListAlignment::Top, px(200.0)),
            outgoing_list: ListState::new(0, ListAlignment::Top, px(200.0)),
            job: 0,
            _query: None,
            _mentions: None,
            mentions_cancel: Arc::default(),
            timings: Timings::default(),
        };
        this.rebuild_rows();
        this
    }

    /// Follows the window: its vault, the active tab's file, whether the
    /// right panel is shown and whether sources may be edited.
    pub fn set_context(
        &mut self,
        vault: Option<Entity<WindowVault>>,
        file: Option<PathBuf>,
        visible: bool,
        can_link: bool,
        cx: &mut Context<'_, Self>,
    ) {
        let vault_changed =
            self.vault.as_ref().map(Entity::entity_id) != vault.as_ref().map(Entity::entity_id);
        if vault_changed {
            self._vault_events = vault
                .as_ref()
                .map(|v| {
                    vec![
                        cx.subscribe(v, |this, _, _: &ContentUpdated, cx| {
                            this.refresh(true, cx);
                        }),
                        cx.observe(v, |this, _, cx| this.refresh(false, cx)),
                    ]
                })
                .unwrap_or_default();
            self.vault = vault;
        }
        let file_changed = self.file != file;
        let shown = visible && !self.visible;
        self.file = file;
        self.visible = visible;
        if self.can_link != can_link {
            self.can_link = can_link;
            cx.notify();
        }
        if vault_changed || file_changed {
            self.generation = None;
            self.selected = None;
            self.folded.clear();
            self.folded_mentions.clear();
            self.backlinks = Listing::default();
            self.mentions = None;
            self.outgoing.clear();
            self.findings = None;
        }
        if vault_changed || file_changed || shown {
            if shown {
                self.generation = None;
            }
            self.refresh(false, cx);
        }
    }

    /// The active note's link findings, once known.
    pub fn findings(&self) -> Option<Findings> {
        self.findings
    }

    /// Expands Backlinks (`workspace.show_backlinks`).
    pub fn expand_backlinks(&mut self, cx: &mut Context<'_, Self>) {
        if !self.backlinks_open {
            self.toggle_backlinks(cx);
        }
    }

    /// Expands *Links in this note* (`workspace.show_outgoing_links`).
    pub fn expand_outgoing(&mut self, cx: &mut Context<'_, Self>) {
        if !self.outgoing_open {
            self.outgoing_open = true;
            self.rebuild_rows();
            cx.notify();
        }
    }

    fn toggle_backlinks(&mut self, cx: &mut Context<'_, Self>) {
        self.backlinks_open = !self.backlinks_open;
        self.rebuild_rows();
        if self.backlinks_open {
            self.generation = None;
            self.refresh(false, cx);
        }
        cx.notify();
    }

    fn toggle_outgoing(&mut self, cx: &mut Context<'_, Self>) {
        self.outgoing_open = !self.outgoing_open;
        self.rebuild_rows();
        cx.notify();
    }

    fn toggle_mentions(&mut self, cx: &mut Context<'_, Self>) {
        self.mentions_open = !self.mentions_open;
        self.rebuild_rows();
        cx.notify();
    }

    fn toggle_group(&mut self, mentions: bool, group: usize, cx: &mut Context<'_, Self>) {
        let (listing, folded) = if mentions {
            (self.mentions.as_ref(), &mut self.folded_mentions)
        } else {
            (Some(&self.backlinks), &mut self.folded)
        };
        let Some(source) = listing.and_then(|l| l.groups.get(group)).map(|g| &g.source) else {
            return;
        };
        if !folded.remove(source) {
            folded.insert(source.clone());
        }
        self.rebuild_rows();
        cx.notify();
    }

    /// Where the note and the index are.
    fn phase_and_snapshot(&self, cx: &App) -> (Phase, Option<(ContentSnapshot, VaultPath)>) {
        let Some(file) = &self.file else {
            return (Phase::NoDocument, None);
        };
        let Some(vault) = &self.vault else {
            return (Phase::NoVault, None);
        };
        let vault = vault.read(cx);
        match vault.status() {
            VaultStatus::Building => return (Phase::Indexing, None),
            VaultStatus::NotIndexed(_) | VaultStatus::Failed(_) => return (Phase::NoVault, None),
            VaultStatus::Ready => {}
        }
        let Some(path) = vault_path(vault.root(), file) else {
            return (Phase::NoVault, None);
        };
        match vault.content() {
            Some(snapshot) => (Phase::Ready, Some((snapshot, path))),
            None => (Phase::Indexing, None),
        }
    }

    /// Looks again after the note, the vault or its index changed.
    /// `content`: the content index published a snapshot.
    fn refresh(&mut self, content: bool, cx: &mut Context<'_, Self>) {
        let (phase, found) = self.phase_and_snapshot(cx);
        let phase_changed = phase != self.phase;
        self.phase = phase;
        let Some((snapshot, target)) = found else {
            self.target = None;
            if phase_changed || self.generation.is_some() {
                self.generation = None;
                self.backlinks = Listing::default();
                self.mentions = None;
                self.outgoing.clear();
                self.findings = None;
                self.rebuild_rows();
                cx.notify();
            }
            return;
        };
        if self.generation == Some(snapshot.generation()) && self.target.as_ref() == Some(&target) {
            if phase_changed {
                self.rebuild_rows();
                cx.notify();
            }
            return;
        }
        self.generation = Some(snapshot.generation());
        self.target = Some(target.clone());
        self.job += 1;
        let job = self.job;
        let root = snapshot.root().map(Path::to_path_buf);
        let lists = self.visible;
        let snippets = self.visible && self.backlinks_open;
        let query = snapshot.clone();
        let query_target = target.clone();
        let root_for_query = root.clone();
        self._query = Some(cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let (outgoing, findings, groups) = cx
                .background_spawn(async move {
                    let source = root_for_query.as_deref().map(disk_source);
                    let text = source
                        .as_ref()
                        .and_then(|s| query.note_text(&query_target, s));
                    let (outgoing, findings) =
                        links_data::outgoing(&query, &query_target, text.as_deref());
                    let groups = lists.then(|| backlink_groups(&query, &query_target));
                    (outgoing, findings, groups)
                })
                .await;
            let elapsed = started.elapsed();
            let keys = this
                .update(cx, |this, cx| {
                    this.query_done(job, outgoing, findings, groups, elapsed, cx)
                })
                .ok()
                .flatten();
            let (Some(keys), true, Some(root)) = (keys, snippets, root) else {
                return;
            };
            let started = Instant::now();
            let keys = Arc::new(keys);
            let mut at = 0;
            while at < keys.len() {
                let end = (at + SNIPPET_PAGE).min(keys.len());
                let (snapshot, keys, root) = (snapshot.clone(), keys.clone(), root.clone());
                let page = cx
                    .background_spawn(async move {
                        let source = disk_source(&root);
                        links_data::snippets(&snapshot, &source, &keys[at..end], true)
                    })
                    .await;
                let kept = this
                    .update(cx, |this, cx| this.snippets_read(job, at, page, cx))
                    .unwrap_or(false);
                if !kept {
                    return;
                }
                at = end;
            }
            let elapsed = started.elapsed();
            this.update(cx, |this, _| {
                if this.job == job {
                    this.timings.snippets = Some(elapsed);
                }
            })
            .ok();
        }));
        if self.visible && self.backlinks_open {
            self.find_mentions(content, cx);
        }
    }

    /// The backlinks and outgoing links of query `job` are in; returns the
    /// sources whose context lines are to be read.
    fn query_done(
        &mut self,
        job: u64,
        outgoing: Vec<Outgoing>,
        findings: Findings,
        groups: Option<Vec<Group>>,
        elapsed: Duration,
        cx: &mut Context<'_, Self>,
    ) -> Option<Vec<(VaultPath, Vec<Range<usize>>)>> {
        if job != self.job {
            return None;
        }
        self.timings.query = Some(elapsed);
        tracing::debug!(?elapsed, "backlinks and outgoing links");
        self.outgoing = outgoing;
        self.findings = Some(findings);
        let keys = groups.map(|groups| {
            // Context lines already read stay until they are read again.
            let mut listing = Listing::new(groups);
            let old = std::mem::take(&mut self.backlinks);
            let by_source: std::collections::HashMap<&VaultPath, usize> = old
                .groups
                .iter()
                .enumerate()
                .map(|(ix, g)| (&g.source, ix))
                .collect();
            for (group, snippets) in listing.groups.iter().zip(listing.snippets.iter_mut()) {
                if let Some(&ix) = by_source.get(&group.source)
                    && old.groups[ix].items == group.items
                {
                    snippets.clone_from(&old.snippets[ix]);
                }
            }
            let keys = listing.keys();
            self.backlinks = listing;
            keys
        });
        self.rebuild_rows();
        cx.notify();
        keys
    }

    fn snippets_read(
        &mut self,
        job: u64,
        at: usize,
        page: Vec<Vec<Option<Snippet>>>,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        if job != self.job {
            return false;
        }
        for (slot, snippets) in self.backlinks.snippets[at..].iter_mut().zip(page) {
            *slot = snippets;
        }
        self.backlinks_list.remeasure();
        cx.notify();
        true
    }

    /// Looks for unlinked mentions of the active note, after
    /// [`MENTIONS_DELAY`] when the index changed.
    fn find_mentions(&mut self, delay: bool, cx: &mut Context<'_, Self>) {
        let (Phase::Ready, Some((snapshot, target))) = self.phase_and_snapshot(cx) else {
            return;
        };
        let Some(root) = snapshot.root().map(Path::to_path_buf) else {
            return;
        };
        self.mentions_cancel.store(true, Ordering::Release);
        let cancel = Arc::new(AtomicBool::new(false));
        self.mentions_cancel = cancel.clone();
        let job = self.job;
        self._mentions = Some(cx.spawn(async move |this, cx| {
            if delay {
                cx.background_executor().timer(MENTIONS_DELAY).await;
            }
            let started = Instant::now();
            let found = cx
                .background_spawn(async move {
                    let source = disk_source(&root);
                    let mentions = snapshot.unlinked_mentions(&target, &source, &cancel).ok()?;
                    let groups = mention_groups(&snapshot, mentions);
                    let mut listing = Listing::new(groups);
                    listing.snippets =
                        links_data::snippets(&snapshot, &source, &listing.keys(), false);
                    Some(listing)
                })
                .await;
            let elapsed = started.elapsed();
            let Some(listing) = found else {
                return;
            };
            this.update(cx, |this, cx| {
                if this.job == job {
                    this.timings.mentions = Some(elapsed);
                    this.mentions = Some(listing);
                    this.rebuild_rows();
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Lays the lists out again from the data and the folds.
    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        if self.backlinks_open {
            match self.phase {
                Phase::NoDocument => rows.push(Row::Message("No document")),
                Phase::NoVault => rows.push(Row::Message("Not in an indexed folder")),
                Phase::Indexing => rows.push(Row::Message("Indexing…")),
                Phase::Ready if self.generation.is_none() || self.findings.is_none() => {
                    rows.push(Row::Message("Looking for links…"));
                }
                Phase::Ready => {
                    if self.backlinks.groups.is_empty() {
                        rows.push(Row::Message("No backlinks"));
                    }
                    for (g, group) in self.backlinks.groups.iter().enumerate() {
                        rows.push(Row::Head(g));
                        if !self.folded.contains(&group.source) {
                            rows.extend((0..group.items.len()).map(|i| Row::Line(g, i)));
                        }
                    }
                    rows.push(Row::MentionsHead);
                    if self.mentions_open
                        && let Some(mentions) = &self.mentions
                    {
                        for (g, group) in mentions.groups.iter().enumerate() {
                            rows.push(Row::MentionHead(g));
                            if !self.folded_mentions.contains(&group.source) {
                                rows.extend((0..group.items.len()).map(|i| Row::MentionLine(g, i)));
                            }
                        }
                    }
                }
            }
        }
        let mut out_rows = Vec::new();
        if self.outgoing_open {
            match self.phase {
                Phase::Ready if !self.outgoing.is_empty() => {
                    out_rows.extend((0..self.outgoing.len()).map(Row::Out));
                }
                Phase::Ready => out_rows.push(Row::Message("No links")),
                Phase::NoDocument => out_rows.push(Row::Message("No document")),
                Phase::NoVault => out_rows.push(Row::Message("Not in an indexed folder")),
                Phase::Indexing => out_rows.push(Row::Message("Indexing…")),
            }
        }
        if rows != self.rows {
            self.backlinks_list.reset(rows.len());
            self.rows = rows;
            // A reset scrolls to the top: keep the selected row in view.
            if let Some(ix) = self
                .selected
                .and_then(|s| self.rows.iter().position(|r| *r == s))
            {
                self.backlinks_list.scroll_to_reveal_item(ix);
            }
        }
        if out_rows != self.out_rows {
            self.outgoing_list.reset(out_rows.len());
            self.out_rows = out_rows;
        }
        if let Some(selected) = self.selected
            && !self.rows.contains(&selected)
            && !self.out_rows.contains(&selected)
        {
            self.selected = None;
        }
    }

    // ----- Keyboard ---------------------------------------------------------

    /// Rows the keyboard moves through, in order.
    fn selectable(&self) -> Vec<Row> {
        self.rows
            .iter()
            .chain(&self.out_rows)
            .copied()
            .filter(|r| !matches!(r, Row::Message(_)))
            .collect()
    }

    fn select_step(&mut self, step: isize, cx: &mut Context<'_, Self>) {
        let rows = self.selectable();
        if rows.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|s| rows.iter().position(|r| *r == s));
        let next = match (at, step) {
            (None, s) if s > 0 => 0,
            (None, _) => rows.len() - 1,
            (Some(at), s) => at.saturating_add_signed(s).min(rows.len() - 1),
        };
        self.select(rows[next], cx);
    }

    fn select(&mut self, row: Row, cx: &mut Context<'_, Self>) {
        self.selected = Some(row);
        if let Some(ix) = self.rows.iter().position(|r| *r == row) {
            self.backlinks_list.scroll_to_reveal_item(ix);
        } else if let Some(ix) = self.out_rows.iter().position(|r| *r == row) {
            self.outgoing_list.scroll_to_reveal_item(ix);
        }
        cx.notify();
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.select_step(-1, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.select_step(1, cx);
    }

    /// Left folds the selected source, or selects the source of a line.
    fn collapse(&mut self, _: &CollapseOrParent, _: &mut Window, cx: &mut Context<'_, Self>) {
        match self.selected {
            Some(Row::Head(g)) if !self.is_folded(false, g) => self.toggle_group(false, g, cx),
            Some(Row::MentionHead(g)) if !self.is_folded(true, g) => {
                self.toggle_group(true, g, cx);
            }
            Some(Row::MentionsHead) if self.mentions_open => self.toggle_mentions(cx),
            Some(Row::Line(g, _)) => self.select(Row::Head(g), cx),
            Some(Row::MentionLine(g, _)) => self.select(Row::MentionHead(g), cx),
            Some(Row::MentionHead(_)) => self.select(Row::MentionsHead, cx),
            _ => {}
        }
    }

    /// Right unfolds the selected source.
    fn expand(&mut self, _: &ExpandOrChild, _: &mut Window, cx: &mut Context<'_, Self>) {
        match self.selected {
            Some(Row::Head(g)) if self.is_folded(false, g) => self.toggle_group(false, g, cx),
            Some(Row::MentionHead(g)) if self.is_folded(true, g) => self.toggle_group(true, g, cx),
            Some(Row::MentionsHead) if !self.mentions_open => self.toggle_mentions(cx),
            _ => {}
        }
    }

    /// Enter opens the selected line, or folds or unfolds the selected
    /// source.
    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(row) = self.selected {
            self.activate(row, cx);
        }
    }

    fn is_folded(&self, mentions: bool, group: usize) -> bool {
        let (listing, folded) = if mentions {
            (self.mentions.as_ref(), &self.folded_mentions)
        } else {
            (Some(&self.backlinks), &self.folded)
        };
        listing
            .and_then(|l| l.groups.get(group))
            .is_some_and(|g| folded.contains(&g.source))
    }

    /// What a click or Enter on `row` does.
    fn activate(&mut self, row: Row, cx: &mut Context<'_, Self>) {
        match row {
            Row::Message(_) => {}
            Row::Head(g) => self.toggle_group(false, g, cx),
            Row::MentionHead(g) => self.toggle_group(true, g, cx),
            Row::MentionsHead => self.toggle_mentions(cx),
            Row::Line(g, i) => {
                if let Some((group, item)) = self
                    .backlinks
                    .groups
                    .get(g)
                    .and_then(|group| Some((group, group.items.get(i)?)))
                {
                    cx.emit(LinksPanelEvent::Open {
                        path: group.source.clone(),
                        offset: Some(item.span.start),
                    });
                }
            }
            Row::MentionLine(g, i) => {
                if let Some((group, item)) = self
                    .mentions
                    .as_ref()
                    .and_then(|m| m.groups.get(g))
                    .and_then(|group| Some((group, group.items.get(i)?)))
                {
                    cx.emit(LinksPanelEvent::Open {
                        path: group.source.clone(),
                        offset: Some(item.span.start),
                    });
                }
            }
            Row::Out(i) => {
                let (Some(link), Some(note)) = (self.outgoing.get(i), self.target.clone()) else {
                    return;
                };
                let event = match &link.target {
                    Some(target) if link.status.is_resolved() => LinksPanelEvent::Open {
                        path: target.clone(),
                        offset: None,
                    },
                    _ => LinksPanelEvent::Open {
                        path: note,
                        offset: Some(link.span.start),
                    },
                };
                cx.emit(event);
            }
        }
    }

    fn click(&mut self, row: Row, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.focus.focus(window, cx);
        self.selected = Some(row);
        self.activate(row, cx);
        cx.notify();
    }

    /// The Link button of an unlinked mention.
    fn link_mention(&mut self, group: usize, item: usize, cx: &mut Context<'_, Self>) {
        if !self.can_link {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some((group, item)) = self
            .mentions
            .as_ref()
            .and_then(|m| m.groups.get(group))
            .and_then(|g| Some((g, g.items.get(item)?)))
        else {
            return;
        };
        let Some(term) = item.term.clone() else {
            return;
        };
        let unique_stem = self
            .vault
            .as_ref()
            .and_then(|v| v.read(cx).content())
            .is_none_or(|s| links_data::stem_is_unique(&s, &target));
        cx.emit(LinksPanelEvent::Link(LinkRequest {
            source: group.source.clone(),
            span: item.span.clone(),
            term,
            target,
            unique_stem,
        }));
    }

    // ----- Tests ------------------------------------------------------------

    /// Backlinks as `(source title, [context line])`, for tests.
    #[cfg(test)]
    pub fn backlink_rows(&self) -> Vec<(String, Vec<Option<String>>)> {
        listing_rows(&self.backlinks)
    }

    /// Unlinked mentions like [`Self::backlink_rows`], once found.
    #[cfg(test)]
    pub fn mention_rows(&self) -> Option<Vec<(String, Vec<Option<String>>)>> {
        self.mentions.as_ref().map(listing_rows)
    }

    #[cfg(test)]
    pub fn backlink_count(&self) -> usize {
        self.backlinks.count
    }

    #[cfg(test)]
    pub fn outgoing_labels(&self) -> Vec<(String, Option<Severity>)> {
        self.outgoing
            .iter()
            .map(|o| (o.label.to_string(), o.severity))
            .collect()
    }

    #[cfg(test)]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    #[cfg(test)]
    pub fn timings(&self) -> Timings {
        self.timings
    }

    /// Whether the last query has its answer, context lines included.
    #[cfg(test)]
    pub fn settled(&self) -> bool {
        self.phase == Phase::Ready
            && self.generation.is_some()
            && self.findings.is_some()
            && self
                .backlinks
                .snippets
                .iter()
                .all(|g| g.iter().all(Option::is_some))
    }

    /// The selected row's debug selector.
    #[cfg(test)]
    pub fn selected_selector(&self) -> Option<String> {
        self.selected.map(Row::selector)
    }

    // ----- Drawing ----------------------------------------------------------

    /// What assistive technology calls `row`.
    fn row_name(&self, row: Row) -> String {
        let head = |listing: Option<&Listing>, g: usize, what: &str| {
            listing
                .and_then(|l| l.groups.get(g))
                .map(|group| format!("{}, {} {what}", group.title, group.items.len()))
                .unwrap_or_default()
        };
        let line = |listing: Option<&Listing>, g: usize, i: usize| {
            listing
                .and_then(|l| l.snippets.get(g))
                .and_then(|s| s.get(i))
                .and_then(Option::as_ref)
                .map(|snippet| snippet.text.trim().to_owned())
                .unwrap_or_default()
        };
        match row {
            Row::Message(note) => note.to_owned(),
            Row::Head(g) => head(Some(&self.backlinks), g, "links"),
            Row::MentionHead(g) => head(self.mentions.as_ref(), g, "mentions"),
            Row::Line(g, i) => line(Some(&self.backlinks), g, i),
            Row::MentionLine(g, i) => line(self.mentions.as_ref(), g, i),
            Row::MentionsHead => "Unlinked mentions".to_owned(),
            Row::Out(i) => self
                .outgoing
                .get(i)
                .map(|link| match &link.target_name {
                    Some(target) => format!("{} to {target}", link.label),
                    None => format!("{}, not found", link.label),
                })
                .unwrap_or_default(),
        }
    }

    fn render_row(&mut self, ix: usize, outgoing: bool, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = Appearance::theme(cx);
        let rows = if outgoing { &self.out_rows } else { &self.rows };
        let Some(&row) = rows.get(ix) else {
            return div().into_any_element();
        };
        let selected = self.selected == Some(row);
        let body = match row {
            Row::Message(note) => return message(note, theme).into_any_element(),
            Row::Head(g) => self.render_head(&self.backlinks, &self.folded, g, theme),
            Row::MentionHead(g) => match &self.mentions {
                Some(m) => self.render_head(m, &self.folded_mentions, g, theme),
                None => div(),
            },
            Row::Line(g, i) => self.render_line(&self.backlinks, g, i, theme),
            Row::MentionLine(g, i) => match &self.mentions {
                Some(m) => self
                    .render_line(m, g, i, theme)
                    .child(self.link_button(g, i, theme, cx)),
                None => div(),
            },
            Row::MentionsHead => self.render_mentions_head(theme),
            Row::Out(i) => self.render_outgoing(i, theme),
        };
        let line = matches!(row, Row::Line(..) | Row::MentionLine(..));
        let hover = theme.state.hover;
        let selector = row.selector();
        let frame = body
            .id(SharedString::from(selector.clone()))
            .role(match row {
                Row::Head(_) | Row::MentionHead(_) | Row::MentionsHead => Role::TreeItem,
                _ => Role::ListItem,
            })
            .aria_label(self.row_name(row))
            .aria_selected(selected)
            .when(selected, |r| r.aria_active_descendant())
            .debug_selector(move || selector.clone())
            .rounded(px(radius::CONTROL))
            .when(selected, |row| row.bg(theme.accent.tint))
            .when(!selected, |row| row.hover(move |style| style.bg(hover)))
            .on_click(cx.listener(move |this, _, window, cx| this.click(row, window, cx)));
        // A full-width slot holds the row and its side margins.
        div()
            .w_full()
            .px(px(size::LIST_ROW_INSET))
            .when(line, |slot| slot.pb(px(spacing::LINE_GAP)))
            .child(frame)
            .into_any_element()
    }

    /// A source note: icon, title, and the heading its links are under (or,
    /// folded or under several headings, how many there are).
    fn render_head(
        &self,
        listing: &Listing,
        folded: &HashSet<VaultPath>,
        g: usize,
        theme: &Theme,
    ) -> Div {
        let Some(group) = listing.groups.get(g) else {
            return div();
        };
        let right: Option<SharedString> = if folded.contains(&group.source) {
            Some(group.items.len().to_string().into())
        } else {
            match group.shared_heading() {
                Some(heading) => Some(heading.to_string().into()),
                None if group.items.len() > 1 => Some(group.items.len().to_string().into()),
                None => None,
            }
        };
        div()
            .h(px(size::OUTLINE_ROW))
            .flex()
            .items_center()
            .gap(px(spacing::HEAD_GAP))
            .px(px(spacing::HEAD_PAD_X))
            .text_color(theme.text.primary)
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text.secondary)
                    .child(Icon::Note.sized(icon::ROW)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight(text::weight::ACTIVE))
                    .child(group.title.clone()),
            )
            .when_some(right, |row, right| {
                row.child(
                    div()
                        .flex_none()
                        .max_w(px(spacing::HEADING_MAX_WIDTH))
                        .truncate()
                        .text_size(px(text::UI_SMALL.size))
                        .text_color(theme.text.secondary)
                        .child(right),
                )
            })
    }

    /// A context line with the link (or the mention) marked like a search
    /// match, and its label ("embed", "alias").
    fn render_line(&self, listing: &Listing, g: usize, i: usize, theme: &Theme) -> Div {
        let item = listing.groups.get(g).and_then(|group| group.items.get(i));
        let snippet = listing
            .snippets
            .get(g)
            .and_then(|s| s.get(i))
            .and_then(Option::as_ref);
        let note = item.and_then(|i| i.note);
        let content: AnyElement = match snippet {
            Some(snippet) => {
                let mark = HighlightStyle {
                    color: Some(theme.text.primary.into()),
                    background_color: Some(theme.accent.tint.into()),
                    ..HighlightStyle::default()
                };
                let mut text = snippet.text.to_string();
                if let Some(note) = note {
                    text.push_str(" (");
                    text.push_str(note);
                    text.push(')');
                }
                StyledText::new(text)
                    .with_highlights([(snippet.highlight.clone(), mark)])
                    .into_any_element()
            }
            // Not read yet: an empty line keeps the row's height.
            None => div().h(px(text::UI_SECTION.line)).into_any_element(),
        };
        div()
            .flex()
            .items_start()
            .pt(px(spacing::LINE_PAD_TOP))
            .pb(px(spacing::LINE_PAD_BOTTOM))
            .pl(px(spacing::LINE_PAD_LEFT))
            .pr(px(spacing::HEAD_PAD_X))
            .text_size(px(text::UI_SECTION.size))
            .line_height(px(text::UI_SECTION.line))
            .text_color(theme.text.secondary)
            .child(div().flex_1().min_w_0().child(content))
    }

    /// The Link button of an unlinked mention (a quiet button).
    fn link_button(&self, g: usize, i: usize, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let enabled = self.can_link;
        let hover = theme.state.press;
        let button = div()
            .id(SharedString::from(format!("mention-link-{g}-{i}")))
            .debug_selector(move || format!("mention-link-{g}-{i}"))
            .flex_none()
            .ml(px(spacing::HEAD_GAP))
            .h(px(text::UI_SECTION.line))
            .px(px(spacing::BUTTON_PAD_X))
            .flex()
            .items_center()
            .rounded(px(radius::SMALL_CONTROL))
            .bg(theme.state.hover)
            .text_size(px(text::UI_SMALL.size))
            .font_weight(FontWeight(text::weight::ACTIVE))
            .text_color(theme.text.primary)
            .child("Link")
            .map(|b| {
                if enabled {
                    b.hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.link_mention(g, i, cx);
                        }))
                } else {
                    b.opacity(0.5)
                }
            });
        div().flex_none().child(button)
    }

    /// The folded row of unlinked mentions, with their count once known.
    fn render_mentions_head(&self, theme: &Theme) -> Div {
        let chevron = if self.mentions_open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        div()
            .h(px(size::OUTLINE_ROW))
            .flex()
            .items_center()
            .gap(px(spacing::HEAD_GAP))
            .px(px(spacing::HEAD_PAD_X))
            .text_color(theme.text.secondary)
            .child(div().flex_none().child(chevron.sized(icon::CHEVRON)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child("Unlinked mentions"),
            )
            .when_some(self.mentions.as_ref(), |row, m| {
                row.child(div().flex_none().child(m.count.to_string()))
            })
    }

    /// A link of the active note: severity icon, its text, its target.
    fn render_outgoing(&self, i: usize, theme: &Theme) -> Div {
        let Some(link) = self.outgoing.get(i) else {
            return div();
        };
        let target: SharedString = match (&link.target_name, link.severity) {
            (Some(name), _) => name.clone(),
            (None, Some(Severity::Error)) => "Not found".into(),
            (None, _) => SharedString::default(),
        };
        div()
            .h(px(size::OUTLINE_ROW))
            .flex()
            .items_center()
            .gap(px(spacing::HEAD_GAP))
            .px(px(spacing::HEAD_PAD_X))
            .text_color(theme.text.primary)
            .child(
                div()
                    .flex_none()
                    .w(px(icon::INLINE))
                    .children(severity_icon(link.severity, icon::INLINE, theme)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(link.label.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(spacing::HEADING_MAX_WIDTH))
                    .truncate()
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(theme.text.secondary)
                    .child(target),
            )
    }

    /// A section's list, as tall as its rows (estimated before they are
    /// measured) and shorter when the panel runs out of room.
    fn render_list(&self, outgoing: bool, cx: &mut Context<'_, Self>) -> Div {
        let (rows, state, id) = if outgoing {
            (&self.out_rows, self.outgoing_list.clone(), "outgoing-list")
        } else {
            (&self.rows, self.backlinks_list.clone(), "backlinks-list")
        };
        let height: f32 = rows.iter().map(|r| r.estimate()).sum();
        div()
            .debug_selector(move || id.into())
            .h(px(height))
            .min_h_0()
            .flex_shrink_1()
            .mb(px(spacing::SECTION_PAD_BOTTOM))
            .child(
                list(
                    state,
                    cx.processor(move |this, ix: usize, _, cx| this.render_row(ix, outgoing, cx)),
                )
                .size_full(),
            )
    }
}

impl Render for LinksPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let count =
            (self.phase == Phase::Ready && self.findings.is_some()).then_some(self.backlinks.count);
        let findings = self.findings.filter(|_| self.phase == Phase::Ready);
        div()
            .id("links-panel")
            .role(gpui::Role::Group)
            .aria_label("Links")
            .debug_selector(|| "links-panel".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::collapse))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::open_selected))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus.focus(window, cx)),
            )
            // The lists give way when the panel runs out of room; the two
            // heads stay.
            .min_h(px(2.0 * (size::PANEL_HEADER + HAIRLINE)))
            .flex_shrink_1()
            .flex()
            .flex_col()
            .child(
                section_head(
                    "backlinks-head",
                    "Backlinks",
                    count,
                    self.backlinks_open,
                    true,
                    theme,
                )
                .debug_selector(|| "backlinks-head".into())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_backlinks(cx))),
            )
            .when(self.backlinks_open, |panel| {
                panel.child(self.render_list(false, cx))
            })
            .child(
                section_head(
                    "outgoing-links-head",
                    "Links in this note",
                    None,
                    self.outgoing_open,
                    true,
                    theme,
                )
                .debug_selector(|| "outgoing-links-head".into())
                .when_some(findings, |head, f| {
                    head.child(findings_counts(f, true, spacing::COUNT_GROUP_GAP, theme))
                })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_outgoing(cx))),
            )
            .when(self.outgoing_open, |panel| {
                panel.child(self.render_list(true, cx))
            })
    }
}

/// Error and warning icons with their counts; with `total`, then the
/// number of links.
fn findings_counts(f: Findings, total: bool, group_gap: f32, theme: &Theme) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(spacing::COUNT_GAP))
        .font_weight(FontWeight(text::weight::ACTIVE))
        .when(f.errors > 0, |row| {
            row.children(severity_icon(Some(Severity::Error), icon::SMALL, theme))
                .child(f.errors.to_string())
        })
        .when(f.warnings > 0, |row| {
            row.child(
                div()
                    .flex_none()
                    .when(f.errors > 0, |d| d.ml(px(group_gap)))
                    .children(severity_icon(Some(Severity::Warning), icon::SMALL, theme)),
            )
            .child(f.warnings.to_string())
        })
        .when(total, |row| {
            row.child(
                div()
                    .when(f.errors + f.warnings > 0, |d| {
                        d.ml(px(spacing::COUNT_TOTAL_GAP))
                    })
                    .child(f.links.to_string()),
            )
        })
}

fn severity_icon(severity: Option<Severity>, size: f32, theme: &Theme) -> Option<Div> {
    let (icon, color) = match severity? {
        Severity::Error => (Icon::NoticeError, theme.status.error),
        Severity::Warning => (Icon::NoticeWarning, theme.status.warning),
    };
    Some(
        div()
            .flex_none()
            .flex()
            .items_center()
            .text_color(color)
            .child(icon.sized(size)),
    )
}

/// A quiet one-line state where the rows would be (not designed).
fn message(note: &'static str, theme: &Theme) -> Div {
    div()
        .debug_selector(move || format!("links-message:{note}"))
        .w_full()
        .h(px(size::OUTLINE_ROW))
        .flex()
        .items_center()
        .pl(px(size::LIST_ROW_INSET + spacing::HEAD_PAD_X))
        .text_color(theme.text.secondary)
        .child(note)
}

#[cfg(test)]
fn listing_rows(listing: &Listing) -> Vec<(String, Vec<Option<String>>)> {
    listing
        .groups
        .iter()
        .zip(&listing.snippets)
        .map(|(g, s)| {
            (
                g.title.to_string(),
                s.iter()
                    .zip(&g.items)
                    .map(|(s, item)| {
                        s.as_ref().map(|s| {
                            let h = s.highlight.clone();
                            let note = item.note.map(|n| format!(" ({n})")).unwrap_or_default();
                            format!(
                                "{}[{}]{}{note}",
                                &s.text[..h.start],
                                &s.text[h.clone()],
                                &s.text[h.end..]
                            )
                        })
                    })
                    .collect(),
            )
        })
        .collect()
}

/// `file` as a path in the vault at `root`.
fn vault_path(root: &Path, file: &Path) -> Option<VaultPath> {
    let relative = file.strip_prefix(root).ok()?;
    VaultPath::from_relative_fs_path(relative).ok()
}

fn disk_source(root: &Path) -> DiskSource {
    DiskSource::new(root, &ContentOptions::for_profile(ProfileId::Extended))
}

impl Workspace {
    /// Keeps the links sections in step with the window: creates them once
    /// the window has a vault or shows the right panel (Backlinks needs the
    /// vault, so showing the panel creates it), and tells them the active
    /// file.
    pub(super) fn sync_links(&mut self, cx: &mut Context<'_, Self>) {
        if self.panel_open && self.vault.is_none() {
            // Calls back here through `vault_changed`.
            self.ensure_vault(cx);
            if self.vault.is_some() {
                return;
            }
        }
        if self.links.is_none() && (self.vault.is_some() || self.panel_open) {
            let links = cx.new(LinksPanel::new);
            let handle = self.window_handle;
            self._links_events = Some(cx.subscribe(
                &links,
                move |_, _, event: &LinksPanelEvent, cx| {
                    let event = event.clone();
                    let this = cx.weak_entity();
                    cx.defer(move |cx| {
                        let _ = cx.update_window(handle, |_, window, cx| {
                            this.update(cx, |ws, cx| ws.links_event(event, window, cx))
                                .ok();
                        });
                    });
                },
            ));
            self.links = Some(links);
        }
        let Some(links) = self.links.clone() else {
            return;
        };
        let vault = self.vault.clone();
        let file = self.active_tab().path.clone();
        let visible = self.panel_open;
        let can_link = !AppSettings::viewer_mode(cx);
        links.update(cx, |l, cx| {
            l.set_context(vault, file, visible, can_link, cx)
        });
        self.sync_link_checker(cx);
        self.sync_base_panel(cx);
    }

    fn links_event(
        &mut self,
        event: LinksPanelEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(root) = self.vault.as_ref().map(|v| v.read(cx).root().to_path_buf()) else {
            return;
        };
        match event {
            LinksPanelEvent::Open { path, offset } => {
                let file = path.to_fs_path(&root);
                let scroll = offset.map(ScrollTarget::Offset);
                self.navigate(self.active, file, scroll, window, cx);
                self.focus_document(window, cx);
            }
            LinksPanelEvent::Link(request) => self.link_mention(&root, request, window, cx),
        }
    }

    /// Writes the link of an unlinked mention into its source's buffer,
    /// opening the source in a background tab when it is not open. The
    /// edit is one undo step of that tab.
    fn link_mention(
        &mut self,
        root: &Path,
        request: LinkRequest,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let file = request.source.to_fs_path(root);
        let open = |ws: &Self| {
            ws.tabs
                .iter()
                .position(|t| t.path.as_deref() == Some(&file))
        };
        let index = match open(self) {
            Some(index) => index,
            None => {
                self.insert_tab(OpenTarget::new(file.clone()), None, false, window, cx);
                let Some(index) = open(self) else {
                    return;
                };
                index
            }
        };
        if !self.can_edit(index, cx) {
            return;
        }
        let wikilinks = self.tab_profile(index, cx).compiled.switches.wikilinks;
        let Some(editor) = self.ensure_editor(index, cx) else {
            return;
        };
        let reader = self.tabs[index].reader.clone();
        let write = move |editor: &Entity<crate::editor::EditorView>, cx: &mut App| {
            let LinkRequest {
                source,
                span,
                term,
                target,
                unique_stem,
            } = &request;
            let changed = editor.update(cx, |e, cx| {
                e.apply_checked_replace(
                    span.clone(),
                    term,
                    |found| {
                        links_data::link_for_mention(found, source, target, wikilinks, *unique_stem)
                    },
                    cx,
                )
            });
            if changed
                && let Some(reader) = &reader
                && let Some(text) = editor.update(cx, |e, _| e.take_preview_text())
            {
                reader.update(cx, |r, cx| r.show_edited(text, cx));
            }
        };
        if editor.read(cx).editor().is_some() {
            write(&editor, cx);
            return;
        }
        // The Source view is loading its file: write once it is in.
        let pending = std::rc::Rc::new(std::cell::Cell::new(Some(write)));
        cx.observe(&editor, move |_, editor, cx| {
            if editor.read(cx).editor().is_some()
                && let Some(write) = pending.take()
            {
                write(&editor, cx);
            }
        })
        .detach();
    }

    /// The status bar's link findings of the active note: in Preview, when
    /// it has any.
    pub(super) fn link_findings(&self, cx: &App) -> Option<AnyElement> {
        if self.mode() != ViewMode::Preview {
            return None;
        }
        let findings = self.links.as_ref()?.read(cx).findings()?;
        if findings.errors + findings.warnings == 0 {
            return None;
        }
        let theme = Appearance::theme(cx);
        Some(
            findings_counts(findings, false, spacing::STATUS_GROUP_GAP, theme)
                .id("status-link-findings")
                .debug_selector(|| "status-link-findings".into())
                .font_weight(FontWeight(text::weight::REGULAR))
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(ShowOutgoingLinks), cx);
                })
                .into_any_element(),
        )
    }

    /// The links sections, for tests.
    #[cfg(test)]
    pub(super) fn links_panel(&self) -> Option<Entity<LinksPanel>> {
        self.links.clone()
    }
}
