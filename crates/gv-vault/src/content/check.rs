// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The link checker: every link of a note, a folder or the vault, checked
//! against a [`ContentSnapshot`].
//!
//! | Finding | Severity | When |
//! |---|---|---|
//! | [`FindingKind::Unresolved`] | error | No target |
//! | [`FindingKind::MissingAnchor`] | error | The target has no such heading or block id |
//! | [`FindingKind::Blocked`] | error | Disallowed by policy (script URL, network path, backslash, invalid encoding) |
//! | [`FindingKind::Ambiguous`] | warning | Several candidates |
//! | [`FindingKind::CaseFold`] | warning (info where the profile ignores case) | Matched only ignoring case or Unicode normalization |
//! | [`FindingKind::Loose`] | warning | Matched with leniency the profile's tool lacks (`.md` appended), or a heading found only through a fallback |
//! | [`FindingKind::OutsideRoot`] | warning | The target is outside the vault |
//! | [`FindingKind::ProfileDivergent`] | warning | Resolves differently under a second profile ([`CheckOptions::compare_with`]) |
//! | [`FindingKind::NonPortable`] | warning | The target's name is not valid on Windows |
//! | [`FindingKind::AliasHint`] | info | An unresolved wikilink names another note's alias |
//!
//! Links are resolved again (the index keeps only statuses) on the
//! snapshot's worker threads; fragments are matched against the target's
//! **indexed** headings and block ids (unsaved buffers included), with the
//! profile's anchoring first and the other slug algorithms and letter case
//! as loose fallbacks. Explicit heading ids (`{#id}`) are not indexed, so
//! they are not checked.
//!
//! Each finding carries the [`Fix`]es that apply: a letter-case fix, one
//! rewrite per candidate of an ambiguous link, the alias's note for an
//! alias hint, and *Create note* for an unresolved link.
//! [`rewrite_link`] applies a rewrite to the link's text.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use gv_model::slug::obsidian::heading_key;
use gv_model::{Projection, SlugAlgorithm, Slugger};

use super::record::{IndexedBlockId, IndexedHeading, IndexedLink, RecordKind, Span, file_stem};
use super::state::{Cancelled, State, parallel_map};
use super::{ContentOptions, ContentSnapshot, DiskSource, FileSource};
use crate::fragment::FragmentKind;
use crate::index::{IndexConfig, IndexError, PathIndex};
use crate::keys::{fold_key, nfc_key};
use crate::link::{DecodedTarget, LinkKind, LinkRef, decode};
use crate::path::VaultPath;
use crate::platform::PlatformRules;
use crate::profile::{Anchoring, BaseStep, ProfileId, ProfileParams};
use crate::resolve::{Note, Resolution, Status, resolve};
use crate::root::{RootMarker, RootReason, detect_root};
use crate::url::BlockReason;

/// What to check.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CheckScope {
    /// The links of one note.
    Note(VaultPath),
    /// The links of every note in a folder and its subfolders.
    Folder(VaultPath),
    /// Every note.
    Vault,
}

impl CheckScope {
    /// Whether `path` is in the scope.
    #[must_use]
    pub fn contains(&self, path: &VaultPath) -> bool {
        match self {
            Self::Note(note) => note == path,
            Self::Folder(folder) => path.starts_with(folder),
            Self::Vault => true,
        }
    }
}

/// How bad a finding is. Ordered: errors first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    /// `error`, `warning` or `info`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

/// What is wrong with a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FindingKind {
    Unresolved,
    MissingAnchor,
    Blocked,
    Ambiguous,
    CaseFold,
    Loose,
    OutsideRoot,
    ProfileDivergent,
    NonPortable,
    AliasHint,
}

impl FindingKind {
    /// The machine-readable name (`Unresolved`, `MissingAnchor`, …).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved",
            Self::MissingAnchor => "MissingAnchor",
            Self::Blocked => "Blocked",
            Self::Ambiguous => "Ambiguous",
            Self::CaseFold => "CaseFold",
            Self::Loose => "Loose",
            Self::OutsideRoot => "OutsideRoot",
            Self::ProfileDivergent => "ProfileDivergent",
            Self::NonPortable => "NonPortable",
            Self::AliasHint => "AliasHint",
        }
    }

    /// A short title for people.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved link",
            Self::MissingAnchor => "Missing anchor",
            Self::Blocked => "Blocked link",
            Self::Ambiguous => "Ambiguous link",
            Self::CaseFold => "Case differs",
            Self::Loose => "Loose match",
            Self::OutsideRoot => "Outside the vault",
            Self::ProfileDivergent => "Differs between profiles",
            Self::NonPortable => "Not portable",
            Self::AliasHint => "Alias",
        }
    }
}

/// What a fix does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixAction {
    /// Replace the path part of the link's destination with `path`, written
    /// as it goes into the link (percent-encoded in Markdown links). The
    /// fragment, query and display text stay. See [`rewrite_link`].
    Rewrite { path: String },
    /// Create an empty note at `path`, which the link then resolves to.
    CreateNote { path: VaultPath },
}

/// One way to fix a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    /// For a menu or a button: "Fix case: Meeting", "Use Work/Meeting.md",
    /// "Create Ideas/New.md".
    pub label: String,
    pub action: FixAction,
}

/// One problem with one link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The note the link is in.
    pub source: VaultPath,
    /// The link construct in the note.
    pub span: Span,
    /// 1-based line.
    pub line: u32,
    /// 1-based column in characters; 0 when the text could not be read.
    pub column: u32,
    pub link_kind: LinkKind,
    /// In a front matter property.
    pub front_matter: bool,
    pub kind: FindingKind,
    pub severity: Severity,
    /// The destination as written (wikilinks: the whole inside).
    pub raw: Arc<str>,
    pub status: Status,
    /// Where the link points, when it resolved.
    pub target: Option<VaultPath>,
    /// For ambiguous links: every candidate, best first.
    pub candidates: Vec<VaultPath>,
    pub message: String,
    pub fixes: Vec<Fix>,
}

/// Checker settings.
#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    /// Also resolve Markdown links under this profile and report those
    /// that resolve differently ([`FindingKind::ProfileDivergent`]), for a
    /// folder that is both a vault and a repository. Off by default.
    pub compare_with: Option<ProfileId>,
}

/// Progress of a running check, for a progress bar.
#[derive(Debug, Default)]
pub struct CheckProgress {
    /// Notes checked so far.
    pub done: AtomicUsize,
    /// Notes in the scope (set when the check starts).
    pub total: AtomicUsize,
}

/// The result of a check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckReport {
    /// By source path, then position, then kind.
    pub findings: Vec<Finding>,
    /// Notes checked.
    pub notes: usize,
    /// Links checked.
    pub links: usize,
}

/// `glyphvane check-links` exit status: no errors (and with `--strict`, no
/// warnings).
pub const EXIT_CLEAN: u8 = 0;
/// Errors were found.
pub const EXIT_ERRORS: u8 = 1;
/// Warnings were found and `--strict` was given.
pub const EXIT_WARNINGS: u8 = 2;
/// A usage or I/O error.
pub const EXIT_USAGE: u8 = 3;

impl CheckReport {
    /// Findings of `severity`.
    #[must_use]
    pub fn count(&self, severity: Severity) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == severity)
            .count()
    }

    /// The command line's exit status: [`EXIT_ERRORS`] with errors,
    /// [`EXIT_WARNINGS`] with warnings and `strict`, else [`EXIT_CLEAN`].
    #[must_use]
    pub fn exit_code(&self, strict: bool) -> u8 {
        if self.count(Severity::Error) > 0 {
            EXIT_ERRORS
        } else if strict && self.count(Severity::Warning) > 0 {
            EXIT_WARNINGS
        } else {
            EXIT_CLEAN
        }
    }

    /// One line per finding (`path:line:col: severity: Kind: raw: message`),
    /// then a summary line.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            let _ = writeln!(
                out,
                "{}:{}:{}: {}: {}: {}: {}",
                f.source,
                f.line,
                f.column,
                f.severity.name(),
                f.kind.name(),
                f.raw,
                f.message
            );
        }
        let plural = |n: usize, one: &str, many: &str| {
            if n == 1 {
                format!("{n} {one}")
            } else {
                format!("{n} {many}")
            }
        };
        let _ = writeln!(
            out,
            "{}, {}, {} in {} ({})",
            plural(self.count(Severity::Error), "error", "errors"),
            plural(self.count(Severity::Warning), "warning", "warnings"),
            plural(self.count(Severity::Info), "info message", "info messages"),
            plural(self.notes, "file", "files"),
            plural(self.links, "link", "links"),
        );
        out
    }

    /// A JSON array of `{source, line, column, kind, severity, raw, status,
    /// target, candidates, message}` objects, one per finding, with a
    /// final newline.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::from("[");
        for (i, f) in self.findings.iter().enumerate() {
            out.push_str(if i == 0 { "\n  {" } else { ",\n  {" });
            let _ = write!(
                out,
                "\"source\": {}, \"line\": {}, \"column\": {}, \"kind\": {}, \"severity\": {}, \
                 \"raw\": {}, \"status\": {}, \"target\": {}, \"candidates\": [",
                json_str(f.source.as_str()),
                f.line,
                f.column,
                json_str(f.kind.name()),
                json_str(f.severity.name()),
                json_str(&f.raw),
                json_str(&f.status.to_string()),
                f.target
                    .as_ref()
                    .map_or_else(|| "null".to_owned(), |t| json_str(t.as_str())),
            );
            for (j, c) in f.candidates.iter().enumerate() {
                if j > 0 {
                    out.push_str(", ");
                }
                out.push_str(&json_str(c.as_str()));
            }
            let _ = write!(out, "], \"message\": {}}}", json_str(&f.message));
        }
        out.push_str(if self.findings.is_empty() {
            "]\n"
        } else {
            "\n]\n"
        });
        out
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl ContentSnapshot {
    /// Builds a snapshot synchronously from a complete path index (the
    /// command line; no cache, no watcher).
    pub fn build(
        paths: PathIndex,
        options: ContentOptions,
        source: &dyn FileSource,
        cancel: &AtomicBool,
    ) -> Result<Self, Cancelled> {
        let mut state = State::new(Arc::new(options));
        state.apply(Some(paths), Vec::new(), source, cancel)?;
        Ok(Self(Arc::new(state)))
    }

    /// The notes of `scope` (sync-conflict copies left out), sorted.
    #[must_use]
    pub fn scope_notes(&self, scope: &CheckScope) -> Vec<VaultPath> {
        let copies: std::collections::HashSet<VaultPath> =
            self.sync_conflicts().into_iter().map(|c| c.copy).collect();
        self.notes()
            .into_iter()
            .filter(|r| {
                scope.contains(&r.path)
                    && matches!(r.kind, RecordKind::Indexed | RecordKind::Overlay)
            })
            .map(|r| r.path.clone())
            .filter(|p| !copies.contains(p))
            .collect()
    }

    /// The findings of one link of `source` (kind `kind`, destination `raw`
    /// as the resolver takes it, the construct at `span`), with the fixes
    /// [`Self::check_links`] would offer: for a link under the pointer or
    /// the caret, which may not be indexed yet. `construct` is the link's
    /// text in the note; rewrites that cannot be applied to it are left
    /// out.
    #[must_use]
    pub fn check_link(
        &self,
        source: &VaultPath,
        kind: LinkKind,
        raw: &str,
        span: Span,
        construct: Option<&str>,
    ) -> Vec<Finding> {
        let Some(paths) = self.0.paths.as_ref() else {
            return Vec::new();
        };
        let checker = Checker {
            snapshot: self,
            paths,
            params: &self.0.options.params,
            compare: None,
            aliases: OnceLock::new(),
        };
        let link = IndexedLink {
            kind,
            raw: raw.into(),
            span,
            line: 0,
            front_matter: false,
        };
        let mut out = Vec::new();
        checker.link(source, &link, &mut out);
        if let Some(construct) = construct {
            for f in &mut out {
                f.fixes.retain(|fix| match &fix.action {
                    FixAction::Rewrite { path } => {
                        rewrite_link(construct, f.link_kind, &f.raw, path).is_some()
                    }
                    FixAction::CreateNote { .. } => true,
                });
            }
        }
        out
    }

    /// Checks the links of the notes in `scope` on the snapshot's worker
    /// threads. `source` reads note text for columns and to drop rewrites
    /// that cannot be applied (`None`: columns are 0). `cancel` stops it.
    pub fn check_links(
        &self,
        scope: &CheckScope,
        options: &CheckOptions,
        source: Option<&dyn FileSource>,
        progress: &CheckProgress,
        cancel: &AtomicBool,
    ) -> Result<CheckReport, Cancelled> {
        let notes = self.scope_notes(scope);
        progress.total.store(notes.len(), Ordering::Relaxed);
        progress.done.store(0, Ordering::Relaxed);
        let Some(paths) = self.0.paths.as_ref() else {
            return Ok(CheckReport::default());
        };
        let checker = Checker {
            snapshot: self,
            paths,
            params: &self.0.options.params,
            compare: options.compare_with.map(ProfileParams::for_profile),
            aliases: OnceLock::new(),
        };
        let per_note = parallel_map(&notes, self.0.options.threads, cancel, |path| {
            let out = checker.note(path, source);
            progress.done.fetch_add(1, Ordering::Relaxed);
            out
        })?;
        let mut report = CheckReport {
            notes: notes.len(),
            ..CheckReport::default()
        };
        for (links, findings) in per_note {
            report.links += links;
            report.findings.extend(findings);
        }
        Ok(report)
    }
}

/// Folded alias → the notes with it and the alias as written.
type AliasMap = HashMap<String, Vec<(VaultPath, Arc<str>)>>;

struct Checker<'a> {
    snapshot: &'a ContentSnapshot,
    paths: &'a PathIndex,
    params: &'a ProfileParams,
    compare: Option<ProfileParams>,
    /// Folded alias → notes with it, built on first use.
    aliases: OnceLock<AliasMap>,
}

impl Checker<'_> {
    /// The link count and the findings of one note.
    fn note(&self, path: &VaultPath, source: Option<&dyn FileSource>) -> (usize, Vec<Finding>) {
        let Some(record) = self.snapshot.note(path) else {
            return (0, Vec::new());
        };
        let mut out = Vec::new();
        for link in &record.data.links {
            self.link(path, link, &mut out);
        }
        if !out.is_empty()
            && let Some(source) = source
            && let Some(text) = self.snapshot.note_text(path, source)
        {
            for f in &mut out {
                f.column = column_of(&text, f.span.start as usize);
                let construct = text.get(f.span.range()).unwrap_or("");
                f.fixes.retain(|fix| match &fix.action {
                    FixAction::Rewrite { path } => {
                        rewrite_link(construct, f.link_kind, &f.raw, path).is_some()
                    }
                    FixAction::CreateNote { .. } => true,
                });
            }
        }
        out.sort_by(|a, b| {
            (a.span.start, a.kind, &a.message).cmp(&(b.span.start, b.kind, &b.message))
        });
        (record.data.links.len(), out)
    }

    fn link(&self, source: &VaultPath, link: &IndexedLink, out: &mut Vec<Finding>) {
        let lref = LinkRef::new(link.kind, &*link.raw, source.clone(), self.params.profile);
        let r = resolve(self.paths, &lref, self.params);
        let target = r
            .target
            .and_then(|id| self.paths.get(id))
            .map(|e| e.path.clone())
            .or_else(|| r.target_path.clone());
        let base = Finding {
            source: source.clone(),
            span: link.span,
            line: link.line + 1,
            column: 0,
            link_kind: link.kind,
            front_matter: link.front_matter,
            kind: FindingKind::Unresolved,
            severity: Severity::Error,
            raw: link.raw.clone(),
            status: r.status,
            target: target.clone(),
            candidates: Vec::new(),
            message: String::new(),
            fixes: Vec::new(),
        };
        let push = |out: &mut Vec<Finding>,
                    kind: FindingKind,
                    severity: Severity,
                    message: String,
                    fixes: Vec<Fix>| {
            out.push(Finding {
                kind,
                severity,
                message,
                fixes,
                ..base.clone()
            });
        };
        let shown = |t: &VaultPath| t.as_str().to_owned();
        match r.status {
            Status::Exact | Status::CaseFold | Status::Loose | Status::Ambiguous(_) => {}
            Status::Unresolved => {
                let name = path_text(&lref, self.params);
                let fixes = self.create_fix(&lref, source).into_iter().collect();
                push(
                    out,
                    FindingKind::Unresolved,
                    Severity::Error,
                    format!("No note or file “{name}”"),
                    fixes,
                );
                self.alias_hint(link, &name, out, &base);
                return;
            }
            Status::Blocked => {
                let reason = r
                    .block
                    .map_or_else(|| "not allowed".to_owned(), |b| b.to_string());
                let extra = if r.block == Some(BlockReason::Backslash) {
                    "; use `/` between folders"
                } else {
                    ""
                };
                push(
                    out,
                    FindingKind::Blocked,
                    Severity::Error,
                    format!("Blocked: {reason}{extra}"),
                    Vec::new(),
                );
                return;
            }
            Status::OutsideRoot => {
                let at = r.outside_path.clone().unwrap_or_default();
                push(
                    out,
                    FindingKind::OutsideRoot,
                    Severity::Warning,
                    format!("Points outside the vault: {at}"),
                    Vec::new(),
                );
                return;
            }
            Status::Pending | Status::External => return,
        }
        let Some(target) = target else {
            return;
        };

        // Resolved: how well.
        if let Status::Ambiguous(n) = r.status {
            let candidates: Vec<VaultPath> = r.candidates.iter().map(|c| c.path.clone()).collect();
            let fixes = candidates
                .iter()
                .filter_map(|c| {
                    let path = self.unambiguous_path(&lref, c)?;
                    Some(Fix {
                        label: format!("Use {}", c.as_str()),
                        action: FixAction::Rewrite { path },
                    })
                })
                .collect();
            out.push(Finding {
                kind: FindingKind::Ambiguous,
                severity: Severity::Warning,
                message: format!("{n} files match; “{}” is used", shown(&target)),
                candidates,
                fixes,
                ..base.clone()
            });
        }
        let case_note = r
            .notes
            .iter()
            .find(|n| matches!(n, Note::CaseDiffers | Note::NormalizationDiffers));
        if r.status == Status::CaseFold || case_note.is_some() {
            let what = if case_note == Some(&Note::NormalizationDiffers)
                && !r.notes.contains(&Note::CaseDiffers)
            {
                "Unicode normalization"
            } else {
                "Letter case"
            };
            let (severity, why) = if r.status == Status::CaseFold {
                (Severity::Warning, "breaks on Linux and GitHub")
            } else {
                (Severity::Info, "the profile ignores it")
            };
            let fixes = fixed_case(&lref, &target)
                .map(|path| Fix {
                    label: format!("Fix case: {path}"),
                    action: FixAction::Rewrite { path },
                })
                .into_iter()
                .collect();
            push(
                out,
                FindingKind::CaseFold,
                severity,
                format!("{what} differs from “{}”: {why}", shown(&target)),
                fixes,
            );
        }
        if r.status == Status::Loose {
            let message = if r.notes.contains(&Note::ExtensionAppended) {
                format!(
                    "Resolved only by appending an extension (“{}”): breaks on GitHub",
                    shown(&target)
                )
            } else {
                format!("Resolved loosely to “{}”", shown(&target))
            };
            let fixes = r
                .notes
                .contains(&Note::ExtensionAppended)
                .then(|| fixed_case(&lref, &target))
                .flatten()
                .map(|path| Fix {
                    label: format!("Use {path}"),
                    action: FixAction::Rewrite { path },
                })
                .into_iter()
                .collect();
            push(out, FindingKind::Loose, Severity::Warning, message, fixes);
        }
        if let Some(segment) = target
            .segments()
            .find(|s| !PlatformRules::WINDOWS.name_issues(s).is_empty())
        {
            push(
                out,
                FindingKind::NonPortable,
                Severity::Warning,
                format!("“{segment}” is not a valid file name on Windows"),
                Vec::new(),
            );
        }
        self.fragment(&r, &target, out, &base);
        if let Some(other) = &self.compare
            && !link.kind.is_wiki()
            && matches!(decode(&lref, self.params).target, DecodedTarget::Path(_))
        {
            let lref_other = LinkRef::new(link.kind, &*link.raw, source.clone(), other.profile);
            let o = resolve(self.paths, &lref_other, other);
            let other_target = o
                .target
                .and_then(|id| self.paths.get(id))
                .map(|e| e.path.clone())
                .or(o.target_path);
            let same = o.status.is_resolved() && other_target.as_ref() == Some(&target);
            if !same {
                let there = match &other_target {
                    Some(t) if o.status.is_resolved() => format!("“{}”", t.as_str()),
                    _ => "nothing".to_owned(),
                };
                push(
                    out,
                    FindingKind::ProfileDivergent,
                    Severity::Warning,
                    format!(
                        "Resolves to “{}” here but to {there} under {}",
                        shown(&target),
                        other.profile.as_str()
                    ),
                    Vec::new(),
                );
            }
        }
    }

    /// Matches the link's heading or block fragment in the target note.
    fn fragment(&self, r: &Resolution, target: &VaultPath, out: &mut Vec<Finding>, base: &Finding) {
        let Some(fragment) = &r.fragment else {
            return;
        };
        let Some(record) = self.snapshot.note(target) else {
            return;
        };
        if !matches!(record.kind, RecordKind::Indexed | RecordKind::Overlay) {
            return;
        }
        let name = target.as_str();
        let (found, what) = match &fragment.kind {
            FragmentKind::Heading(parts) => (
                match_heading(&record.data.headings, parts, self.params.anchoring),
                format!("heading “{}”", parts.join(" > ")),
            ),
            FragmentKind::Block(id) => (
                match_block(&record.data.block_ids, id),
                format!("block “^{id}”"),
            ),
            FragmentKind::Top | FragmentKind::Lines { .. } | FragmentKind::Viewer(_) => return,
        };
        let (kind, severity, message) = match found {
            AnchorMatch::Exact | AnchorMatch::Top => return,
            AnchorMatch::Loose => (
                FindingKind::Loose,
                Severity::Warning,
                format!(
                    "The {what} of “{name}” matches only loosely (another slug style or letter case)"
                ),
            ),
            AnchorMatch::Missing => (
                FindingKind::MissingAnchor,
                Severity::Error,
                format!("“{name}” has no {what}"),
            ),
        };
        out.push(Finding {
            kind,
            severity,
            message,
            fixes: Vec::new(),
            ..base.clone()
        });
    }

    /// An unresolved wikilink that names another note's alias.
    fn alias_hint(&self, link: &IndexedLink, name: &str, out: &mut Vec<Finding>, base: &Finding) {
        if !link.kind.is_wiki() || name.is_empty() {
            return;
        }
        let aliases = self.aliases.get_or_init(|| {
            let mut map = AliasMap::new();
            for r in self.snapshot.notes() {
                for alias in &r.data.aliases {
                    map.entry(fold_key(alias))
                        .or_default()
                        .push((r.path.clone(), alias.clone()));
                }
            }
            map
        });
        let Some(notes) = aliases.get(&fold_key(name)) else {
            return;
        };
        for (note, alias) in notes {
            let title = self
                .snapshot
                .note(note)
                .map_or_else(|| file_stem(note).to_owned(), |r| r.title().to_owned());
            let lref = LinkRef::new(
                link.kind,
                &*link.raw,
                base.source.clone(),
                self.params.profile,
            );
            let fixes = self
                .unambiguous_path(&lref, note)
                .map(|path| Fix {
                    label: format!("Link to {}", note.as_str()),
                    action: FixAction::Rewrite { path },
                })
                .into_iter()
                .collect();
            out.push(Finding {
                kind: FindingKind::AliasHint,
                severity: Severity::Info,
                message: format!("Did you mean “{title}” (alias {alias})?"),
                target: Some(note.clone()),
                fixes,
                ..base.clone()
            });
        }
    }

    /// The shortest path text, in the link's syntax, that resolves exactly
    /// to `target` from the link's source: the name, then more folders
    /// (wikilinks); a relative path (Markdown links).
    fn unambiguous_path(&self, link: &LinkRef, target: &VaultPath) -> Option<String> {
        let segments: Vec<&str> = target.segments().collect();
        let resolves = |text: &str| {
            let probe = LinkRef::new(link.kind, text, link.source_file.clone(), link.profile);
            let r = resolve(self.paths, &probe, self.params);
            r.status == Status::Exact
                && !r.notes.contains(&Note::CaseDiffers)
                && r.target.and_then(|id| self.paths.get(id)).map(|e| &e.path) == Some(target)
        };
        if link.kind.is_wiki() {
            let is_md = target
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md"));
            for n in 1..=segments.len() {
                let mut text = segments[segments.len() - n..].join("/");
                if is_md {
                    text.truncate(text.len() - 3);
                }
                if text.contains(['|', '#', '[', ']', '^']) {
                    return None;
                }
                if resolves(&text) {
                    return Some(text);
                }
            }
            None
        } else {
            let relative = relative_url(&link.source_file, target);
            if resolves(&relative) {
                return Some(relative);
            }
            let dotted = format!("./{relative}");
            if !relative.starts_with("..") && resolves(&dotted) {
                return Some(dotted);
            }
            let rooted = format!("/{}", encode_path(target.as_str()));
            resolves(&rooted).then_some(rooted)
        }
    }

    /// *Create note* for an unresolved link: in the folder written in the
    /// link, otherwise the source's folder; `.md` appended when the name
    /// has no note extension. `None` when the name is not valid on every
    /// platform, or the link is not a note link.
    fn create_fix(&self, link: &LinkRef, source: &VaultPath) -> Option<Fix> {
        let path = note_path_for_link(link, self.params, source)?;
        Some(Fix {
            label: format!("Create {}", path.as_str()),
            action: FixAction::CreateNote { path },
        })
    }
}

/// Where *Create note* puts the note an unresolved `link` in `source`
/// names. `None` when there is nothing to create (a URL, a non-note
/// file, a name not valid on every platform, a path outside the root).
#[must_use]
pub fn note_path_for_link(
    link: &LinkRef,
    params: &ProfileParams,
    source: &VaultPath,
) -> Option<VaultPath> {
    let decoded = decode(link, params);
    let DecodedTarget::Path(path) = decoded.target else {
        return None;
    };
    if path.is_empty() || path.trailing_slash {
        return None;
    }
    let source_dir: Vec<String> = source
        .parent()
        .map(|p| p.segments().map(str::to_owned).collect())
        .unwrap_or_default();
    let order = if link.kind.is_wiki() {
        &params.wikilink_order
    } else {
        &params.markdown_order
    };
    let from_root = path.absolute
        || (!path.is_dot_relative()
            && path.segments.len() > 1
            && order.first() == Some(&BaseStep::VaultRoot));
    let mut segments: Vec<String> = if from_root { Vec::new() } else { source_dir };
    for seg in &path.segments {
        match seg.as_str() {
            "." => {}
            ".." => {
                segments.pop()?;
            }
            s => segments.push(s.to_owned()),
        }
    }
    let last = segments.last_mut()?;
    let extension = crate::path::extension_of(last);
    match extension {
        None => last.push_str(".md"),
        Some(ext) if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown") => {}
        // `v1.2 notes` has an extension-like suffix: append, as the
        // resolver does.
        Some(_) if link.kind.is_wiki() => last.push_str(".md"),
        Some(_) => return None,
    }
    for seg in &segments {
        if PlatformRules::ALL
            .iter()
            .any(|rules| !rules.name_issues(seg).is_empty())
        {
            return None;
        }
    }
    VaultPath::from_segments(&segments).ok()
}

/// The link's decoded path text (or its raw destination).
fn path_text(link: &LinkRef, params: &ProfileParams) -> String {
    match decode(link, params).target {
        DecodedTarget::Path(p) => p.text,
        _ => link.raw_destination.clone(),
    }
}

/// The link's path with each written segment in the letter case of the
/// target's (for a case-only or extension-appended match): `[[meeting]]`
/// → `Meeting`, `[x](readme.md)` → `README.md`, `[x](other)` →
/// `other.md`. `None` when nothing would change or the path cannot be
/// matched segment by segment.
fn fixed_case(link: &LinkRef, target: &VaultPath) -> Option<String> {
    let written = path_part(link.kind, &link.raw_destination);
    let pieces: Vec<&str> = written.split('/').collect();
    let target_segments: Vec<&str> = target.segments().collect();
    let encoded = !link.kind.is_wiki();
    let mut out: Vec<String> = pieces.iter().map(|p| (*p).to_owned()).collect();
    let mut t = target_segments.len();
    for (i, piece) in pieces.iter().enumerate().rev() {
        if piece.is_empty() || *piece == "." {
            continue;
        }
        if *piece == ".." {
            break;
        }
        if t == 0 {
            return None;
        }
        t -= 1;
        let want = target_segments[t];
        let decoded = if encoded {
            crate::link::percent_decode_lossy(piece)
        } else {
            (*piece).to_owned()
        };
        let key = fold_key(&nfc_key(&decoded));
        let replacement = if key == fold_key(&nfc_key(want)) {
            want.to_owned()
        } else if i == pieces.len() - 1 && key == fold_key(&nfc_key(crate::path::stem_of(want))) {
            // Extension left off: keep it off in wikilinks, add it in
            // Markdown links, where it was only appended loosely.
            if link.kind.is_wiki() {
                crate::path::stem_of(want).to_owned()
            } else {
                want.to_owned()
            }
        } else {
            return None;
        };
        out[i] = if encoded && replacement != decoded {
            encode_segment(&replacement)
        } else if encoded {
            (*piece).to_owned()
        } else {
            replacement
        };
    }
    let fixed = out.join("/");
    (fixed != written).then_some(fixed)
}

/// The part of a raw destination before the fragment, query or display
/// text.
pub(crate) fn path_part(kind: LinkKind, raw: &str) -> &str {
    let end = if kind.is_wiki() {
        raw.find(['#', '|'])
    } else {
        raw.find(['#', '?'])
    };
    end.map_or(raw, |e| &raw[..e])
}

/// `construct` (the link's text in the note) with the path part of its
/// destination replaced by `path`. `None` when the destination cannot be
/// found in the construct as written (a reference link, escapes).
#[must_use]
pub fn rewrite_link(construct: &str, kind: LinkKind, raw: &str, path: &str) -> Option<String> {
    let written = path_part(kind, raw);
    if written.is_empty() {
        return None;
    }
    let at = if kind.is_wiki() {
        let open = construct.find("[[")? + 2;
        let rest = &construct[open..];
        let lead = rest.len() - rest.trim_start().len();
        construct[open + lead..]
            .starts_with(written)
            .then_some(open + lead)?
    } else {
        let from = construct.find("](").map_or(0, |i| i + 2);
        from + construct[from..].find(written)?
    };
    let mut out = String::with_capacity(construct.len() + path.len());
    out.push_str(&construct[..at]);
    out.push_str(path);
    out.push_str(&construct[at + written.len()..]);
    Some(out)
}

/// The 1-based column (in characters) of byte `offset`.
fn column_of(text: &str, offset: usize) -> u32 {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let chars = text
        .get(line_start..offset)
        .map_or(0, |s| s.chars().count());
    u32::try_from(chars + 1).unwrap_or(u32::MAX)
}

/// The path of `to` relative to the folder of `from`, percent-encoded.
fn relative_url(from: &VaultPath, to: &VaultPath) -> String {
    let mut from_dir: Vec<&str> = from.segments().collect();
    from_dir.pop();
    let to_segments: Vec<&str> = to.segments().collect();
    let common = from_dir
        .iter()
        .zip(&to_segments)
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = vec!["..".into(); from_dir.len() - common];
    parts.extend(to_segments[common..].iter().map(|s| encode_segment(s)));
    parts.join("/")
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for c in segment.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            '%' => out.push_str("%25"),
            _ => out.push(c),
        }
    }
    out
}

// ----- Fragments against indexed headings and block ids ---------------------

/// Where a fragment landed in the target's indexed headings or block ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnchorMatch {
    Exact,
    /// Through a fallback: another anchoring, or ignoring case.
    Loose,
    /// `top` with no heading of that id.
    Top,
    Missing,
}

/// GitHub prefixes the ids of user content.
const USER_CONTENT: &str = "user-content-";

/// Slugs and heading-text keys of one note's headings, computed on use.
struct HeadingKeys<'a> {
    headings: &'a [IndexedHeading],
    /// Per algorithm: (slug in NFC, lowercased).
    slugs: HashMap<SlugAlgorithm, Vec<(String, String)>>,
    /// Obsidian keys of (source, text).
    text: Option<Vec<(String, String)>>,
}

impl<'a> HeadingKeys<'a> {
    fn new(headings: &'a [IndexedHeading]) -> Self {
        Self {
            headings,
            slugs: HashMap::new(),
            text: None,
        }
    }

    fn slugs(&mut self, alg: SlugAlgorithm) -> &[(String, String)] {
        let headings = self.headings;
        self.slugs.entry(alg).or_insert_with(|| {
            let mut slugger = Slugger::new(alg);
            headings
                .iter()
                .map(|h| {
                    let projected: &str = match alg.projection() {
                        Projection::Text | Projection::TextWithAlt => &h.text,
                        Projection::Source => &h.source,
                        Projection::SourceLastLine => h.source.rsplit('\n').next().unwrap_or(""),
                    };
                    let slug = nfc_key(&slugger.slug(projected)).into_owned();
                    let lower = slug.to_lowercase();
                    (slug, lower)
                })
                .collect()
        })
    }

    fn text(&mut self) -> &[(String, String)] {
        let headings = self.headings;
        self.text.get_or_insert_with(|| {
            headings
                .iter()
                .map(|h| {
                    (
                        nfc_key(&heading_key(&h.source)).into_owned(),
                        nfc_key(&heading_key(&h.text)).into_owned(),
                    )
                })
                .collect()
        })
    }

    /// The first heading in `range` that `part` names under `anchoring`.
    fn find_by(
        &mut self,
        anchoring: Anchoring,
        range: std::ops::Range<usize>,
        part: &str,
    ) -> Option<usize> {
        let stripped = part.strip_prefix(USER_CONTENT).filter(|s| !s.is_empty());
        match anchoring {
            Anchoring::None => None,
            Anchoring::Slug(alg) => {
                let keys = self.slugs(alg);
                range
                    .into_iter()
                    .find(|&i| keys[i].0 == part || Some(keys[i].0.as_str()) == stripped)
            }
            Anchoring::HeadingText => {
                let want = nfc_key(&heading_key(part)).into_owned();
                if want.is_empty() {
                    return None;
                }
                let keys = self.text();
                range
                    .into_iter()
                    .find(|&i| keys[i].0 == want || keys[i].1 == want)
            }
        }
    }

    /// The first heading in `range` that `part` names, and whether only
    /// loosely. `exact_only`: the primary anchoring only.
    fn find(
        &mut self,
        range: std::ops::Range<usize>,
        part: &str,
        primary: Anchoring,
        exact_only: bool,
    ) -> Option<(usize, bool)> {
        let part = nfc_key(part).into_owned();
        if let Some(i) = self.find_by(primary, range.clone(), &part) {
            return Some((i, false));
        }
        if exact_only {
            return None;
        }
        let others: Vec<Anchoring> = std::iter::once(Anchoring::HeadingText)
            .chain(SlugAlgorithm::ALL.iter().map(|&a| Anchoring::Slug(a)))
            .filter(|&a| a != primary)
            .collect();
        for &a in &others {
            if let Some(i) = self.find_by(a, range.clone(), &part) {
                return Some((i, true));
            }
        }
        let lower = part.to_lowercase();
        let lower_stripped = lower
            .strip_prefix(USER_CONTENT)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        for a in std::iter::once(primary).chain(others) {
            if let Anchoring::Slug(alg) = a {
                let keys = self.slugs(alg);
                if let Some(i) = range.clone().find(|&i| {
                    keys[i].1 == lower || lower_stripped.as_deref() == Some(keys[i].1.as_str())
                }) {
                    return Some((i, true));
                }
            }
        }
        None
    }
}

/// Matches a heading fragment (`parts`: one part, or an Obsidian chain)
/// against `headings` in document order.
fn match_heading(headings: &[IndexedHeading], parts: &[String], primary: Anchoring) -> AnchorMatch {
    let mut keys = HeadingKeys::new(headings);
    if let [only] = parts
        && only.eq_ignore_ascii_case("top")
    {
        return match keys.find(0..headings.len(), "top", primary, true) {
            Some(_) => AnchorMatch::Exact,
            None => AnchorMatch::Top,
        };
    }
    let mut range = 0..headings.len();
    let mut loose = false;
    let mut found = false;
    for part in parts {
        let Some((i, l)) = keys.find(range.clone(), part, primary, false) else {
            return AnchorMatch::Missing;
        };
        loose |= l;
        found = true;
        let level = headings[i].level;
        let end = headings[i + 1..]
            .iter()
            .position(|h| h.level <= level)
            .map_or(headings.len(), |p| i + 1 + p);
        range = i + 1..end;
    }
    match (found, loose) {
        (false, _) => AnchorMatch::Missing,
        (true, false) => AnchorMatch::Exact,
        (true, true) => AnchorMatch::Loose,
    }
}

/// Matches a block id: exactly, then ignoring case (loose).
/// ORACLE-PENDING: whether Obsidian's block ids are case-sensitive.
fn match_block(blocks: &[IndexedBlockId], id: &str) -> AnchorMatch {
    if blocks.iter().any(|b| &*b.id == id) {
        return AnchorMatch::Exact;
    }
    let lower = id.to_lowercase();
    if blocks.iter().any(|b| b.id.to_lowercase() == lower) {
        AnchorMatch::Loose
    } else {
        AnchorMatch::Missing
    }
}

// ----- Headless runs ---------------------------------------------------------

/// A check of a path on disk (the command line).
#[derive(Debug)]
pub struct PathCheck {
    /// The vault root the links resolve in.
    pub root: PathBuf,
    /// The profile used.
    pub profile: ProfileId,
    pub scope: CheckScope,
    pub report: CheckReport,
}

/// The profile of a vault found by root detection: Obsidian for a
/// `.obsidian` folder, GitHub for a repository, Extended otherwise (as the
/// app picks it).
#[must_use]
pub fn profile_for_root(reason: RootReason) -> ProfileId {
    match reason {
        RootReason::Marker(RootMarker::Obsidian) => ProfileId::Obsidian,
        RootReason::Marker(RootMarker::Git) => ProfileId::GfmFile,
        RootReason::Marker(RootMarker::Glyphvane) | RootReason::FileFolder => ProfileId::Extended,
    }
}

/// Checks the links of `path`: a note (its links), the vault root (every
/// note) or a folder inside a vault (its notes). The vault is found by
/// root detection from `path`; `profile` overrides the detected profile.
/// Builds the path and content indexes in memory, without a cache.
pub fn check_path(
    path: &Path,
    profile: Option<ProfileId>,
    options: &CheckOptions,
) -> io::Result<PathCheck> {
    let path = std::path::absolute(path)?;
    let meta = std::fs::metadata(&path)?;
    let detected = if meta.is_dir() {
        // Root detection starts at a file's folder.
        detect_root(&path.join(".glyphvane-check"))
    } else {
        detect_root(&path)
    };
    let profile = profile.unwrap_or_else(|| profile_for_root(detected.reason));
    let root = detected.root;
    let relative = path
        .strip_prefix(&root)
        .ok()
        .and_then(|p| VaultPath::from_relative_fs_path(p).ok())
        .ok_or_else(|| io::Error::other("the path is not inside its vault"))?;
    let scope = if meta.is_dir() {
        if relative == VaultPath::root() {
            CheckScope::Vault
        } else {
            CheckScope::Folder(relative)
        }
    } else {
        CheckScope::Note(relative)
    };
    let (paths, _) = PathIndex::build(&root, IndexConfig::default()).map_err(|e| match e {
        IndexError::Io(e) => e,
        other => io::Error::other(other.to_string()),
    })?;
    let content = ContentOptions::for_profile(profile);
    let source = DiskSource::new(&root, &content);
    let cancel = AtomicBool::new(false);
    let snapshot = ContentSnapshot::build(paths, content, &source, &cancel)
        .map_err(|_| io::Error::other("cancelled"))?;
    let report = snapshot
        .check_links(
            &scope,
            options,
            Some(&source),
            &CheckProgress::default(),
            &cancel,
        )
        .map_err(|_| io::Error::other("cancelled"))?;
    Ok(PathCheck {
        root,
        profile,
        scope,
        report,
    })
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
