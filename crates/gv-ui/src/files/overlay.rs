// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Unsaved buffers for the content index (the dirty-buffer overlay).
//!
//! A document's worker ([`super::worker`]) reports its text after every
//! batch of edits ([`dirty`]) and reports it clean again after a save, a
//! reload, a discard or when the document closes ([`clean`]). Every
//! window's content index whose vault holds the file gets the text, so
//! unsaved headings and links count until the buffer is saved, and note
//! embeds of the file show it ([`unsaved`]).
//!
//! The registry is process-wide: workers run on the background executor
//! without an app context, and a file may be open in one window while
//! another window's vault holds it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use gv_text::TextSnapshot;
use gv_vault::VaultPath;
use gv_vault::content::ContentHandle;

#[derive(Default)]
struct Hub {
    /// Unsaved texts by absolute path.
    dirty: HashMap<PathBuf, TextSnapshot>,
    /// Content indexes by vault root.
    sinks: Vec<(PathBuf, ContentHandle)>,
}

static HUB: LazyLock<Mutex<Hub>> = LazyLock::new(Mutex::default);

fn hub() -> MutexGuard<'static, Hub> {
    HUB.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `path` relative to `root`, as a vault path.
fn vault_path(root: &Path, path: &Path) -> Option<VaultPath> {
    VaultPath::from_relative_fs_path(path.strip_prefix(root).ok()?).ok()
}

/// Feeds the content index of the vault at `root` with the unsaved
/// buffers in it, now and from now on, until it stops.
pub(crate) fn register(root: &Path, handle: ContentHandle) {
    let mut hub = hub();
    hub.sinks.retain(|(_, h)| !h.is_stopped());
    for (path, text) in &hub.dirty {
        if let Some(vp) = vault_path(root, path) {
            handle.set_overlay(vp, text.clone());
        }
    }
    hub.sinks.push((root.to_path_buf(), handle));
}

/// The content index of the vault that holds `path` (the deepest one when
/// vaults nest), with its root: its current snapshot, once built. Editor
/// autocomplete reads the vault's notes, headings and tags from it.
pub(crate) fn content_of(path: &Path) -> Option<(PathBuf, gv_vault::content::ContentSnapshot)> {
    let mut hub = hub();
    hub.sinks.retain(|(_, h)| !h.is_stopped());
    hub.sinks
        .iter()
        .filter(|(root, _)| path.starts_with(root))
        .max_by_key(|(root, _)| root.components().count())
        .and_then(|(root, handle)| Some((root.clone(), handle.snapshot()?)))
}

/// The document at `path` has unsaved text `text`.
pub(crate) fn dirty(path: &Path, text: &TextSnapshot) {
    let mut hub = hub();
    hub.sinks.retain(|(_, h)| !h.is_stopped());
    for (root, handle) in &hub.sinks {
        if let Some(vp) = vault_path(root, path) {
            handle.set_overlay(vp, text.clone());
        }
    }
    hub.dirty.insert(path.to_path_buf(), text.clone());
    drop(hub);
    // Embeds of the note show the buffer.
    crate::reader::embed_texts::changed(path);
}

/// The unsaved text of the document at `path`, if it has one.
pub(crate) fn unsaved(path: &Path) -> Option<TextSnapshot> {
    hub().dirty.get(path).cloned()
}

/// The document at `path` matches the disk again, or closed.
pub(crate) fn clean(path: &Path) {
    let mut hub = hub();
    if hub.dirty.remove(path).is_none() {
        return;
    }
    hub.sinks.retain(|(_, h)| !h.is_stopped());
    for (root, handle) in &hub.sinks {
        if let Some(vp) = vault_path(root, path) {
            handle.clear_overlay(vp);
        }
    }
    drop(hub);
    // Embeds of the note show the file again.
    crate::reader::embed_texts::changed(path);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gv_vault::content::{ContentIndexer, ContentOptions, RecordKind};
    use gv_vault::{IndexConfig, PathIndex, ProfileId};

    use super::*;

    #[test]
    fn dirty_buffers_reach_the_index_of_their_vault() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("A.md"), "a\n").expect("write");
        std::fs::write(root.join("B.md"), "b\n").expect("write");
        let paths = PathIndex::build(&root, IndexConfig::default())
            .expect("index")
            .0;
        let mut options = ContentOptions::for_profile(ProfileId::Obsidian);
        options.overlay_debounce = Duration::ZERO;
        let indexer = ContentIndexer::start(paths, options, |_| {}).expect("starts");

        // Unsaved before the index started, and after.
        dirty(&root.join("A.md"), &TextSnapshot::new("[[B]]\n"));
        register(&root, indexer.handle());
        dirty(
            &dir.path().join("elsewhere.md"),
            &TextSnapshot::new("[[B]]\n"),
        );
        assert!(indexer.flush(Duration::from_secs(10)));
        let s = indexer.snapshot().expect("snapshot");
        let b = VaultPath::new("B.md").expect("path");
        let a = VaultPath::new("A.md").expect("path");
        assert_eq!(s.backlink_sources(&b), vec![a.clone()]);
        assert_eq!(s.note(&a).map(|r| r.kind), Some(RecordKind::Overlay));

        clean(&root.join("A.md"));
        clean(&dir.path().join("elsewhere.md"));
        assert!(indexer.flush(Duration::from_secs(10)));
        let s = indexer.snapshot().expect("snapshot");
        assert!(s.backlink_sources(&b).is_empty());
        let handle = indexer.handle();
        drop(indexer);
        assert!(handle.is_stopped());
        // A stopped index is dropped from the registry.
        dirty(&root.join("A.md"), &TextSnapshot::new("x\n"));
        assert!(!hub().sinks.iter().any(|(r, _)| r == &root));
        clean(&root.join("A.md"));
    }
}
