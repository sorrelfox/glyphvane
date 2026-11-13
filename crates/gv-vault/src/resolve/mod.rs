// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Link resolution: pure functions over a [`PathIndex`] snapshot.
//!
//! [`resolve`] follows these steps and records each one in a
//! [trace](Resolution::trace):
//!
//! 1. **Scheme.** URLs are classified ([`crate::url`]) and become
//!    `External` or `Blocked`; `file:` URLs and drive paths become local
//!    absolute paths; network paths are `Blocked` (following needs
//!    confirmation). Repository URLs of the vault's own repository and
//!    `obsidian://open` links to this vault map to local files.
//! 2. **Empty path**: the current document.
//! 3. **Base.** `./x` and `../x` resolve relative to the source only; `/x`
//!    against the link root; bare paths in the profile's order (Obsidian:
//!    vault root, then relative, then basename lookup).
//! 4. **Lexical normalization** of `.` and `..`; symlinks are never
//!    consulted. A path above the root is `OutsideRoot`.
//! 5. **Candidates**: the exact path, then the path with each profile
//!    extension **appended**, then a directory's index files.
//! 6. **Match keys**: exact bytes, then NFC, then case fold (outer loop),
//!    over the candidates (inner loop).
//! 7. **Basename / suffix lookup** with segment-boundary matching; sync
//!    conflict copies excluded; `userIgnoreFilters` matches ranked last.
//! 8. **Tie-break** ([`tiebreak`]); several candidates give `Ambiguous(n)`.
//! 9. **While the index builds**, only exact names are decided (from the
//!    partial index or a [`PathProbe`]); everything else is `Pending`.

pub mod tiebreak;

use std::fmt;
use std::path::{Component, Path};

use crate::fragment::{Fragment, FragmentResolver, FragmentTarget};
use crate::index::conflict::detect_sync_conflict;
use crate::index::{Entry, EntryKind, FileId, PathIndex, SymlinkTarget};
use crate::keys::{fold_key, nfc_key};
use crate::link::{DecodedTarget, ImageSize, LinkPath, LinkRef, decode, percent_decode_lossy};
use crate::path::{Lexical, VaultPath, normalize_lexically, stem_of};
use crate::probe::{NoProbe, PathProbe, ProbeResult};
use crate::profile::{BaseStep, DirectoryTargets, IndexFile, MatchQuality, ProfileParams};
use crate::repo::{MappedUrl, RepoContext, RepoUrlMode};
use crate::url::{BlockReason, ConfirmReason, UrlClass, UrlContext, UrlPolicy};

use tiebreak::TieBreakKey;

/// The outcome class of a resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// Resolved exactly as the profile's tool would.
    Exact,
    /// Matched only by case folding or NFC normalization.
    CaseFold,
    /// Matched with leniency the tool does not have (`.md` appended in GFM).
    Loose,
    /// Several candidates; the best one was chosen by tie-break.
    Ambiguous(usize),
    /// The index is still building; the target cannot be decided yet.
    Pending,
    /// No target.
    Unresolved,
    /// The target escapes the root.
    OutsideRoot,
    /// A URL with an allowed (or confirmable) scheme, handed to the OS.
    External,
    /// Disallowed by policy.
    Blocked,
}

impl Status {
    /// `true` if a target file or folder was found.
    #[must_use]
    pub fn is_resolved(self) -> bool {
        matches!(
            self,
            Self::Exact | Self::CaseFold | Self::Loose | Self::Ambiguous(_)
        )
    }

    fn from_quality(q: MatchQuality) -> Self {
        match q {
            MatchQuality::Exact => Self::Exact,
            MatchQuality::CaseFold => Self::CaseFold,
            MatchQuality::Loose => Self::Loose,
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact => f.write_str("Exact"),
            Self::CaseFold => f.write_str("CaseFold"),
            Self::Loose => f.write_str("Loose"),
            Self::Ambiguous(n) => write!(f, "Ambiguous({n})"),
            Self::Pending => f.write_str("Pending"),
            Self::Unresolved => f.write_str("Unresolved"),
            Self::OutsideRoot => f.write_str("OutsideRoot"),
            Self::External => f.write_str("External"),
            Self::Blocked => f.write_str("Blocked"),
        }
    }
}

/// The step a trace line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The link being resolved.
    Link,
    /// Decoding results worth showing.
    Decode,
    /// Scheme detection and URL policy.
    Scheme,
    /// The current document.
    Current,
    /// Lookup at the vault root.
    VaultRoot,
    /// Lookup relative to the source file.
    Relative,
    /// Lookup at the link root (`/x`).
    LinkRoot,
    /// A local absolute path.
    Absolute,
    /// Basename / suffix lookup.
    Basename,
    /// Following a symlink.
    Symlink,
    /// Folder fallback (Obsidian).
    Folder,
    /// Repository URL or `obsidian://` mapping.
    Mapped,
    /// Waiting for the index.
    Pending,
    /// The final status.
    Status,
    /// A remark.
    Note,
}

impl Stage {
    /// The label used in rendered traces.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Decode => "decode",
            Self::Scheme => "scheme",
            Self::Current => "current",
            Self::VaultRoot => "vault-root",
            Self::Relative => "relative",
            Self::LinkRoot => "link-root",
            Self::Absolute => "absolute",
            Self::Basename => "basename",
            Self::Symlink => "symlink",
            Self::Folder => "folder",
            Self::Mapped => "mapped",
            Self::Pending => "pending",
            Self::Status => "status",
            Self::Note => "note",
        }
    }
}

/// One line of a resolution trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceStep {
    /// The step.
    pub stage: Stage,
    /// What was tried (a path, a key).
    pub subject: String,
    /// What happened.
    pub outcome: String,
}

impl fmt::Display for TraceStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.outcome.is_empty() {
            write!(f, "{:<11} {}", self.stage.label(), self.subject)
        } else {
            write!(
                f,
                "{:<11} {}  {}",
                self.stage.label(),
                self.subject,
                self.outcome
            )
        }
    }
}

/// Facts the link checker and the hover card report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// The match differs in letter case ("may break on case-sensitive
    /// systems and GitHub"), even where the status is `Exact`.
    CaseDiffers,
    /// The match differs in Unicode normalization (NFC vs NFD).
    NormalizationDiffers,
    /// An extension was appended (`Note` → `Note.md`).
    ExtensionAppended,
    /// A directory resolved to its index file.
    DirectoryIndex,
    /// No file matched; a folder of that name is revealed instead.
    FolderRevealed,
    /// Reached through a symlink inside the root.
    ViaSymlink,
    /// Mapped from a repository URL or an `obsidian://` link.
    MappedFromUrl,
    /// A commit SHA was mapped to the working tree (shows the current version).
    WorkingTreeForSha,
    /// The URL's ref is not a known ref of the repository.
    UnknownRef,
    /// An absolute OS path that lies inside the root.
    AbsolutePathInsideRoot,
    /// A `..` path that climbs out of the root and back in by its name.
    ReenteredRoot,
    /// The link's profile differs from the parameters' profile.
    ProfileMismatch,
    /// Sync-conflict copies were left out of a basename lookup.
    ConflictCopiesExcluded(usize),
}

/// One candidate of an ambiguous resolution, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Its id.
    pub id: FileId,
    /// Its path.
    pub path: VaultPath,
    /// Matched by `userIgnoreFilters`.
    pub user_ignored: bool,
}

/// What the target is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// A file.
    File,
    /// A folder (no index file): reveal it in the file tree.
    Directory,
}

/// The result of resolving one link.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    /// The status.
    pub status: Status,
    /// The target, when the index knows it (a target confirmed by a probe
    /// while the index builds has a path but no id yet).
    pub target: Option<FileId>,
    /// The target's path.
    pub target_path: Option<VaultPath>,
    /// File or folder.
    pub target_kind: Option<TargetKind>,
    /// For `OutsideRoot`: where the link points (`../x.md`, `/etc/hosts`).
    pub outside_path: Option<String>,
    /// The fragment or subpath, classified (matched later by a
    /// [`FragmentResolver`]).
    pub fragment: Option<Fragment>,
    /// Wikilink display text.
    pub display: Option<String>,
    /// Wikilink image size.
    pub size: Option<ImageSize>,
    /// `?plain=1`: open in Source mode.
    pub open_source_mode: bool,
    /// For URLs: the classification.
    pub url: Option<UrlClass>,
    /// What the link's context may do (follow, load) with the target, when
    /// policy applies (URLs, outside-root embeds).
    pub policy: Option<UrlPolicy>,
    /// For `Blocked` (and blocked outside-root embeds): why.
    pub block: Option<BlockReason>,
    /// For `Ambiguous`: all candidates, best first.
    pub candidates: Vec<Candidate>,
    /// Checker facts.
    pub notes: Vec<Note>,
    /// The steps tried.
    pub trace: Vec<TraceStep>,
}

impl Resolution {
    fn new(status: Status) -> Self {
        Self {
            status,
            target: None,
            target_path: None,
            target_kind: None,
            outside_path: None,
            fragment: None,
            display: None,
            size: None,
            open_source_mode: false,
            url: None,
            policy: None,
            block: None,
            candidates: Vec::new(),
            notes: Vec::new(),
            trace: Vec::new(),
        }
    }

    /// The trace as text, one step per line (the hover card's "Show
    /// resolution trace").
    #[must_use]
    pub fn trace_text(&self) -> String {
        let mut out = String::new();
        for (i, step) in self.trace.iter().enumerate() {
            if i == 0 && step.stage == Stage::Link {
                out.push_str(&format!("{} {}\n", step.subject, step.outcome));
            } else {
                out.push_str(&format!("  {step}\n"));
            }
        }
        out
    }

    /// Resolves the fragment inside the target with `fragments`. `None`
    /// when there is no fragment or no target.
    #[must_use]
    pub fn resolve_fragment(
        &self,
        fragments: &dyn FragmentResolver,
        params: &ProfileParams,
    ) -> Option<FragmentTarget> {
        let fragment = self.fragment.as_ref()?;
        let path = self.target_path.as_ref()?;
        if !self.status.is_resolved() {
            return None;
        }
        Some(fragments.resolve_fragment(path, fragment, params))
    }
}

/// Resolves `link` against a snapshot. Deterministic; no I/O.
#[must_use]
pub fn resolve(index: &PathIndex, link: &LinkRef, params: &ProfileParams) -> Resolution {
    resolve_with_probe(index, link, params, &NoProbe)
}

/// Like [`resolve`], with a probe for exact names while the index is
/// still building ([`crate::probe::FsProbe`]). With a complete index the
/// probe is never used.
#[must_use]
pub fn resolve_with_probe(
    index: &PathIndex,
    link: &LinkRef,
    params: &ProfileParams,
    probe: &dyn PathProbe,
) -> Resolution {
    Resolver {
        index,
        link,
        params,
        probe,
        trace: Vec::new(),
        notes: Vec::new(),
    }
    .run()
}

/// Candidates listed one per line in a trace; the rest are counted.
const TRACE_CANDIDATES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Exact,
    Nfc,
    Fold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Base {
    VaultRoot,
    Relative,
    LinkRoot,
    Basename,
}

struct Found {
    id: Option<FileId>,
    path: VaultPath,
    kind: TargetKind,
    quality: MatchQuality,
    candidates: Vec<Candidate>,
}

enum Outcome {
    Found(Found),
    NotFound,
    Pending(String),
    Outside(String),
    Blocked(BlockReason),
}

enum Followed {
    Target(Option<FileId>, VaultPath, TargetKind),
    Outside(String),
    Broken,
}

struct Resolver<'a> {
    index: &'a PathIndex,
    link: &'a LinkRef,
    params: &'a ProfileParams,
    probe: &'a dyn PathProbe,
    trace: Vec<TraceStep>,
    notes: Vec<Note>,
}

fn root_components(root: &Path) -> Vec<String> {
    root.components()
        .filter_map(|c| match c {
            Component::Prefix(p) => p.as_os_str().to_str().map(str::to_owned),
            Component::Normal(s) => s.to_str().map(str::to_owned),
            _ => None,
        })
        .collect()
}

impl<'a> Resolver<'a> {
    fn step(&mut self, stage: Stage, subject: impl Into<String>, outcome: impl Into<String>) {
        self.trace.push(TraceStep {
            stage,
            subject: subject.into(),
            outcome: outcome.into(),
        });
    }

    fn note(&mut self, note: Note) {
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    fn run(mut self) -> Resolution {
        let link = self.link;
        let params = self.params;
        self.step(
            Stage::Link,
            format!("{} {:?}", link.kind, link.raw_destination),
            format!("from {} (profile: {})", link.source_file, params.profile),
        );
        if link.profile != params.profile {
            self.note(Note::ProfileMismatch);
            self.step(
                Stage::Note,
                format!("link profile {}", link.profile),
                format!("resolving with {}", params.profile),
            );
        }
        let decoded = decode(link, params);
        let mut res = match &decoded.target {
            DecodedTarget::Blocked(reason) => {
                self.step(
                    Stage::Decode,
                    &link.raw_destination,
                    format!("blocked: {reason}"),
                );
                self.finish(Outcome::Blocked(*reason))
            }
            DecodedTarget::Url(class) => self.url(class),
            DecodedTarget::Path(lp) => {
                if decoded.fragment.is_some()
                    || decoded.query.is_some()
                    || lp.text != link.raw_destination
                {
                    let mut parts = vec![format!("path {:?}", lp.text)];
                    if let Some(q) = &decoded.query {
                        parts.push(format!("query {q:?}"));
                    }
                    if let Some(f) = &decoded.fragment {
                        parts.push(format!("fragment {:?}", f.text));
                    }
                    self.step(Stage::Decode, "", parts.join(", "));
                }
                let outcome = self.local(lp, None);
                self.finish(outcome)
            }
        };
        if res.fragment.is_none() {
            res.fragment = decoded.fragment.clone();
        }
        res.display.clone_from(&decoded.display);
        res.size = decoded.size;
        res.open_source_mode = decoded.wants_source_mode();
        if res.status == Status::OutsideRoot
            && link.kind.loads_content()
            && !params.settings.outside_root_embeds
        {
            res.block = Some(BlockReason::OutsideRootEmbed);
            res.policy = Some(UrlPolicy::Block(BlockReason::OutsideRootEmbed));
        }
        let mut status_line = res.status.to_string();
        if let Some(p) = &res.target_path {
            status_line.push_str(&format!(" -> {p}"));
        }
        self.step(Stage::Status, status_line, "");
        res.trace = std::mem::take(&mut self.trace);
        res.notes = std::mem::take(&mut self.notes);
        res
    }

    fn finish(&mut self, outcome: Outcome) -> Resolution {
        match outcome {
            Outcome::Found(f) => {
                let status = if f.candidates.len() > 1 {
                    Status::Ambiguous(f.candidates.len())
                } else {
                    Status::from_quality(f.quality)
                };
                let mut r = Resolution::new(status);
                r.target = f.id;
                r.target_path = Some(f.path);
                r.target_kind = Some(f.kind);
                r.candidates = f.candidates;
                r
            }
            Outcome::NotFound => Resolution::new(Status::Unresolved),
            Outcome::Pending(why) => {
                self.step(Stage::Pending, why, "");
                Resolution::new(Status::Pending)
            }
            Outcome::Outside(p) => {
                let mut r = Resolution::new(Status::OutsideRoot);
                r.outside_path = Some(p);
                r
            }
            Outcome::Blocked(reason) => {
                let mut r = Resolution::new(Status::Blocked);
                r.block = Some(reason);
                r.policy = Some(UrlPolicy::Block(reason));
                r
            }
        }
    }

    // ---- URLs -----------------------------------------------------------

    fn url(&mut self, class: &UrlClass) -> Resolution {
        let ctx = self.link.kind.url_context();
        let settings = &self.params.settings;
        let mut res = match class {
            UrlClass::LocalFile { path, drive } => {
                self.step(Stage::Scheme, path, "local absolute path");
                let outcome = self.os_absolute(path, *drive);
                let mut r = self.finish(outcome);
                if r.status == Status::OutsideRoot {
                    r.policy = Some(class.policy(ctx));
                }
                r
            }
            UrlClass::Web { url, .. }
                if settings.map_repo_urls
                    && let Some(repo) = settings.repo.as_deref()
                    && let Some(m) = repo.map_web_url(url) =>
            {
                self.step(Stage::Scheme, url, "repository URL of this vault");
                let outcome = self.mapped(&m, repo);
                self.finish(outcome)
            }
            UrlClass::Obsidian(url) => self.obsidian(url, class, ctx),
            UrlClass::Network { url, .. } => {
                let policy = class.policy(ctx);
                self.step(
                    Stage::Scheme,
                    url,
                    "network path: never loaded automatically",
                );
                let mut r = self.finish(Outcome::Blocked(BlockReason::NetworkPath));
                r.policy = Some(policy);
                r
            }
            _ => {
                let policy = class.policy(ctx);
                match policy {
                    UrlPolicy::Block(reason) => {
                        self.step(
                            Stage::Scheme,
                            describe_url(class),
                            format!("blocked: {reason}"),
                        );
                        let mut r = self.finish(Outcome::Blocked(reason));
                        r.policy = Some(policy);
                        r
                    }
                    UrlPolicy::Allow | UrlPolicy::Confirm(_) => {
                        let how = if let UrlPolicy::Confirm(c) = policy {
                            format!("external, confirm ({c:?})")
                        } else {
                            "external".to_owned()
                        };
                        self.step(Stage::Scheme, describe_url(class), how);
                        let mut r = Resolution::new(Status::External);
                        r.policy = Some(policy);
                        r
                    }
                }
            }
        };
        res.url = Some(class.clone());
        res
    }

    fn obsidian(&mut self, url: &str, class: &UrlClass, ctx: UrlContext) -> Resolution {
        let rest = url.split_once("://").map_or("", |(_, r)| r);
        let (action, query) = rest.split_once('?').unwrap_or((rest, ""));
        let action = action.trim_end_matches('/').to_ascii_lowercase();
        let args: Vec<(&str, String)> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k, percent_decode_lossy(v)))
            .collect();
        let get = |key: &str| args.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
        let this_vault = self.params.settings.vault_name.is_some()
            && get("vault") == self.params.settings.vault_name;
        if action == "open" && this_vault {
            if let Some(file) = get("file") {
                self.step(Stage::Mapped, url, format!("this vault, file {file:?}"));
                self.note(Note::MappedFromUrl);
                let (path, sub) = match file.split_once('#') {
                    Some((p, s)) => (p.to_owned(), Some(s.to_owned())),
                    None => (file.clone(), None),
                };
                let outcome = match LinkPath::parse(&path, false) {
                    // ORACLE-PENDING: `file=` is a vault path or a name,
                    // resolved like a link from the vault root.
                    Ok(lp) => self.local(&lp, Some(&[Base::VaultRoot, Base::Basename])),
                    Err(reason) => Outcome::Blocked(reason),
                };
                let mut r = self.finish(outcome);
                r.fragment = sub.as_deref().map(Fragment::from_wikilink);
                return r;
            }
            if let Some(path) = get("path") {
                self.step(Stage::Mapped, url, format!("this vault, path {path:?}"));
                self.note(Note::MappedFromUrl);
                let drive = path.len() > 2 && path.as_bytes()[1] == b':';
                let outcome = self.os_absolute(&path, drive);
                return self.finish(outcome);
            }
        }
        let policy = class.policy(ctx);
        self.step(
            Stage::Scheme,
            url,
            "obsidian link to another vault or action",
        );
        let mut r = Resolution::new(if matches!(policy, UrlPolicy::Block(_)) {
            Status::Blocked
        } else {
            Status::External
        });
        if let UrlPolicy::Block(reason) = policy {
            r.block = Some(reason);
        }
        r.policy = Some(policy);
        r
    }

    fn os_absolute(&mut self, path: &str, drive: bool) -> Outcome {
        let comps: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        let root = root_components(self.index.root());
        let eq = |a: &str, b: &str| {
            if drive {
                a.eq_ignore_ascii_case(b)
            } else {
                a == b
            }
        };
        if comps.len() >= root.len()
            && comps.iter().zip(&root).all(|(a, b)| eq(a, b))
            && let Lexical::Inside(segs) =
                normalize_lexically(Vec::<String>::new(), &comps[root.len()..])
        {
            self.note(Note::AbsolutePathInsideRoot);
            return self.candidates(Stage::Absolute, &segs, false);
        }
        self.step(Stage::Absolute, path, "outside the root");
        Outcome::Outside(path.to_owned())
    }

    fn mapped(&mut self, m: &MappedUrl, repo: &RepoContext) -> Outcome {
        let mut segs: Vec<String> = repo.repo_root.segments().map(str::to_owned).collect();
        segs.extend(m.path.iter().cloned());
        self.note(Note::MappedFromUrl);
        if m.sha {
            self.note(Note::WorkingTreeForSha);
        } else if !m.known_ref {
            self.note(Note::UnknownRef);
        }
        self.step(
            Stage::Mapped,
            format!("{:?} {}", m.mode, m.git_ref),
            segs.join("/"),
        );
        match normalize_lexically(Vec::<String>::new(), &segs) {
            Lexical::Inside(s) => self.candidates(Stage::Mapped, &s, m.mode == RepoUrlMode::Tree),
            Lexical::Escapes { .. } => Outcome::Outside(segs.join("/")),
        }
    }

    // ---- local paths ----------------------------------------------------

    fn local(&mut self, lp: &LinkPath, forced: Option<&[Base]>) -> Outcome {
        let source = &self.link.source_file;
        if lp.is_empty() {
            self.step(Stage::Current, source.as_str(), "current document");
            return Outcome::Found(Found {
                id: self.index.id_of(source.as_str()),
                path: source.clone(),
                kind: TargetKind::File,
                quality: MatchQuality::Exact,
                candidates: Vec::new(),
            });
        }
        let source_dir: Vec<String> = source
            .parent()
            .map(|p| p.segments().map(str::to_owned).collect())
            .unwrap_or_default();
        let order = if self.link.kind.is_wiki() {
            &self.params.wikilink_order
        } else {
            &self.params.markdown_order
        };
        let bases: Vec<Base> = if let Some(f) = forced {
            f.to_vec()
        } else if lp.is_dot_relative() {
            vec![Base::Relative]
        } else if lp.absolute {
            vec![Base::LinkRoot]
        } else {
            order
                .iter()
                .map(|b| match b {
                    BaseStep::VaultRoot => Base::VaultRoot,
                    BaseStep::Relative => Base::Relative,
                    BaseStep::Basename => Base::Basename,
                })
                .collect()
        };
        let mut tried: Vec<(Stage, Vec<String>)> = Vec::new();
        for base in bases {
            let outcome = if base == Base::Basename {
                self.basename(lp)
            } else {
                let (stage, base_segs) = match base {
                    Base::VaultRoot => (Stage::VaultRoot, Vec::new()),
                    Base::Relative => (Stage::Relative, source_dir.clone()),
                    _ => (
                        Stage::LinkRoot,
                        self.params
                            .link_root
                            .segments()
                            .map(str::to_owned)
                            .collect(),
                    ),
                };
                match normalize_lexically(&base_segs, &lp.segments) {
                    Lexical::Inside(segs) => {
                        let o = self.candidates(stage, &segs, lp.trailing_slash);
                        if matches!(o, Outcome::NotFound) {
                            tried.push((stage, segs));
                        }
                        o
                    }
                    Lexical::Escapes { above, rest } => self.escaped(stage, lp, above, &rest),
                }
            };
            if !matches!(outcome, Outcome::NotFound) {
                return outcome;
            }
        }
        if lp.absolute
            && let Some(inside) = self.abs_inside_root(&lp.segments)
        {
            self.note(Note::AbsolutePathInsideRoot);
            let o = self.candidates(Stage::Absolute, &inside, lp.trailing_slash);
            if !matches!(o, Outcome::NotFound) {
                return o;
            }
        }
        if self.folders_after_files() && !lp.trailing_slash {
            for (_, segs) in &tried {
                let o = self.candidates(Stage::Folder, segs, true);
                if let Outcome::Found(f) = o {
                    self.note(Note::FolderRevealed);
                    return Outcome::Found(f);
                }
            }
        }
        Outcome::NotFound
    }

    /// `/home/u/vault/x.md` written as a link-root path: the part below the
    /// root, if the path names the root on disk.
    fn abs_inside_root(&self, segs: &[String]) -> Option<Vec<String>> {
        let root = root_components(self.index.root());
        if root.is_empty() || segs.len() <= root.len() || segs[..root.len()] != root[..] {
            return None;
        }
        match normalize_lexically(Vec::<String>::new(), &segs[root.len()..]) {
            Lexical::Inside(s) => Some(s),
            Lexical::Escapes { .. } => None,
        }
    }

    fn escaped(&mut self, stage: Stage, lp: &LinkPath, above: usize, rest: &[String]) -> Outcome {
        // README-style `../../blob/main/docs/x.md`.
        let settings = &self.params.settings;
        if stage == Stage::Relative
            && settings.map_repo_urls
            && let Some(repo) = settings.repo.as_deref()
            && let Some(in_repo) = self.link.source_file.parent().and_then(|d| {
                if d.starts_with(&repo.repo_root) {
                    d.strip_prefix(&repo.repo_root).map(str::to_owned)
                } else {
                    None
                }
            })
        {
            let mut virtual_base = vec!["\u{0}mode".to_owned(), "\u{0}ref".to_owned()];
            virtual_base.extend(
                in_repo
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
            if let Lexical::Inside(v) = normalize_lexically(&virtual_base, &lp.segments)
                && let Some(m) = repo.map_escaped(&v)
            {
                self.step(stage, &lp.text, "leaves the repository through blob/<ref>/");
                return self.mapped(&m, repo);
            }
        }
        // `../vault/x.md`: out of the root and back in by its own name.
        let root = root_components(self.index.root());
        if above <= root.len() && rest.len() > above && rest[..above] == root[root.len() - above..]
        {
            self.note(Note::ReenteredRoot);
            self.step(stage, &lp.text, "climbs out of the root and back in");
            return self.candidates(stage, &rest[above..], lp.trailing_slash);
        }
        let shown = format!("{}{}", "../".repeat(above), rest.join("/"));
        self.step(stage, &lp.text, format!("escapes the root: {shown}"));
        Outcome::Outside(shown)
    }

    fn is_dir_entry(&self, e: &Entry) -> bool {
        match &e.kind {
            EntryKind::Dir => true,
            EntryKind::Symlink(SymlinkTarget::Inside(t)) => self
                .index
                .entry_by_path(t.as_str())
                .is_some_and(|t| t.kind.is_dir()),
            _ => false,
        }
    }

    /// Folders are targets only after every file lookup failed: always for
    /// wikilinks and embeds (they name notes), and per profile otherwise.
    fn folders_after_files(&self) -> bool {
        self.link.kind.is_wiki() || self.params.directory_targets == DirectoryTargets::AfterFiles
    }

    fn kind_ok(&self, e: &Entry, wants_dir: bool, appended: bool) -> bool {
        let is_dir = self.is_dir_entry(e);
        if wants_dir {
            is_dir
        } else if is_dir {
            !appended && !self.folders_after_files()
        } else {
            true
        }
    }

    fn lookup(&self, level: Level, cand: &str) -> Vec<FileId> {
        match level {
            Level::Exact => self.index.id_of(cand).into_iter().collect(),
            Level::Nfc => self.index.by_nfc(&nfc_key(cand)).to_vec(),
            Level::Fold => self.index.by_fold(&fold_key(cand)).to_vec(),
        }
    }

    /// Steps 5 and 6 for one base: candidates × match keys.
    fn candidates(&mut self, stage: Stage, segs: &[String], wants_dir: bool) -> Outcome {
        let base = segs.join("/");
        let mut cands: Vec<(String, bool)> = vec![(base.clone(), false)];
        if !wants_dir && !segs.is_empty() {
            for ext in &self.params.appended_extensions {
                cands.push((format!("{base}{ext}"), true));
            }
        }
        let shown = if base.is_empty() {
            "/".to_owned()
        } else {
            base.clone()
        };
        let building = !self.index.is_complete();
        for level in [Level::Exact, Level::Nfc, Level::Fold] {
            for (cand, appended) in &cands {
                let hits: Vec<&Entry> = self
                    .lookup(level, cand)
                    .into_iter()
                    .filter_map(|id| self.index.get(id))
                    .filter(|e| self.kind_ok(e, wants_dir, *appended))
                    .collect();
                if hits.is_empty() {
                    if level == Level::Exact {
                        if let Some(o) =
                            self.via_symlinked_ancestor(stage, cand, wants_dir, *appended, 0)
                        {
                            return o;
                        }
                        if building {
                            match self.probe_exact(cand, wants_dir, *appended) {
                                ProbeOutcome::Hit(f) => {
                                    self.step(stage, cand, "exists (probe; index building)");
                                    return Outcome::Found(f);
                                }
                                ProbeOutcome::Missing => {}
                                ProbeOutcome::Unknown => {
                                    self.step(stage, cand, "unknown (index building)");
                                    return Outcome::Pending(
                                        "the path index is still building".to_owned(),
                                    );
                                }
                            }
                        }
                    }
                    continue;
                }
                return self.hit(stage, cand, hits, level, *appended);
            }
            if building && level == Level::Exact {
                self.step(
                    stage,
                    &shown,
                    "no exact name; case and NFC variants need the full index",
                );
                return Outcome::Pending("the path index is still building".to_owned());
            }
        }
        let tried: Vec<&str> = cands.iter().map(|(c, _)| c.as_str()).collect();
        let tried = if base.is_empty() {
            "/".to_owned()
        } else {
            tried.join(" | ")
        };
        self.step(stage, tried, "not found");
        Outcome::NotFound
    }

    fn probe_exact(&self, cand: &str, wants_dir: bool, appended: bool) -> ProbeOutcome {
        let Ok(vp) = VaultPath::new(cand) else {
            return ProbeOutcome::Missing;
        };
        match self.probe.probe(&vp) {
            ProbeResult::File if !wants_dir => ProbeOutcome::Hit(Found {
                id: None,
                path: vp,
                kind: TargetKind::File,
                quality: if appended {
                    self.params.appended_extension_quality
                } else {
                    MatchQuality::Exact
                },
                candidates: Vec::new(),
            }),
            // A directory's index files need the full index.
            ProbeResult::Dir if wants_dir || (!appended && !self.folders_after_files()) => {
                ProbeOutcome::Unknown
            }
            ProbeResult::Unknown => ProbeOutcome::Unknown,
            ProbeResult::File | ProbeResult::Dir | ProbeResult::Missing => ProbeOutcome::Missing,
        }
    }

    /// Exact lookups through a symlinked directory inside the root.
    fn via_symlinked_ancestor(
        &mut self,
        stage: Stage,
        cand: &str,
        wants_dir: bool,
        appended: bool,
        hops: usize,
    ) -> Option<Outcome> {
        if !self.index.has_symlinks() || hops >= 8 {
            return None;
        }
        let segs: Vec<&str> = cand.split('/').collect();
        for i in 1..segs.len() {
            let prefix = segs[..i].join("/");
            let Some(e) = self.index.entry_by_path(&prefix) else {
                continue;
            };
            let EntryKind::Symlink(SymlinkTarget::Inside(t)) = &e.kind else {
                continue;
            };
            let rewritten = if t.is_root() {
                segs[i..].join("/")
            } else {
                format!("{}/{}", t.as_str(), segs[i..].join("/"))
            };
            self.step(Stage::Symlink, &prefix, format!("-> {t}"));
            self.note(Note::ViaSymlink);
            if let Some(hit) = self.index.entry_by_path(&rewritten)
                && self.kind_ok(hit, wants_dir, appended)
            {
                return Some(self.hit(stage, &rewritten, vec![hit], Level::Exact, appended));
            }
            return self.via_symlinked_ancestor(stage, &rewritten, wants_dir, appended, hops + 1);
        }
        None
    }

    fn follow(&mut self, e: &Entry) -> Followed {
        match &e.kind {
            EntryKind::File => Followed::Target(Some(e.id), e.path.clone(), TargetKind::File),
            EntryKind::Dir => Followed::Target(Some(e.id), e.path.clone(), TargetKind::Directory),
            EntryKind::Symlink(SymlinkTarget::Inside(t)) => {
                self.step(Stage::Symlink, e.path.as_str(), format!("-> {t}"));
                self.note(Note::ViaSymlink);
                match self.index.entry_by_path(t.as_str()) {
                    Some(te) if te.kind.is_dir() => {
                        Followed::Target(Some(te.id), te.path.clone(), TargetKind::Directory)
                    }
                    Some(te) => Followed::Target(Some(te.id), te.path.clone(), TargetKind::File),
                    None => Followed::Target(None, t.clone(), TargetKind::File),
                }
            }
            EntryKind::Symlink(SymlinkTarget::Outside(p)) => {
                self.step(Stage::Symlink, e.path.as_str(), "points outside the root");
                Followed::Outside(p.display().to_string())
            }
            EntryKind::Symlink(SymlinkTarget::Broken) => {
                self.step(Stage::Symlink, e.path.as_str(), "broken");
                Followed::Broken
            }
        }
    }

    fn index_file(&self, dir: FileId) -> Option<&'a Entry> {
        let index: &'a PathIndex = self.index;
        let kids: Vec<&'a Entry> = index
            .children(dir)
            .iter()
            .filter_map(|&id| index.get(id))
            .filter(|e| !e.kind.is_dir())
            .collect();
        for pattern in &self.params.index_files {
            match pattern {
                IndexFile::Name(n) => {
                    if let Some(e) = kids.iter().find(|e| e.path.file_name() == Some(n)) {
                        return Some(e);
                    }
                    let key = fold_key(n);
                    let mut folded: Vec<&&'a Entry> = kids
                        .iter()
                        .filter(|e| e.path.file_name().map(fold_key) == Some(key.clone()))
                        .collect();
                    folded.sort_by(|a, b| a.path.cmp(&b.path));
                    if let Some(e) = folded.first() {
                        return Some(e);
                    }
                }
                IndexFile::Stem(s) => {
                    let key = fold_key(s);
                    let mut matching: Vec<&&'a Entry> = kids
                        .iter()
                        .filter(|e| {
                            e.path
                                .file_name()
                                .is_some_and(|n| n.contains('.') && fold_key(stem_of(n)) == key)
                        })
                        .collect();
                    matching
                        .sort_by(|a, b| a.fold_key().cmp(b.fold_key()).then(a.path.cmp(&b.path)));
                    if let Some(e) = matching.first() {
                        return Some(e);
                    }
                }
            }
        }
        None
    }

    fn rank(&self, hits: &mut [(&Entry, bool)]) {
        let tb = self.params.tie_break;
        hits.sort_by(|(a, ea), (b, eb)| tb.compare(&tie_key(a, *ea), &tie_key(b, *eb)));
    }

    fn hit(
        &mut self,
        stage: Stage,
        cand: &str,
        hits: Vec<&Entry>,
        level: Level,
        appended: bool,
    ) -> Outcome {
        let cand_nfc = nfc_key(cand).into_owned();
        let mut ranked: Vec<(&Entry, bool)> = hits
            .into_iter()
            .map(|e| (e, e.nfc_key() == cand_nfc))
            .collect();
        // Several hits at one level are NFC or case-fold twins (`Case.md` and
        // `case.md` for `[[CASE]]`). ORACLE-PENDING: reported as ambiguous and
        // ordered by the tie-break.
        self.rank(&mut ranked);
        let chosen = ranked[0].0;
        let candidates: Vec<Candidate> = if ranked.len() > 1 {
            ranked
                .iter()
                .map(|(e, _)| Candidate {
                    id: e.id,
                    path: e.path.clone(),
                    user_ignored: e.user_ignored,
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut quality = MatchQuality::Exact;
        let how = match level {
            Level::Exact => "exact",
            Level::Nfc => {
                self.note(Note::NormalizationDiffers);
                quality = quality.max(self.params.case_only_quality);
                "NFC"
            }
            Level::Fold => {
                if nfc_key(chosen.path.as_str()) == nfc_key(cand) {
                    self.note(Note::NormalizationDiffers);
                } else {
                    self.note(Note::CaseDiffers);
                }
                quality = quality.max(self.params.case_only_quality);
                "case fold"
            }
        };
        if appended {
            self.note(Note::ExtensionAppended);
            quality = quality.max(self.params.appended_extension_quality);
        }
        let mut outcome = format!("found {} ({how})", chosen.path);
        if candidates.len() > 1 {
            outcome.push_str(&format!(", {} candidates", candidates.len()));
        }
        self.step(stage, cand, outcome);
        let (id, path, kind) = match self.follow(chosen) {
            Followed::Target(id, path, kind) => (id, path, kind),
            Followed::Outside(p) => return Outcome::Outside(p),
            Followed::Broken => return Outcome::NotFound,
        };
        if kind == TargetKind::Directory
            && let Some(dir) = id
            && let Some(ix) = self.index_file(dir)
        {
            self.note(Note::DirectoryIndex);
            self.step(stage, path.as_str(), format!("index file {}", ix.path));
            return Outcome::Found(Found {
                id: Some(ix.id),
                path: ix.path.clone(),
                kind: TargetKind::File,
                quality,
                candidates,
            });
        }
        Outcome::Found(Found {
            id,
            path,
            kind,
            quality,
            candidates,
        })
    }

    fn is_conflict_copy(&self, e: &Entry) -> bool {
        let Some(name) = e.path.file_name() else {
            return false;
        };
        let parent = e.path.parent().unwrap_or_else(VaultPath::root);
        detect_sync_conflict(name, |orig| {
            parent
                .join(orig)
                .is_ok_and(|p| self.index.id_of(p.as_str()).is_some())
        })
        .is_some()
    }

    /// Step 7: basename / suffix lookup.
    fn basename(&mut self, lp: &LinkPath) -> Outcome {
        let segs = match normalize_lexically(Vec::<String>::new(), &lp.segments) {
            Lexical::Inside(s) if !s.is_empty() && !lp.trailing_slash => s,
            _ => return Outcome::NotFound,
        };
        let shown = segs.join("/");
        if !self.index.is_complete() {
            self.step(Stage::Basename, format!("{shown:?}"), "index building");
            return Outcome::Pending("basename lookup needs the complete path index".to_owned());
        }
        let n = segs.len();
        let last = &segs[n - 1];
        let link_fold: Vec<String> = segs.iter().map(|s| fold_key(s)).collect();
        let link_nfc: Vec<String> = segs.iter().map(|s| nfc_key(s).into_owned()).collect();
        let mut names: Vec<(String, Option<&str>)> = vec![(fold_key(last), None)];
        for ext in &self.params.appended_extensions {
            names.push((fold_key(&format!("{last}{ext}")), Some(ext.as_str())));
        }
        let mut pool: Vec<(&Entry, bool, bool)> = Vec::new(); // entry, appended, exact case
        for (key, ext) in &names {
            for &id in self.index.by_name_fold(key) {
                let Some(e) = self.index.get(id) else {
                    continue;
                };
                if pool.iter().any(|(p, _, _)| p.id == id) {
                    continue;
                }
                let ef: Vec<&str> = e.fold_key().split('/').collect();
                if ef.len() < n {
                    continue;
                }
                let tail = &ef[ef.len() - n..];
                if tail[..n - 1]
                    .iter()
                    .zip(&link_fold[..n - 1])
                    .any(|(a, b)| a != b)
                    || tail[n - 1] != key
                {
                    continue;
                }
                let en: Vec<&str> = e.nfc_key().split('/').collect();
                let etail = &en[en.len() - n..];
                let mut want: Vec<String> = link_nfc.clone();
                if let Some(ext) = ext {
                    want[n - 1] = nfc_key(&format!("{last}{ext}")).into_owned();
                }
                let exact_case = etail.iter().zip(&want).all(|(a, b)| a == b);
                pool.push((e, ext.is_some(), exact_case));
            }
        }
        let before = pool.len();
        pool.retain(|(e, _, _)| !self.is_conflict_copy(e));
        let excluded = before - pool.len();
        if excluded > 0 {
            self.note(Note::ConflictCopiesExcluded(excluded));
            self.step(
                Stage::Basename,
                format!("{shown:?}"),
                format!("{excluded} sync-conflict copies excluded"),
            );
        }
        if pool.is_empty() {
            self.step(
                Stage::Basename,
                format!("{:?}", link_fold.join("/")),
                "no candidates",
            );
            return Outcome::NotFound;
        }
        let mut ranked: Vec<(&Entry, bool)> = pool.iter().map(|(e, _, x)| (*e, *x)).collect();
        self.rank(&mut ranked);
        let chosen = ranked[0].0;
        let (_, appended, exact_case) = *pool
            .iter()
            .find(|(e, _, _)| e.id == chosen.id)
            .unwrap_or(&pool[0]);
        let mut quality = if exact_case {
            MatchQuality::Exact
        } else {
            self.params.case_only_quality
        };
        if !exact_case {
            self.note(Note::CaseDiffers);
        }
        if appended {
            self.note(Note::ExtensionAppended);
            quality = quality.max(self.params.appended_extension_quality);
        }
        let mut candidates = Vec::new();
        if ranked.len() > 1 {
            self.step(
                Stage::Basename,
                format!("{:?}", link_fold.join("/")),
                format!("{} candidates:", ranked.len()),
            );
            let best_x = ranked[0].1;
            let second = ranked[1];
            let tb = self.params.tie_break;
            let rule = tb
                .decide(&tie_key(chosen, best_x), &tie_key(second.0, second.1))
                .1;
            let lines: Vec<(String, String)> = ranked
                .iter()
                .take(TRACE_CANDIDATES)
                .enumerate()
                .map(|(i, (e, _))| {
                    let mut tail = format!("({} segments)", e.path.depth());
                    if e.user_ignored {
                        tail.push_str(" [userIgnoreFilters]");
                    }
                    if i == 0 {
                        tail.push_str(&format!("  <- chosen ({})", rule.describe()));
                    }
                    (e.path.as_str().to_owned(), tail)
                })
                .collect();
            for (p, t) in lines {
                self.step(Stage::Note, p, t);
            }
            if ranked.len() > TRACE_CANDIDATES {
                self.step(
                    Stage::Note,
                    format!("... and {} more", ranked.len() - TRACE_CANDIDATES),
                    "",
                );
            }
            candidates = ranked
                .iter()
                .map(|(e, _)| Candidate {
                    id: e.id,
                    path: e.path.clone(),
                    user_ignored: e.user_ignored,
                })
                .collect();
        } else {
            self.step(
                Stage::Basename,
                format!("{:?}", link_fold.join("/")),
                format!("found {}", chosen.path),
            );
        }
        match self.follow(chosen) {
            Followed::Target(id, path, kind) => Outcome::Found(Found {
                id,
                path,
                kind,
                quality,
                candidates,
            }),
            Followed::Outside(p) => Outcome::Outside(p),
            Followed::Broken => Outcome::NotFound,
        }
    }
}

fn tie_key(e: &Entry, exact_case: bool) -> TieBreakKey<'_> {
    TieBreakKey {
        path: &e.path,
        nfc: e.nfc_key(),
        fold: e.fold_key(),
        exact_case,
        user_ignored: e.user_ignored,
    }
}

enum ProbeOutcome {
    Hit(Found),
    Missing,
    Unknown,
}

fn describe_url(class: &UrlClass) -> String {
    match class {
        UrlClass::Fragment(s)
        | UrlClass::Relative(s)
        | UrlClass::Mailto(s)
        | UrlClass::Obsidian(s) => s.clone(),
        UrlClass::Web { url, .. }
        | UrlClass::External { url, .. }
        | UrlClass::Unknown { url, .. } => url.clone(),
        UrlClass::Network { url, .. } => url.clone(),
        UrlClass::LocalFile { path, .. } => path.clone(),
        UrlClass::DataImage { mime, .. } => format!("data:{mime}"),
        UrlClass::Data => "data:".to_owned(),
        UrlClass::Script(s) | UrlClass::DangerousHandler(s) => format!("{s}:"),
        UrlClass::Invalid(i) => format!("invalid ({i:?})"),
    }
}

/// A shorthand for the confirm reason of a policy, if any.
#[must_use]
pub fn confirm_reason(policy: Option<UrlPolicy>) -> Option<ConfirmReason> {
    match policy {
        Some(UrlPolicy::Confirm(r)) => Some(r),
        _ => None,
    }
}
