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

use gv_model::{BlockMatch, DocModel, FragmentMatch};
use gv_render_tree::{ItemId, LinkStatus, LinkStatusSource, LinkTarget, RenderTree};
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
fn resolver_kind(kind: gv_model::LinkKind, image: bool) -> LinkKind {
    if image {
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

    pub fn resolve(&self, kind: LinkKind, destination: &str) -> Resolution {
        let link = LinkRef::new(kind, destination, self.source.clone(), self.profile);
        match &self.probe {
            Some(probe) => resolve_with_probe(&self.index, &link, &self.params, probe),
            None => resolve(&self.index, &link, &self.params),
        }
    }
}

impl Resolver {
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
        let res = self.resolve(resolver_kind(link.kind, link.image), link.destination);
        match link_status(res.status) {
            // Without the index nothing local is known to be missing.
            LinkStatus::Unresolved if !self.vault => LinkStatus::Pending,
            status => status,
        }
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
    /// Nothing happens, for this reason (logged).
    Refused(String),
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
        Status::Pending => refused("the vault index is still building"),
        Status::Unresolved => refused("no such note; creating it comes later"),
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
        for refused in ["missing.md", "javascript:x", "sub"] {
            assert!(matches!(go(refused), Navigation::Refused(_)), "{refused}");
        }
        // Other files and other schemes wait for the user.
        assert!(matches!(go("pic.png"), Navigation::OpenFile(path) if path.ends_with("pic.png")));
        assert!(matches!(go("ssh://host"), Navigation::Confirm { url, why }
                if url == "ssh://host" && why.contains("“ssh:”")));
        assert!(matches!(go("weird-scheme:x"), Navigation::Confirm { .. }));
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
