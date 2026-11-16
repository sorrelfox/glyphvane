// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Link checker section of the right panel (FR-NAV-12).
//!
//! - **Scope:** a Note / Folder / Vault segmented control. Note is the
//!   active note, Folder its folder and subfolders, Vault every note. The
//!   `links.check_note`, `links.check_folder` and `links.check_vault`
//!   commands show the section with that scope.
//! - **Summary:** an icon and a count per severity; while a folder or the
//!   vault is checked, how many notes are done.
//! - **Findings,** grouped by severity, then by source note: the kind with
//!   the line at the right, the link as written (mono), and the message. A
//!   click selects a finding and opens its note at the link; the selected
//!   finding lists its fixes below it.
//! - **Fixes** (`links.fix`, and `links.create_note` for the selected
//!   finding): a letter-case fix, one rewrite per candidate of an ambiguous
//!   link, the note an alias belongs to, and *Create note* for an
//!   unresolved link. A rewrite is one undoable edit in the source's
//!   buffer (the source opens in a background tab when it is not open);
//!   *Create note* creates the empty note and opens it.
//!
//! The check runs off the window's thread on the content index's snapshot
//! ([`gv_vault::content::check`]) and runs again when the index changes,
//! after [`RECHECK_DELAY`].
//!
//! Not designed (built from the Backlinks rows and the mode switch): the
//! severity and note rows, the progress text, the fix buttons, the empty
//! states and what a click does.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, ListAlignment, ListState, MouseButton,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, list, px, transparent_black,
};
use gv_vault::content::check::rewrite_link;
use gv_vault::content::{
    CheckOptions, CheckProgress, CheckReport, CheckScope, ContentOptions, ContentSnapshot,
    DiskSource, Finding, Fix, FixAction, Severity,
};
use gv_vault::profile::ProfileId;
use gv_vault::{EntryKind, FileMeta, VaultPath};

use super::files_panel::{KEY_CONTEXT, OpenSelected, SelectNext, SelectPrevious};
use super::links_panel::Phase;
use super::panels::{RightSection, section_head};
use super::vault::{ContentUpdated, VaultStatus, WindowVault};
use super::{OpenTarget, Workspace};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::keymap::actions::links as commands;
use crate::metrics::spacing::{backlinks, mode_switch};
use crate::metrics::{DISABLED_OPACITY, HAIRLINE, font, icon, radius, size, text};
use crate::reader::ScrollTarget;
use crate::settings::AppSettings;
use crate::theme::Theme;

/// How long a re-check waits after the index changed, for more changes.
pub(super) const RECHECK_DELAY: Duration = Duration::from_millis(300);

/// How often the progress of a running check is drawn.
const PROGRESS_TICK: Duration = Duration::from_millis(100);

/// SPEC § Right panel: the summary row is 30 high; a finding has padding
/// `5 8 6`, margin `0 6` and radius 5. Not designed: the segmented
/// control's margins, the gap between a finding's lines.
mod spacing {
    pub const SUMMARY: f32 = 30.0;
    pub const SUMMARY_PAD_X: f32 = 10.0;
    pub const FINDING_PAD_TOP: f32 = 5.0;
    pub const FINDING_PAD_X: f32 = 8.0;
    pub const FINDING_PAD_BOTTOM: f32 = 6.0;
    pub const FINDING_GAP: f32 = 5.0;
    pub const SCOPE_MARGIN_X: f32 = 10.0;
    pub const SCOPE_MARGIN_Y: f32 = 4.0;
    pub const FIX_PAD_LEFT: f32 = 26.0;
    /// The kind (12/16), the link (11/14) and the message (11/14).
    pub const FINDING_HEIGHT: f32 = FINDING_PAD_TOP + 16.0 + 14.0 + 14.0 + FINDING_PAD_BOTTOM;
}

/// What the scope control picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ScopeKind {
    #[default]
    Note,
    Folder,
    Vault,
}

impl ScopeKind {
    const ALL: [Self; 3] = [Self::Note, Self::Folder, Self::Vault];

    fn label(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Folder => "Folder",
            Self::Vault => "Vault",
        }
    }
}

/// What the section asks of its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CheckerEvent {
    /// Open `path` at the byte offset.
    Open { path: VaultPath, offset: usize },
    /// Apply `fix` to `finding`.
    Fix { finding: Box<Finding>, fix: Fix },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Message(&'static str),
    Severity(Severity),
    /// A source note: its group.
    Source(usize),
    Finding(usize),
    /// A fix of the selected finding.
    Fix(usize, usize),
}

impl Row {
    fn estimate(self) -> f32 {
        match self {
            Self::Finding(_) => spacing::FINDING_HEIGHT,
            _ => size::OUTLINE_ROW,
        }
    }

    fn selector(self) -> String {
        match self {
            Self::Message(m) => format!("checker-message:{m}"),
            Self::Severity(s) => format!("checker-severity-{}", s.name()),
            Self::Source(g) => format!("checker-source-{g}"),
            Self::Finding(i) => format!("checker-finding-{i}"),
            Self::Fix(i, k) => format!("checker-fix-{i}-{k}"),
        }
    }
}

/// The findings of one source note with one severity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Group {
    severity: Severity,
    source: VaultPath,
    findings: Range<usize>,
}

/// The Link checker section.
pub(crate) struct LinkChecker {
    focus: FocusHandle,
    vault: Option<Entity<WindowVault>>,
    _vault_events: Vec<Subscription>,
    file: Option<PathBuf>,
    visible: bool,
    can_fix: bool,
    open: bool,
    scope: ScopeKind,
    phase: Phase,
    /// The scope and index generation the findings are for.
    checked: Option<(CheckScope, u64)>,
    report: Option<CheckReport>,
    groups: Vec<Group>,
    running: Option<Arc<CheckProgress>>,
    cancel: Arc<AtomicBool>,
    job: u64,
    _task: Option<Task<()>>,
    selected: Option<Row>,
    rows: Vec<Row>,
    list: ListState,
    elapsed: Option<Duration>,
}

impl EventEmitter<CheckerEvent> for LinkChecker {}

impl Focusable for LinkChecker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl LinkChecker {
    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        let mut this = Self {
            focus: cx.focus_handle(),
            vault: None,
            _vault_events: Vec::new(),
            file: None,
            visible: false,
            can_fix: true,
            open: true,
            scope: ScopeKind::default(),
            phase: Phase::NoDocument,
            checked: None,
            report: None,
            groups: Vec::new(),
            running: None,
            cancel: Arc::default(),
            job: 0,
            _task: None,
            selected: None,
            rows: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(200.0)),
            elapsed: None,
        };
        this.rebuild_rows();
        this
    }

    /// Follows the window: its vault, the active tab's file, whether the
    /// right panel is shown and whether notes may be edited.
    pub fn set_context(
        &mut self,
        vault: Option<Entity<WindowVault>>,
        file: Option<PathBuf>,
        visible: bool,
        can_fix: bool,
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
            self.clear();
        }
        let changed = vault_changed || self.file != file || self.visible != visible;
        self.file = file;
        self.visible = visible;
        if self.can_fix != can_fix {
            self.can_fix = can_fix;
            cx.notify();
        }
        if changed {
            self.refresh(false, cx);
        }
    }

    /// Checks `scope` (the `links.check_*` commands, the scope control).
    pub fn set_scope(&mut self, scope: ScopeKind, cx: &mut Context<'_, Self>) {
        self.open = true;
        if self.scope != scope {
            self.scope = scope;
            self.selected = None;
        }
        self.refresh(false, cx);
        cx.notify();
    }

    fn clear(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.checked = None;
        self.report = None;
        self.groups.clear();
        self.running = None;
        self.selected = None;
        self._task = None;
        self.rebuild_rows();
    }

    /// Where the vault and its index are, and the scope to check.
    fn phase_and_scope(&self, cx: &App) -> (Phase, Option<(ContentSnapshot, CheckScope, PathBuf)>) {
        let Some(vault) = &self.vault else {
            return (Phase::NoVault, None);
        };
        let vault = vault.read(cx);
        match vault.status() {
            VaultStatus::Building => return (Phase::Indexing, None),
            VaultStatus::NotIndexed(_) | VaultStatus::Failed(_) => return (Phase::NoVault, None),
            VaultStatus::Ready => {}
        }
        let root = vault.root().to_path_buf();
        let note = self.file.as_deref().and_then(|f| vault_path(&root, f));
        let scope = match (self.scope, note) {
            (ScopeKind::Vault, _) => CheckScope::Vault,
            (ScopeKind::Note, Some(note)) => CheckScope::Note(note),
            (ScopeKind::Folder, Some(note)) => match note.parent() {
                Some(folder) if folder != VaultPath::root() => CheckScope::Folder(folder),
                _ => CheckScope::Vault,
            },
            (_, None) => return (Phase::NoDocument, None),
        };
        match vault.content() {
            Some(snapshot) => (Phase::Ready, Some((snapshot, scope, root))),
            None => (Phase::Indexing, None),
        }
    }

    /// Checks again when the scope or the index changed. `content`: the
    /// index published a snapshot (wait [`RECHECK_DELAY`] first).
    fn refresh(&mut self, content: bool, cx: &mut Context<'_, Self>) {
        let (phase, found) = self.phase_and_scope(cx);
        if phase != self.phase {
            self.phase = phase;
            self.rebuild_rows();
            cx.notify();
        }
        let Some((snapshot, scope, root)) = found else {
            if self.checked.is_some() || self.running.is_some() {
                self.clear();
                cx.notify();
            }
            return;
        };
        if !self.visible {
            return;
        }
        let key = (scope.clone(), snapshot.generation());
        if self.checked.as_ref() == Some(&key) {
            return;
        }
        let same_scope = self.checked.as_ref().is_some_and(|(s, _)| *s == scope);
        if !same_scope {
            self.report = None;
            self.groups.clear();
            self.selected = None;
        }
        self.checked = Some(key);
        self.cancel.store(true, Ordering::Release);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let progress = Arc::new(CheckProgress::default());
        self.running = Some(progress.clone());
        self.job += 1;
        let job = self.job;
        let delay = content && same_scope;
        self.rebuild_rows();
        cx.notify();
        self._task = Some(cx.spawn(async move |this, cx| {
            if delay {
                cx.background_executor().timer(RECHECK_DELAY).await;
            }
            let started = Instant::now();
            let work = cx.background_spawn({
                let progress = progress.clone();
                async move {
                    let source =
                        DiskSource::new(&root, &ContentOptions::for_profile(ProfileId::Extended));
                    snapshot
                        .check_links(
                            &scope,
                            &CheckOptions::default(),
                            Some(&source),
                            &progress,
                            &cancel,
                        )
                        .ok()
                }
            });
            // Draw the progress while it runs.
            let ticker = {
                let this = this.clone();
                let cx = cx.clone();
                cx.spawn(async move |cx| {
                    loop {
                        cx.background_executor().timer(PROGRESS_TICK).await;
                        let alive = this
                            .update(cx, |this, cx| {
                                if this.job == job && this.running.is_some() {
                                    cx.notify();
                                    true
                                } else {
                                    false
                                }
                            })
                            .unwrap_or(false);
                        if !alive {
                            break;
                        }
                    }
                })
            };
            let report = work.await;
            drop(ticker);
            let elapsed = started.elapsed();
            this.update(cx, |this, cx| this.checked_done(job, report, elapsed, cx))
                .ok();
        }));
    }

    fn checked_done(
        &mut self,
        job: u64,
        report: Option<CheckReport>,
        elapsed: Duration,
        cx: &mut Context<'_, Self>,
    ) {
        if job != self.job {
            return;
        }
        self.running = None;
        let Some(mut report) = report else {
            return;
        };
        tracing::debug!(?elapsed, findings = report.findings.len(), "link check");
        self.elapsed = Some(elapsed);
        // By severity, then source, then position (the report is by source).
        report.findings.sort_by(|a, b| {
            (a.severity, &a.source, a.span.start, a.kind).cmp(&(
                b.severity,
                &b.source,
                b.span.start,
                b.kind,
            ))
        });
        let selected_finding = self.selected_finding().cloned();
        self.groups = group(&report.findings);
        self.report = Some(report);
        // Keep the selection on the same link when it is still there.
        self.selected = selected_finding
            .and_then(|old| {
                self.report.as_ref()?.findings.iter().position(|f| {
                    f.source == old.source && f.span == old.span && f.kind == old.kind
                })
            })
            .map(Row::Finding);
        self.rebuild_rows();
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        match self.phase {
            Phase::NoDocument => rows.push(Row::Message("No document")),
            Phase::NoVault => rows.push(Row::Message("Not in an indexed folder")),
            Phase::Indexing => rows.push(Row::Message("Indexing…")),
            Phase::Ready => match &self.report {
                None => rows.push(Row::Message("Checking…")),
                Some(r) if r.findings.is_empty() => rows.push(Row::Message("No problems found")),
                Some(_) => {
                    let mut severity = None;
                    for (g, group) in self.groups.iter().enumerate() {
                        if severity != Some(group.severity) {
                            severity = Some(group.severity);
                            rows.push(Row::Severity(group.severity));
                        }
                        rows.push(Row::Source(g));
                        for i in group.findings.clone() {
                            rows.push(Row::Finding(i));
                            if self.selected_index() == Some(i) {
                                let fixes = self.finding(i).map_or(0, |f| f.fixes.len());
                                rows.extend((0..fixes).map(|k| Row::Fix(i, k)));
                            }
                        }
                    }
                }
            },
        }
        if rows != self.rows {
            self.list.reset(rows.len());
            self.rows = rows;
            if let Some(ix) = self
                .selected
                .and_then(|s| self.rows.iter().position(|r| *r == s))
            {
                self.list.scroll_to_reveal_item(ix);
            }
        }
        if let Some(selected) = self.selected
            && !self.rows.contains(&selected)
        {
            self.selected = None;
        }
    }

    fn finding(&self, i: usize) -> Option<&Finding> {
        self.report.as_ref()?.findings.get(i)
    }

    /// The finding selected, or whose fix is selected.
    fn selected_index(&self) -> Option<usize> {
        match self.selected? {
            Row::Finding(i) | Row::Fix(i, _) => Some(i),
            _ => None,
        }
    }

    fn selected_finding(&self) -> Option<&Finding> {
        self.finding(self.selected_index()?)
    }

    /// The fix `links.fix` applies: the selected fix, else the selected
    /// finding's first one (with `create`, its *Create note*).
    pub fn fix_for_command(&self, create: bool) -> Option<(Finding, Fix)> {
        let finding = self.selected_finding()?;
        let fix = match (self.selected, create) {
            (_, true) => finding
                .fixes
                .iter()
                .find(|f| matches!(f.action, FixAction::CreateNote { .. })),
            (Some(Row::Fix(_, k)), false) => finding.fixes.get(k),
            _ => finding.fixes.first(),
        }?;
        Some((finding.clone(), fix.clone()))
    }

    fn select(&mut self, row: Row, cx: &mut Context<'_, Self>) {
        self.selected = Some(row);
        self.rebuild_rows();
        if let Some(ix) = self.rows.iter().position(|r| *r == row) {
            self.list.scroll_to_reveal_item(ix);
        }
        cx.notify();
    }

    fn select_step(&mut self, step: isize, cx: &mut Context<'_, Self>) {
        let rows: Vec<Row> = self
            .rows
            .iter()
            .copied()
            .filter(|r| matches!(r, Row::Finding(_) | Row::Fix(..)))
            .collect();
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

    fn activate(&mut self, row: Row, cx: &mut Context<'_, Self>) {
        match row {
            Row::Finding(i) => {
                if let Some(f) = self.finding(i) {
                    cx.emit(CheckerEvent::Open {
                        path: f.source.clone(),
                        offset: f.span.start as usize,
                    });
                }
            }
            Row::Fix(i, k) => {
                if !self.can_fix {
                    return;
                }
                if let Some(f) = self.finding(i)
                    && let Some(fix) = f.fixes.get(k)
                {
                    cx.emit(CheckerEvent::Fix {
                        finding: Box::new(f.clone()),
                        fix: fix.clone(),
                    });
                }
            }
            Row::Message(_) | Row::Severity(_) | Row::Source(_) => {}
        }
    }

    fn click(&mut self, row: Row, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.focus.focus(window, cx);
        if matches!(row, Row::Finding(_) | Row::Fix(..)) {
            self.select(row, cx);
        }
        self.activate(row, cx);
    }

    /// Expands the section (`workspace.show_link_checker`).
    pub fn expand(&mut self, cx: &mut Context<'_, Self>) {
        if !self.open {
            self.open = true;
            cx.notify();
        }
    }

    fn toggle(&mut self, cx: &mut Context<'_, Self>) {
        self.open = !self.open;
        cx.notify();
    }

    // ----- Tests ------------------------------------------------------------

    /// `(kind, raw, line)` of the findings in row order, for tests.
    #[cfg(test)]
    pub fn finding_rows(&self) -> Vec<(String, String, u32)> {
        self.rows
            .iter()
            .filter_map(|r| match r {
                Row::Finding(i) => self.finding(*i),
                _ => None,
            })
            .map(|f| (f.kind.name().to_owned(), f.raw.to_string(), f.line))
            .collect()
    }

    /// The selectors of the rows, for tests.
    #[cfg(test)]
    pub fn row_selectors(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.selector()).collect()
    }

    /// The last check's report, when done.
    #[cfg(test)]
    pub fn report(&self) -> Option<&CheckReport> {
        self.report.as_ref().filter(|_| self.running.is_none())
    }

    #[cfg(test)]
    pub fn scope(&self) -> ScopeKind {
        self.scope
    }

    #[cfg(test)]
    pub fn checked_scope(&self) -> Option<CheckScope> {
        self.checked.as_ref().map(|(s, _)| s.clone())
    }

    #[cfg(test)]
    pub fn elapsed(&self) -> Option<Duration> {
        self.elapsed
    }

    // ----- Drawing ----------------------------------------------------------

    fn render_scope(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let hover = theme.state.hover;
        div()
            .mx(px(spacing::SCOPE_MARGIN_X))
            .my(px(spacing::SCOPE_MARGIN_Y))
            .h(px(crate::metrics::size::segment::FRAME))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(mode_switch::FRAME_GAP))
            .p(px(mode_switch::FRAME_PAD))
            .border_1()
            .border_color(theme.border.strong)
            .rounded(px(radius::SEGMENT_FRAME))
            .children(ScopeKind::ALL.into_iter().map(|scope| {
                let active = self.scope == scope;
                let label = scope.label();
                div()
                    .id(SharedString::from(format!("checker-scope-{label}")))
                    .debug_selector(move || format!("checker-scope-{label}"))
                    .flex_1()
                    .h(px(crate::metrics::size::segment::INNER))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(radius::SEGMENT))
                    .border_1()
                    .text_size(px(text::UI.size))
                    .map(|seg| {
                        if active {
                            seg.bg(theme.surface.page)
                                .border_color(theme.border.strong)
                                .text_color(theme.text.primary)
                                .font_weight(FontWeight(text::weight::ACTIVE))
                        } else {
                            seg.border_color(transparent_black())
                                .text_color(theme.text.secondary)
                                .hover(move |s| s.bg(hover))
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_scope(scope, cx)))
                    .child(label)
            }))
    }

    fn render_summary(&self, theme: &Theme) -> Div {
        let row = div()
            .debug_selector(|| "checker-summary".into())
            .h(px(spacing::SUMMARY))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(backlinks::COUNT_GAP))
            .px(px(spacing::SUMMARY_PAD_X))
            .text_size(px(text::UI_SMALL.size))
            .text_color(theme.text.secondary);
        if let Some(progress) = &self.running {
            let done = progress.done.load(Ordering::Relaxed);
            let total = progress.total.load(Ordering::Relaxed);
            let note = if total > 1 {
                format!("Checking… {done} of {total} notes")
            } else {
                "Checking…".to_owned()
            };
            return row.child(note);
        }
        let Some(report) = &self.report else {
            return row;
        };
        let mut row = row;
        for severity in [Severity::Error, Severity::Warning, Severity::Info] {
            let n = report.count(severity);
            row = row
                .child(
                    div()
                        .flex_none()
                        .when(severity != Severity::Error, |d| {
                            d.ml(px(backlinks::COUNT_TOTAL_GAP))
                        })
                        .child(severity_icon(severity, icon::SMALL, theme)),
                )
                .child(div().text_color(theme.text.primary).child(n.to_string()));
        }
        row.child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_right()
                .child(format!("{} notes, {} links", report.notes, report.links)),
        )
    }

    fn render_row(&mut self, ix: usize, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = Appearance::theme(cx);
        let Some(&row) = self.rows.get(ix) else {
            return div().into_any_element();
        };
        let selected = self.selected == Some(row);
        let body = match row {
            Row::Message(note) => {
                return div()
                    .debug_selector(move || format!("checker-message:{note}"))
                    .h(px(size::OUTLINE_ROW))
                    .flex()
                    .items_center()
                    .pl(px(size::LIST_ROW_INSET + backlinks::HEAD_PAD_X))
                    .text_color(theme.text.secondary)
                    .child(note)
                    .into_any_element();
            }
            Row::Severity(s) => div()
                .h(px(size::OUTLINE_ROW))
                .flex()
                .items_center()
                .px(px(backlinks::HEAD_PAD_X))
                .text_size(px(text::UI_SMALL.size))
                .font_weight(FontWeight(text::weight::STRONG))
                .text_color(theme.text.secondary)
                .child(match s {
                    Severity::Error => "Errors",
                    Severity::Warning => "Warnings",
                    Severity::Info => "Information",
                }),
            Row::Source(g) => self.render_source(g, theme),
            Row::Finding(i) => self.render_finding(i, theme),
            Row::Fix(i, k) => self.render_fix(i, k, theme),
        };
        let hover = theme.state.hover;
        let selector = row.selector();
        let clickable = matches!(row, Row::Finding(_) | Row::Fix(..));
        let frame = body
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .rounded(px(radius::CONTROL))
            .when(selected, |r| r.bg(theme.accent.tint))
            .when(clickable && !selected, |r| r.hover(move |s| s.bg(hover)))
            .on_click(cx.listener(move |this, _, window, cx| this.click(row, window, cx)));
        div()
            .w_full()
            .px(px(size::LIST_ROW_INSET))
            .child(frame)
            .into_any_element()
    }

    fn render_source(&self, g: usize, theme: &Theme) -> Div {
        let Some(group) = self.groups.get(g) else {
            return div();
        };
        let name = group.source.file_name().unwrap_or("").to_owned();
        let folder = group
            .source
            .parent()
            .map(|p| p.as_str().to_owned())
            .unwrap_or_default();
        div()
            .h(px(size::OUTLINE_ROW))
            .flex()
            .items_center()
            .gap(px(backlinks::HEAD_GAP))
            .px(px(backlinks::HEAD_PAD_X))
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
                    .child(name),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(backlinks::HEADING_MAX_WIDTH))
                    .truncate()
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(theme.text.secondary)
                    .child(folder),
            )
    }

    /// Three lines: the kind with the line at the right, the link as
    /// written, the message.
    fn render_finding(&self, i: usize, theme: &Theme) -> Div {
        let Some(f) = self.finding(i) else {
            return div();
        };
        let raw = if f.link_kind.is_wiki() {
            format!("[[{}]]", f.raw)
        } else {
            f.raw.to_string()
        };
        div()
            .pt(px(spacing::FINDING_PAD_TOP))
            .pb(px(spacing::FINDING_PAD_BOTTOM))
            .px(px(spacing::FINDING_PAD_X))
            .flex()
            .items_start()
            .gap(px(spacing::FINDING_GAP))
            .child(div().flex_none().pt(px(1.5)).child(severity_icon(
                f.severity,
                icon::INLINE,
                theme,
            )))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(backlinks::HEAD_GAP))
                            .text_size(px(text::UI.size))
                            .line_height(px(text::UI.line))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight(text::weight::ACTIVE))
                                    .text_color(theme.text.primary)
                                    .child(f.kind.title()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(text::UI_SMALL.size))
                                    .text_color(theme.text.secondary)
                                    .child(format!("line {}", f.line)),
                            ),
                    )
                    .child(
                        div()
                            .truncate()
                            .font_family(font::EDITOR)
                            .text_size(px(text::UI_SMALL.size))
                            .line_height(px(text::UI_SMALL.line))
                            .text_color(theme.text.primary)
                            .child(raw),
                    )
                    .child(
                        div()
                            .text_size(px(text::UI_SMALL.size))
                            .line_height(px(text::UI_SMALL.line))
                            .text_color(theme.text.secondary)
                            .child(f.message.clone()),
                    ),
            )
    }

    /// A fix of the selected finding: a quiet button.
    fn render_fix(&self, i: usize, k: usize, theme: &Theme) -> Div {
        let Some(fix) = self.finding(i).and_then(|f| f.fixes.get(k)) else {
            return div();
        };
        let icon = match fix.action {
            FixAction::CreateNote { .. } => Icon::NewNote,
            FixAction::Rewrite { .. } => Icon::Edit,
        };
        div()
            .h(px(size::OUTLINE_ROW))
            .flex()
            .items_center()
            .gap(px(backlinks::HEAD_GAP))
            .pl(px(spacing::FIX_PAD_LEFT))
            .pr(px(backlinks::HEAD_PAD_X))
            .text_size(px(text::UI_SMALL.size))
            .font_weight(FontWeight(text::weight::ACTIVE))
            .text_color(theme.text.primary)
            .when(!self.can_fix, |b| b.opacity(DISABLED_OPACITY))
            .child(div().flex_none().child(icon.sized(icon::SMALL)))
            .child(div().flex_1().min_w_0().truncate().child(fix.label.clone()))
    }
}

impl Render for LinkChecker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let count = self
            .report
            .as_ref()
            .filter(|_| self.phase == Phase::Ready)
            .map(|r| r.findings.len());
        let height: f32 = self.rows.iter().map(|r| r.estimate()).sum();
        div()
            .id("link-checker")
            .debug_selector(|| "link-checker".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.select_step(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select_step(1, cx)))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| {
                if let Some(row) = this.selected {
                    this.activate(row, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus.focus(window, cx)),
            )
            .min_h(px(size::PANEL_HEADER + HAIRLINE))
            .flex_shrink_1()
            .flex()
            .flex_col()
            .child(
                section_head(
                    "link-checker-head",
                    "Link checker",
                    count,
                    self.open,
                    true,
                    theme,
                )
                .debug_selector(|| "link-checker-head".into())
                .on_click(cx.listener(|this, _, _, cx| this.toggle(cx))),
            )
            .when(self.open, |panel| {
                panel
                    .child(self.render_scope(theme, cx))
                    .child(self.render_summary(theme))
                    .child(
                        div()
                            .debug_selector(|| "checker-list".into())
                            .h(px(height))
                            .min_h_0()
                            .flex_shrink_1()
                            .mb(px(backlinks::SECTION_PAD_BOTTOM))
                            .child(
                                list(
                                    self.list.clone(),
                                    cx.processor(|this, ix: usize, _, cx| this.render_row(ix, cx)),
                                )
                                .size_full(),
                            ),
                    )
            })
    }
}

fn severity_icon(severity: Severity, size: f32, theme: &Theme) -> Div {
    let (icon, color) = match severity {
        Severity::Error => (Icon::NoticeError, theme.status.error),
        Severity::Warning => (Icon::NoticeWarning, theme.status.warning),
        Severity::Info => (Icon::NoticeInformation, theme.status.info),
    };
    div()
        .flex()
        .items_center()
        .text_color(color)
        .child(icon.sized(size))
}

/// Consecutive findings of one severity and source.
fn group(findings: &[Finding]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (i, f) in findings.iter().enumerate() {
        match groups.last_mut() {
            Some(g) if g.severity == f.severity && g.source == f.source => g.findings.end = i + 1,
            _ => groups.push(Group {
                severity: f.severity,
                source: f.source.clone(),
                findings: i..i + 1,
            }),
        }
    }
    groups
}

/// `file` as a path in the vault at `root`.
fn vault_path(root: &Path, file: &Path) -> Option<VaultPath> {
    let relative = file.strip_prefix(root).ok()?;
    VaultPath::from_relative_fs_path(relative).ok()
}

impl Workspace {
    /// The Link checker, created when first asked for.
    pub(super) fn ensure_link_checker(
        &mut self,
        cx: &mut Context<'_, Self>,
    ) -> Entity<LinkChecker> {
        if let Some(checker) = &self.panels.link_checker {
            return checker.clone();
        }
        let checker = cx.new(LinkChecker::new);
        let handle = self.window_handle;
        self.panels._checker_events = Some(cx.subscribe(
            &checker,
            move |_, _, event: &CheckerEvent, cx| {
                let event = event.clone();
                let this = cx.weak_entity();
                cx.defer(move |cx| {
                    let _ = cx.update_window(handle, |_, window, cx| {
                        this.update(cx, |ws, cx| ws.checker_event(event, window, cx))
                            .ok();
                    });
                });
            },
        ));
        self.panels.link_checker = Some(checker.clone());
        self.sync_link_checker(cx);
        checker
    }

    /// Tells the Link checker the window's vault and active file.
    pub(super) fn sync_link_checker(&mut self, cx: &mut Context<'_, Self>) {
        let Some(checker) = self.panels.link_checker.clone() else {
            return;
        };
        let vault = self.vault.clone();
        let file = self.active_tab().path.clone();
        let visible = self.panel_open;
        let can_fix = !AppSettings::viewer_mode(cx);
        checker.update(cx, |c, cx| c.set_context(vault, file, visible, can_fix, cx));
    }

    /// `links.check_note`, `links.check_folder`, `links.check_vault`.
    fn check_links_command(
        &mut self,
        scope: ScopeKind,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.show_right_section(RightSection::LinkChecker, window, cx);
        let checker = self.ensure_link_checker(cx);
        checker.update(cx, |c, cx| c.set_scope(scope, cx));
    }

    /// `links.fix` and `links.create_note`: the selected finding's fix.
    pub(super) fn fix_command(
        &mut self,
        create: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(checker) = self.panels.link_checker.clone() else {
            return;
        };
        if let Some((finding, fix)) = checker.read(cx).fix_for_command(create) {
            self.apply_link_fix(&finding, &fix, window, cx);
        }
    }

    /// The Link checker's commands, on the window's root.
    pub(super) fn link_checker_actions(
        &self,
        root: gpui::Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> gpui::Stateful<Div> {
        root.on_action(cx.listener(|this, _: &commands::CheckNote, window, cx| {
            this.check_links_command(ScopeKind::Note, window, cx);
        }))
        .on_action(cx.listener(|this, _: &commands::CheckFolder, window, cx| {
            this.check_links_command(ScopeKind::Folder, window, cx);
        }))
        .on_action(cx.listener(|this, _: &commands::CheckVault, window, cx| {
            this.check_links_command(ScopeKind::Vault, window, cx);
        }))
        // `links.fix` and `links.create_note` are in `link_actions`: they
        // act on the link under the pointer or the caret first.
    }

    fn checker_event(
        &mut self,
        event: CheckerEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            CheckerEvent::Open { path, offset } => {
                let Some(root) = self.vault.as_ref().map(|v| v.read(cx).root().to_path_buf())
                else {
                    return;
                };
                let file = path.to_fs_path(&root);
                self.navigate(
                    self.active,
                    file,
                    Some(ScrollTarget::Offset(offset)),
                    window,
                    cx,
                );
                self.focus_document(window, cx);
            }
            CheckerEvent::Fix { finding, fix } => self.apply_link_fix(&finding, &fix, window, cx),
        }
    }

    /// Applies a fix: a rewrite of the link in its source's buffer (one
    /// undo step), or a new note.
    pub(super) fn apply_link_fix(
        &mut self,
        finding: &Finding,
        fix: &Fix,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if AppSettings::viewer_mode(cx) {
            return;
        }
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let root = vault.read(cx).root().to_path_buf();
        match &fix.action {
            FixAction::Rewrite { path } => {
                let (kind, raw, path) = (finding.link_kind, finding.raw.clone(), path.clone());
                self.edit_source(
                    &root,
                    &finding.source,
                    finding.span.range(),
                    move |construct| rewrite_link(construct, kind, &raw, &path),
                    window,
                    cx,
                );
            }
            FixAction::CreateNote { path } => self.create_linked_note(&vault, path, window, cx),
        }
    }

    /// Replaces `span` of `source`'s buffer with what `make` makes of its
    /// text, opening the source in a background tab when it is not open.
    fn edit_source(
        &mut self,
        root: &Path,
        source: &VaultPath,
        span: Range<usize>,
        make: impl FnOnce(&str) -> Option<String> + 'static,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.edit_path(source.to_fs_path(root), span, make, window, cx);
    }

    /// Replaces `span` of `file`'s buffer with what `make` makes of its
    /// text (one undoable edit), opening the file in a background tab when
    /// it is not open. Nothing changes when the text at `span` is not what
    /// `make` expects.
    pub(super) fn edit_path(
        &mut self,
        file: PathBuf,
        span: Range<usize>,
        make: impl FnOnce(&str) -> Option<String> + 'static,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
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
        let Some(editor) = self.ensure_editor(index, cx) else {
            return;
        };
        let reader = self.tabs[index].reader.clone();
        let write = move |editor: &Entity<crate::editor::EditorView>, cx: &mut App| {
            let changed = editor.update(cx, |e, cx| {
                let Some(construct) = e
                    .editor()
                    .and_then(|ed| ed.text().text_for_range(span.clone()).ok())
                else {
                    return false;
                };
                let Some(new) = make(&construct) else {
                    return false;
                };
                e.apply_checked_replace(span.clone(), &construct, |_| new, cx)
            });
            if !changed {
                tracing::info!("the link changed since it was checked; not fixed");
            }
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

    /// Creates the empty note an unresolved link names (never replacing a
    /// file), tells the vault's index, and opens it for editing.
    fn create_linked_note(
        &mut self,
        vault: &Entity<WindowVault>,
        path: &VaultPath,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let root = vault.read(cx).root().to_path_buf();
        let file = path.to_fs_path(&root);
        let handle = self.window_handle;
        let task = cx.background_spawn({
            let file = file.clone();
            async move {
                if let Some(parent) = file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&file)?;
                std::fs::metadata(&file)
            }
        });
        let path = path.clone();
        let vault = vault.clone();
        cx.spawn(async move |this, cx| {
            let created = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                match created {
                    Ok(meta) => {
                        // The watcher would tell the index too; this is
                        // sooner, and works without one.
                        let index = vault.read(cx).snapshot();
                        if let Ok(next) = index.apply(&[gv_vault::Change::Upsert {
                            path,
                            kind: EntryKind::File,
                            meta: FileMeta {
                                size: meta.len(),
                                mtime: meta.modified().ok(),
                                identity: None,
                            },
                        }]) {
                            vault.update(cx, |v, cx| v.publish(next, cx));
                        }
                        this.update(cx, |ws, cx| {
                            ws.open_tab_for_edit(OpenTarget::new(file), None, window, cx);
                        })
                        .ok();
                    }
                    Err(err) => {
                        tracing::warn!("cannot create {}: {err}", file.display());
                    }
                }
            });
        })
        .detach();
    }

    /// The Link checker, for tests.
    #[cfg(test)]
    pub(super) fn link_checker(&self) -> Option<Entity<LinkChecker>> {
        self.panels.link_checker.clone()
    }
}
