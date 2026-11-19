// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Links in the Reading view: their statuses, what following one does, and
//! where a fragment lands.
//!
//! - [`Resolver::statuses`] is the render tree's [`LinkStatusSource`] over
//!   the window's path index. While the index builds, exact names are
//!   checked on the file system; without the window's vault (the sidebar was
//!   never shown) nothing local is decided: links are `Pending`, never
//!   `Unresolved`, and URLs are still classified.
//! - [`navigation`] resolves a followed link with the same resolver; without
//!   the window's vault it checks the file system directly for this click
//!   only, rooted where the vault would be.
//! - [`fragment_item`] finds the item a fragment names in a loaded
//!   document: headings by the profile's slugs, block ids, line numbers.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gv_model::{BlockMatch, DocModel, FragmentMatch};
use gv_render_tree::{EmbedFetch, ItemId, LinkStatus, LinkStatusSource, LinkTarget, RenderTree};
use gv_vault::url::{UrlClass, UrlPolicy};
use gv_vault::{
    Fragment, FragmentKind, FsProbe, IndexConfig, LinkKind, LinkRef, PathIndex, PlatformRules,
    ProfileId, ProfileParams, Resolution, Status, TargetKind, VaultPath, resolve,
    resolve_with_probe,
};

/// Whether the Reading view opens `path`.
pub(crate) fn is_markdown(path: &Path) -> bool {
    crate::platform::is_markdown(path)
}

/// The resolver's kind of a render-tree link.
pub(crate) fn resolver_kind(kind: gv_model::LinkKind, image: bool) -> LinkKind {
    if kind == gv_model::LinkKind::Wikilink {
        LinkKind::Wikilink
    } else if kind == gv_model::LinkKind::Embed && !image {
        LinkKind::Embed
    } else if image {
        LinkKind::Image
    } else if kind.is_reference() {
        LinkKind::Reference
    } else if kind.is_autolink() {
        LinkKind::Autolink
    } else {
        LinkKind::Inline
    }
}

fn link_status(status: Status) -> LinkStatus {
    match status {
        Status::Exact => LinkStatus::Exact,
        Status::CaseFold => LinkStatus::CaseFold,
        Status::Loose => LinkStatus::Loose,
        Status::Ambiguous(n) => LinkStatus::Ambiguous(u32::try_from(n).unwrap_or(u32::MAX)),
        Status::Pending => LinkStatus::Pending,
        Status::Unresolved => LinkStatus::Unresolved,
        Status::OutsideRoot => LinkStatus::OutsideRoot,
        Status::External => LinkStatus::External,
        Status::Blocked => LinkStatus::Blocked,
    }
}

/// `file` as a vault path below `root`, if it is below it.
fn vault_path(file: &Path, root: &Path) -> Option<VaultPath> {
    let file = std::path::absolute(file).ok()?;
    let rel = file.strip_prefix(root).ok()?;
    VaultPath::from_relative_fs_path(rel).ok()
}

/// Resolves the links of one document.
#[derive(Clone)]
pub(crate) struct Resolver {
    index: PathIndex,
    source: VaultPath,
    /// The document's profile and its resolution parameters.
    profile: ProfileId,
    params: ProfileParams,
    /// Exact-name checks while the index builds.
    probe: Option<FsProbe>,
    /// The window's vault index: without it, local statuses are pending.
    vault: bool,
}

impl fmt::Debug for Resolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolver")
            .field("root", &self.index.root())
            .field("source", &self.source)
            .field("profile", &self.profile)
            .field("complete", &self.index.is_complete())
            .field("vault", &self.vault)
            .finish_non_exhaustive()
    }
}

impl Resolver {
    fn in_vault(file: &Path, index: &PathIndex, profile: ProfileId) -> Option<Self> {
        let source = vault_path(file, index.root())?;
        let probe = (!index.is_complete())
            .then(|| FsProbe::new(index.root().to_path_buf(), PlatformRules::host()));
        Some(Self {
            index: index.clone(),
            source,
            profile,
            params: ProfileParams::for_profile(profile),
            probe,
            vault: true,
        })
    }

    /// The statuses of the links of `file` (in `profile`): through `index`
    /// (the window's vault) when the file is in it; otherwise URLs only.
    pub fn statuses(file: &Path, index: Option<&PathIndex>, profile: ProfileId) -> Self {
        if let Some(r) = index.and_then(|i| Self::in_vault(file, i, profile)) {
            return r;
        }
        let folder = std::path::absolute(file)
            .ok()
            .and_then(|f| f.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| VaultPath::new(n).ok())
            .unwrap_or_default();
        Self {
            index: PathIndex::new_building(folder, IndexConfig::default()),
            source: name,
            profile,
            params: ProfileParams::for_profile(profile),
            probe: None,
            vault: false,
        }
    }

    /// The same resolver for a document in `profile`.
    pub fn with_profile(self, profile: ProfileId) -> Self {
        Self {
            profile,
            params: ProfileParams::for_profile(profile),
            ..self
        }
    }

    /// Follows a link of `file`: through `index` when the file is in it,
    /// otherwise with direct file system checks below the folder the vault
    /// would have.
    pub fn for_click(file: &Path, index: Option<&PathIndex>, profile: ProfileId) -> Self {
        if let Some(r) = index.and_then(|i| Self::in_vault(file, i, profile)) {
            return r;
        }
        let root = gv_vault::detect_root(file).root;
        let source = vault_path(file, &root).unwrap_or_default();
        Self {
            probe: Some(FsProbe::new(root.clone(), PlatformRules::host())),
            index: PathIndex::new_building(root, IndexConfig::default()),
            source,
            profile,
            params: ProfileParams::for_profile(profile),
            vault: true,
        }
    }

    pub fn root(&self) -> &Path {
        self.index.root()
    }

    /// The same resolver for image files: without the window's vault, it
    /// checks the file system below the document's folder (the build runs
    /// off the UI thread).
    pub fn for_images(&self) -> Self {
        if self.vault {
            return self.clone();
        }
        Self {
            probe: Some(FsProbe::new(
                self.index.root().to_path_buf(),
                PlatformRules::host(),
            )),
            vault: true,
            ..self.clone()
        }
    }

    /// The document's place in the vault.
    pub fn source(&self) -> &VaultPath {
        &self.source
    }

    pub fn params(&self) -> &ProfileParams {
        &self.params
    }

    /// The resolver's link for `destination`, from the document.
    pub fn link_ref(&self, kind: LinkKind, destination: &str) -> LinkRef {
        LinkRef::new(kind, destination, self.source.clone(), self.profile)
    }

    /// `path` (a vault path) on the file system.
    pub fn fs_path(&self, path: &VaultPath) -> PathBuf {
        path.to_fs_path(self.index.root())
    }

    pub fn resolve(&self, kind: LinkKind, destination: &str) -> Resolution {
        self.resolve_from(kind, destination, None)
    }

    /// [`Self::resolve`] for a link of the note `from` (a vault path: a
    /// link inside a note embed), or of the document (`None`).
    pub fn resolve_from(
        &self,
        kind: LinkKind,
        destination: &str,
        from: Option<&str>,
    ) -> Resolution {
        let source = from
            .and_then(|f| VaultPath::new(f).ok())
            .unwrap_or_else(|| self.source.clone());
        let link = LinkRef::new(kind, destination, source, self.profile);
        match &self.probe {
            Some(probe) => resolve_with_probe(&self.index, &link, &self.params, probe),
            None => resolve(&self.index, &link, &self.params),
        }
    }
}

impl Resolver {
    /// Resolves an embed (`![[destination]]`) from the document, or from
    /// the embedding note `from` (a vault path) inside a note embed.
    /// Without the window's vault, the files below the document's folder
    /// are checked directly.
    pub fn resolve_embed(&self, destination: &str, from: Option<&str>) -> Resolution {
        let source = from
            .and_then(|f| VaultPath::new(f).ok())
            .unwrap_or_else(|| self.source.clone());
        let link = LinkRef::new(LinkKind::Embed, destination, source, self.profile);
        let mut res = match &self.probe {
            Some(probe) => resolve_with_probe(&self.index, &link, &self.params, probe),
            None => resolve(&self.index, &link, &self.params),
        };
        if !self.vault && !res.status.is_resolved() {
            let probe = FsProbe::new(self.index.root().to_path_buf(), PlatformRules::host());
            res = resolve_with_probe(&self.index, &link, &self.params, &probe);
        }
        res
    }

    /// The note a note embed shows, or what it shows without one.
    fn embed_target(&self, destination: &str, from: Option<&str>) -> Result<VaultPath, EmbedFetch> {
        let res = self.resolve_embed(destination, from);
        if res.status == Status::Pending {
            return Err(EmbedFetch::Pending);
        }
        match (res.target_path, res.target_kind) {
            (Some(target), Some(TargetKind::File))
                if res.status.is_resolved()
                    && target
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("md")) =>
            {
                Ok(target)
            }
            _ => Err(EmbedFetch::Unavailable),
        }
    }

    /// The file of the note a note embed (`![[destination]]`, inside the
    /// embedded note `from`) shows, if it shows one.
    pub fn embed_file(&self, destination: &str, from: Option<&str>) -> Option<PathBuf> {
        let target = self.embed_target(destination, from).ok()?;
        Some(target.to_fs_path(self.index.root()))
    }

    /// Whether the status of `link` can change when the index does. URLs
    /// are classified by their text alone, except web URLs this vault maps
    /// to its own files; local paths, fragments, `file:` and `obsidian:`
    /// links are looked up.
    pub fn depends_on_index(&self, link: &LinkTarget<'_>) -> bool {
        match gv_vault::url::classify(link.destination) {
            UrlClass::Web { .. } => {
                let settings = &self.params.settings;
                settings.map_repo_urls && settings.repo.is_some()
            }
            UrlClass::Mailto(_)
            | UrlClass::DataImage { .. }
            | UrlClass::Data
            | UrlClass::Network { .. }
            | UrlClass::Script(_)
            | UrlClass::DangerousHandler(_)
            | UrlClass::External { .. }
            | UrlClass::Unknown { .. }
            | UrlClass::Invalid(_) => false,
            _ => true,
        }
    }
}

impl LinkStatusSource for Resolver {
    fn status(&self, link: &LinkTarget<'_>) -> LinkStatus {
        let res = self.resolve_from(
            resolver_kind(link.kind, link.image),
            link.destination,
            link.from,
        );
        match link_status(res.status) {
            // Without the index nothing local is known to be missing.
            LinkStatus::Unresolved if !self.vault => LinkStatus::Pending,
            status => status,
        }
    }

    /// A note embed's note: resolved from the document, or from the
    /// embedding note `from` inside an embed; its unsaved buffer, else the
    /// file ([`super::embed_texts::text`]: reads the disk, so trees are
    /// built off the UI thread).
    fn embed(&self, destination: &str, from: Option<&str>) -> EmbedFetch {
        let target = match self.embed_target(destination, from) {
            Ok(target) => target,
            Err(fetch) => return fetch,
        };
        let path = target.to_fs_path(self.index.root());
        let Some(text) = super::embed_texts::text(&path) else {
            return EmbedFetch::Unavailable;
        };
        let title = target
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_end_matches(".md")
            .to_owned();
        EmbedFetch::Note {
            key: target.as_str().into(),
            title: title.into(),
            text,
        }
    }

    fn document_key(&self) -> Option<(Arc<str>, Arc<str>)> {
        let key = self.source.as_str();
        let title = key.rsplit('/').next().unwrap_or("").trim_end_matches(".md");
        (!key.is_empty()).then(|| (Arc::from(key), Arc::from(title)))
    }
}

/// What following a link does.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Navigation {
    /// Open a Markdown file, scrolled to the fragment.
    Open {
        path: PathBuf,
        fragment: Option<Fragment>,
    },
    /// Scroll the current document to the fragment (its top without one).
    Here { fragment: Option<Fragment> },
    /// Hand the URL to the operating system.
    External(String),
    /// Hand the URL to the operating system once the user confirmed it:
    /// `why` says where it goes, and the full URL is shown with it.
    Confirm { url: String, why: String },
    /// A file Glyphvane does not show: the window decides by its kind
    /// (open with the default app after confirmation, or, for a program,
    /// only reveal it).
    OpenFile(PathBuf),
    /// An ambiguous link: open `path` (the best candidate, the one the
    /// tie-break chose) and say which notes match (`candidates`, best
    /// first, on the file system).
    Ambiguous {
        path: PathBuf,
        fragment: Option<Fragment>,
        candidates: Vec<PathBuf>,
    },
    /// No target: the note following would create, or `None` when there
    /// is nothing to create (a non-note file, a path outside the root, a
    /// name with nothing left after sanitising).
    Unresolved {
        name: String,
        create: Option<NewNote>,
    },
    /// The vault index is still building: the target cannot be decided
    /// yet.
    Pending,
    /// Nothing happens, for this reason (logged).
    Refused(String),
}

/// The note an unresolved link creates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewNote {
    /// Below the link root.
    pub path: VaultPath,
    /// On the file system.
    pub file: PathBuf,
    /// When the name written in the link is not valid on every platform:
    /// the path part the link is rewritten to first, so that it resolves
    /// to the sanitised note.
    pub rewrite: Option<String>,
}

/// The note an unresolved link of kind `kind` creates (09 § Unresolved):
/// in the folder written in the link; for a bare name, where the vault's
/// `newFileLocation` says, otherwise the source's folder. A name that is
/// not valid on every platform is sanitised as the quick switcher does,
/// and the link is to be rewritten to it.
pub(crate) fn new_note(resolver: &Resolver, kind: LinkKind, destination: &str) -> Option<NewNote> {
    let params = resolver.params();
    let link = resolver.link_ref(kind, destination);
    let gv_vault::link::DecodedTarget::Path(written) = gv_vault::link::decode(&link, params).target
    else {
        return None;
    };
    let bare = written.segments.len() == 1 && !written.absolute && !written.is_dot_relative();
    let place = |path: VaultPath| -> Option<VaultPath> {
        if !bare {
            return Some(path);
        }
        let location = gv_vault::new_file::NewFileLocation::read(resolver.root());
        gv_vault::new_file::place_bare_link_note(location.as_ref(), path)
    };
    let make = |path: VaultPath, rewrite: Option<String>| {
        let path = place(path)?;
        Some(NewNote {
            file: resolver.fs_path(&path),
            path,
            rewrite,
        })
    };
    let source = resolver.source().clone();
    if let Some(path) = gv_vault::content::check::note_path_for_link(&link, params, &source) {
        return make(path, None);
    }
    if written.text.is_empty() || written.trailing_slash {
        return None;
    }
    // A name some platform does not allow: the switcher's sanitising, and
    // the link rewritten to match.
    let cleaned = crate::switcher::model::new_note_path(&written.text)?;
    let wiki = link.kind.is_wiki();
    let had_extension = written
        .segments
        .last()
        .is_some_and(|s| s.to_lowercase().ends_with(".md"));
    let mut text = if wiki && !had_extension {
        cleaned.trim_end_matches(".md").to_owned()
    } else {
        cleaned
    };
    if !wiki {
        text = text.replace('%', "%25").replace(' ', "%20");
    }
    if text == written.text {
        return None;
    }
    let rest = &destination[link_name_end(wiki, destination)..];
    let rewritten = resolver.link_ref(kind, &format!("{text}{rest}"));
    let path = gv_vault::content::check::note_path_for_link(&rewritten, params, &source)?;
    make(path, Some(text))
}

/// Where the path part of a destination ends.
fn link_name_end(wiki: bool, destination: &str) -> usize {
    let end = if wiki {
        destination.find(['#', '|'])
    } else {
        destination.find(['#', '?'])
    };
    end.unwrap_or(destination.len())
}

fn same_file(a: &Path, b: &Path) -> bool {
    let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    if abs(a) == abs(b) {
        return true;
    }
    matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// What following the link `destination` (of `kind`) in `file`, a
/// document in `profile`, does.
pub(crate) fn navigation(
    file: &Path,
    index: Option<&PathIndex>,
    profile: ProfileId,
    kind: gv_model::LinkKind,
    destination: &str,
) -> Navigation {
    let resolver = Resolver::for_click(file, index, profile);
    let res = resolver.resolve(resolver_kind(kind, false), destination);
    let refused = |why: &str| Navigation::Refused(format!("{destination:?}: {why}"));
    match res.status {
        Status::External => match (&res.url, res.policy) {
            (Some(UrlClass::Web { url, .. } | UrlClass::Mailto(url)), Some(UrlPolicy::Allow)) => {
                Navigation::External(url.clone())
            }
            (Some(UrlClass::LocalFile { path, .. }), Some(UrlPolicy::Confirm(_))) => {
                let path = PathBuf::from(path);
                if is_markdown(&path) {
                    Navigation::Open {
                        path,
                        fragment: res.fragment,
                    }
                } else {
                    Navigation::OpenFile(path)
                }
            }
            (Some(class), Some(UrlPolicy::Confirm(reason))) => match handed_off(class) {
                Some((url, why)) => Navigation::Confirm { url, why },
                None => refused(&format!("cannot be handed to the system ({reason:?})")),
            },
            _ => refused("only http, https and mailto URLs are handed to the system"),
        },
        Status::Blocked => {
            let reason = res.block.map(|b| b.to_string()).unwrap_or_default();
            refused(&format!("blocked: {reason}"))
        }
        Status::Ambiguous(_) => match (&res.target_path, res.target_kind) {
            (Some(target), Some(TargetKind::File)) if is_markdown(Path::new(target.as_str())) => {
                Navigation::Ambiguous {
                    path: target.to_fs_path(resolver.root()),
                    fragment: res.fragment,
                    candidates: res
                        .candidates
                        .iter()
                        .map(|c| c.path.to_fs_path(resolver.root()))
                        .collect(),
                }
            }
            (Some(target), Some(TargetKind::File)) => {
                Navigation::OpenFile(target.to_fs_path(resolver.root()))
            }
            _ => refused("no target"),
        },
        status if status.is_resolved() => match (&res.target_path, res.target_kind) {
            (Some(target), Some(TargetKind::File)) => {
                let path = target.to_fs_path(resolver.root());
                if same_file(&path, file) {
                    Navigation::Here {
                        fragment: res.fragment,
                    }
                } else if is_markdown(&path) {
                    Navigation::Open {
                        path,
                        fragment: res.fragment,
                    }
                } else {
                    // The built-in viewers come later (M4).
                    Navigation::OpenFile(path)
                }
            }
            (Some(_), Some(TargetKind::Directory)) => {
                refused("a folder; revealing it in the file tree comes later")
            }
            _ => refused("no target"),
        },
        Status::Pending => Navigation::Pending,
        Status::Unresolved => Navigation::Unresolved {
            name: super::link_info::link_name(kind, destination),
            create: new_note(&resolver, resolver_kind(kind, false), destination),
        },
        Status::OutsideRoot => refused("outside the vault root; following it comes later"),
        _ => refused("not followed"),
    }
}

/// The URL a confirmed link hands to the system, and the sentence that
/// says where it goes.
fn handed_off(class: &UrlClass) -> Option<(String, String)> {
    let (url, why) = match class {
        UrlClass::External { scheme, url } | UrlClass::Unknown { scheme, url } => (
            url.clone(),
            format!("It opens in the app your system uses for “{scheme}:” links:"),
        ),
        UrlClass::Obsidian(url) => (
            url.clone(),
            "It points to another Obsidian vault or action and opens in Obsidian:".to_owned(),
        ),
        UrlClass::Network { url, .. } => (
            url.clone(),
            "It points to a network share. Opening it can send your sign-in to that computer:"
                .to_owned(),
        ),
        _ => return None,
    };
    Some((url, why))
}

/// The byte offset of 0-based line `line` (LF, CRLF and CR end lines).
pub(crate) fn line_offset(text: &str, line: u32) -> usize {
    let bytes = text.as_bytes();
    let mut seen = 0;
    let mut i = 0;
    while seen < line && i < bytes.len() {
        match bytes[i] {
            b'\n' => seen += 1,
            b'\r' if bytes.get(i + 1) != Some(&b'\n') => seen += 1,
            _ => {}
        }
        i += 1;
    }
    i
}

/// The item a fragment names in a loaded document; `None` when it names
/// nothing there ("heading not found").
pub(crate) fn fragment_item(
    fragment: &Fragment,
    params: &ProfileParams,
    model: &DocModel,
    tree: &RenderTree,
    text: &str,
) -> Option<usize> {
    let at = |offset: usize| tree.item_at(offset).or(tree.items().len().checked_sub(1));
    match &fragment.kind {
        FragmentKind::Top | FragmentKind::Viewer(_) => Some(0),
        FragmentKind::Heading(parts) => match model.match_fragment(parts, params.anchoring) {
            FragmentMatch::Top => Some(0),
            FragmentMatch::Heading { index, .. } => {
                let h = model.headings().get(index)?;
                tree.index_of(ItemId {
                    block: h.block,
                    chunk: 0,
                })
                .or_else(|| at(h.range.start))
            }
            FragmentMatch::Missing => None,
        },
        FragmentKind::Block(id) => match model.match_block(id) {
            BlockMatch::Found { index, .. } => at(model.block_anchors().get(index)?.range.start),
            BlockMatch::Missing => None,
        },
        FragmentKind::Lines { start, .. } => at(line_offset(text, start.saturating_sub(1))),
    }
}

/// The section of the heading a fragment names: from the heading to the
/// next heading of the same or a higher level. `None` for a fragment that
/// names no heading.
pub(crate) fn fragment_section(
    fragment: &Fragment,
    params: &ProfileParams,
    model: &DocModel,
) -> Option<std::ops::Range<usize>> {
    let FragmentKind::Heading(parts) = &fragment.kind else {
        return None;
    };
    match model.match_fragment(parts, params.anchoring) {
        FragmentMatch::Heading { index, .. } => model.section(index),
        FragmentMatch::Top | FragmentMatch::Missing => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(destination: &str) -> LinkTarget<'_> {
        LinkTarget {
            kind: gv_model::LinkKind::Inline,
            image: false,
            destination,
            title: None,
            range: 0..1,
            block: gv_syntax::BlockId(0),
            from: None,
        }
    }

    #[test]
    fn without_a_vault_local_links_are_pending_and_urls_classified() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("a.md");
        std::fs::write(&file, "x").expect("write");
        let r = Resolver::statuses(&file, None, ProfileId::CommonMark);
        for local in ["b.md", "./b.md", "/b.md", "missing", "a.md#x", "#x"] {
            let status = r.status(&target(local));
            assert!(
                matches!(status, LinkStatus::Pending | LinkStatus::Exact),
                "{local}: {status}"
            );
            assert_ne!(status, LinkStatus::Unresolved);
        }
        assert_eq!(
            r.status(&target("https://example.com")),
            LinkStatus::External
        );
        assert_eq!(
            r.status(&target("javascript:alert(1)")),
            LinkStatus::Blocked
        );
    }

    #[test]
    fn a_click_without_a_vault_checks_the_file_system() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join("sub")).expect("dir");
        let file = dir.path().join("a.md");
        for f in ["a.md", "b.md", "sub/c.md", "pic.png"] {
            std::fs::write(dir.path().join(f), "# Title\n").expect("write");
        }
        let go = |dest: &str| {
            navigation(
                &file,
                None,
                ProfileId::CommonMark,
                gv_model::LinkKind::Inline,
                dest,
            )
        };
        assert!(matches!(go("b.md"), Navigation::Open { path, .. } if path.ends_with("b.md")));
        assert!(
            matches!(go("sub/c.md#title"), Navigation::Open { path, fragment: Some(f) }
                if path.ends_with("sub/c.md") && f.text == "title")
        );
        assert!(matches!(
            go("#title"),
            Navigation::Here { fragment: Some(_) }
        ));
        assert!(matches!(go("a.md"), Navigation::Here { fragment: None }));
        assert_eq!(
            go("https://example.com/x"),
            Navigation::External("https://example.com/x".into())
        );
        assert!(matches!(go("mailto:a@b.c"), Navigation::External(_)));
        assert!(matches!(go("javascript:x"), Navigation::Refused(_)));
        assert!(!matches!(go("sub"), Navigation::Open { .. }));
        // A missing note: without the index it may be anywhere in the
        // vault, so it is pending (no create offer).
        assert_eq!(go("missing.md"), Navigation::Pending);
        // Other files and other schemes wait for the user.
        assert!(matches!(go("pic.png"), Navigation::OpenFile(path) if path.ends_with("pic.png")));
        assert!(matches!(go("ssh://host"), Navigation::Confirm { url, why }
                if url == "ssh://host" && why.contains("“ssh:”")));
        assert!(matches!(go("weird-scheme:x"), Navigation::Confirm { .. }));
    }

    #[test]
    fn unresolved_links_create_notes_where_the_plan_says() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join(".obsidian")).expect("marker");
        std::fs::create_dir_all(dir.path().join("sub")).expect("folder");
        let file = dir.path().join("sub/a.md");
        std::fs::write(&file, "x").expect("write");
        let (index, _) = PathIndex::build(dir.path(), IndexConfig::default()).expect("index");
        let r = Resolver::for_click(&file, Some(&index), ProfileId::Obsidian);
        let make = |dest: &str| new_note(&r, LinkKind::Wikilink, dest);
        // A bare name: next to the source without a setting.
        assert_eq!(
            make("New").map(|n| n.path.as_str().to_owned()).as_deref(),
            Some("sub/New.md")
        );
        // A folder written in the link: from the vault root.
        assert_eq!(
            make("Notes/New#H")
                .map(|n| n.path.as_str().to_owned())
                .as_deref(),
            Some("Notes/New.md")
        );
        // A name some platform does not allow: sanitised, with a rewrite.
        let odd = make("A: b?|alias").expect("a note");
        assert_eq!(odd.path.as_str(), "sub/A b.md");
        assert_eq!(odd.rewrite.as_deref(), Some("A b"));
        // `newFileLocation`.
        let app = dir.path().join(".obsidian/app.json");
        std::fs::write(&app, r#"{"newFileLocation":"root"}"#).expect("write");
        assert_eq!(
            make("New").map(|n| n.path.as_str().to_owned()).as_deref(),
            Some("New.md")
        );
        std::fs::write(
            &app,
            r#"{"newFileLocation": "folder", "newFileFolderPath": "In\/box"}"#,
        )
        .expect("write");
        assert_eq!(
            make("New").map(|n| n.path.as_str().to_owned()).as_deref(),
            Some("In/box/New.md")
        );
        // Nothing to create for a URL.
        assert_eq!(new_note(&r, LinkKind::Inline, "https://e.x/"), None);
    }

    #[test]
    fn line_offsets_follow_every_line_ending() {
        let text = "a\nb\r\nc\rd";
        assert_eq!(line_offset(text, 0), 0);
        assert_eq!(line_offset(text, 1), 2);
        assert_eq!(line_offset(text, 2), 5);
        assert_eq!(line_offset(text, 3), 7);
        assert_eq!(line_offset(text, 9), text.len());
    }
}
