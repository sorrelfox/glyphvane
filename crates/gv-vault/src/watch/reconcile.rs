// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Turning what is on disk into index [`Change`]s.
//!
//! Events are only hints: every path they name is looked up on disk again
//! and compared with the index, so the order and grouping of events do not
//! matter and a rescan is the same computation over every path.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::index::{Change, EntryKind, FileMeta, IndexError, PathIndex};
use crate::path::VaultPath;

/// A path as found on disk: its kind and metadata, or `None` when it does
/// not exist (or is excluded).
pub(crate) type Disk = BTreeMap<VaultPath, Option<(EntryKind, FileMeta)>>;

/// The result of [`plan`].
#[derive(Debug)]
pub(crate) struct Plan {
    /// The changes, in the order applied: renames, removals, upserts.
    pub(crate) changes: Vec<Change>,
    /// The index with the changes applied.
    pub(crate) index: PathIndex,
    /// Every move (explicit or matched by file identity), as applied.
    pub(crate) moves: Vec<(VaultPath, VaultPath)>,
    /// Files that were not in the index before (by path).
    pub(crate) new_files: Vec<VaultPath>,
}

/// Computes the changes that make `base` agree with `disk` for the paths
/// `disk` names, given `renames` that are known to have happened (checked
/// against the disk by the caller). Paths absent from `disk` are untouched.
///
/// - A rename onto an existing file is a replacement: the target keeps its
///   [`FileId`](crate::FileId) and the source is removed. That is what an
///   atomic save (write a temporary file, rename it over the original)
///   looks like.
/// - A removed entry and a new entry with the same file identity and kind
///   are a move ([`Change::Rename`]), so ids survive renames that were seen
///   as a removal and a creation (polling, rescans, unmatched events).
/// - Unchanged paths produce no change.
pub(crate) fn plan(
    base: &PathIndex,
    renames: &[(VaultPath, VaultPath)],
    disk: &Disk,
) -> Result<Plan, IndexError> {
    let mut work = base.clone();
    let mut changes = Vec::new();
    let mut moves = Vec::new();

    // 1. Renames reported by the watcher.
    for (from, to) in renames {
        if from == to
            || from.is_root()
            || to.is_root()
            || to.starts_with(from)
            || from.starts_with(to)
        {
            continue;
        }
        let Some(source) = work.entry_by_path(from.as_str()) else {
            continue;
        };
        let replaces_file = !source.kind.is_dir()
            && work
                .entry_by_path(to.as_str())
                .is_some_and(|t| !t.kind.is_dir());
        let change = if replaces_file {
            Change::Remove { path: from.clone() }
        } else {
            Change::Rename {
                from: from.clone(),
                to: to.clone(),
            }
        };
        work = work.apply(std::slice::from_ref(&change))?;
        if !replaces_file {
            moves.push((from.clone(), to.clone()));
        }
        changes.push(change);
    }

    // 2. Moves recognised by file identity.
    let removed = top_most(
        disk.iter()
            .filter(|(p, d)| d.is_none() && work.entry_by_path(p.as_str()).is_some())
            .map(|(p, _)| p.clone()),
    );
    let added: Vec<VaultPath> = {
        let added: HashSet<&VaultPath> = disk
            .iter()
            .filter(|(p, d)| {
                d.is_some() && !p.is_root() && work.entry_by_path(p.as_str()).is_none()
            })
            .map(|(p, _)| p)
            .collect();
        added
            .iter()
            .filter(|p| p.parent().is_none_or(|parent| !added.contains(&parent)))
            .map(|p| (*p).clone())
            .collect()
    };
    if !removed.is_empty() && !added.is_empty() {
        let mut by_identity: HashMap<(crate::index::FileIdentity, bool), Vec<&VaultPath>> =
            HashMap::new();
        for a in &added {
            if let Some(Some((kind, meta))) = disk.get(a)
                && let Some(id) = meta.identity
            {
                by_identity.entry((id, kind.is_dir())).or_default().push(a);
            }
        }
        let mut identity_moves = Vec::new();
        let mut removed_ids: HashMap<(crate::index::FileIdentity, bool), usize> = HashMap::new();
        let removed_entries: Vec<(VaultPath, (crate::index::FileIdentity, bool))> = removed
            .iter()
            .filter_map(|r| {
                let e = work.entry_by_path(r.as_str())?;
                Some((r.clone(), (e.meta.identity?, e.kind.is_dir())))
            })
            .collect();
        for (_, key) in &removed_entries {
            *removed_ids.entry(*key).or_default() += 1;
        }
        for (r, key) in removed_entries {
            if removed_ids.get(&key) != Some(&1) {
                continue;
            }
            if let Some([a]) = by_identity.get(&key).map(Vec::as_slice)
                && !a.starts_with(&r)
                && !r.starts_with(a)
            {
                identity_moves.push(Change::Rename {
                    from: r,
                    to: (*a).clone(),
                });
            }
        }
        for change in identity_moves {
            work = work.apply(std::slice::from_ref(&change))?;
            if let Change::Rename { from, to } = &change {
                moves.push((from.clone(), to.clone()));
            }
            changes.push(change);
        }
    }

    // 3. Removals, top-most only (a directory takes its subtree).
    let removals: Vec<Change> = top_most(
        disk.iter()
            .filter(|(p, d)| {
                d.is_none() && !p.is_root() && work.entry_by_path(p.as_str()).is_some()
            })
            .map(|(p, _)| p.clone()),
    )
    .into_iter()
    .map(|path| Change::Remove { path })
    .collect();
    if !removals.is_empty() {
        work = work.apply(&removals)?;
        changes.extend(removals);
    }

    // 4. New entries and changed metadata, parents first.
    let mut new_files = Vec::new();
    let mut upserts = Vec::new();
    for (path, d) in disk {
        let Some((kind, meta)) = d else { continue };
        if path.is_root() {
            continue;
        }
        match work.entry_by_path(path.as_str()) {
            Some(e) if e.kind == *kind && e.meta == *meta => continue,
            Some(_) => {}
            None => {
                if base.entry_by_path(path.as_str()).is_none() && !kind.is_dir() {
                    new_files.push(path.clone());
                }
            }
        }
        upserts.push(Change::Upsert {
            path: path.clone(),
            kind: kind.clone(),
            meta: meta.clone(),
        });
    }
    if !upserts.is_empty() {
        work = work.apply(&upserts)?;
        changes.extend(upserts);
    }

    Ok(Plan {
        changes,
        index: work,
        moves,
        new_files,
    })
}

/// The paths of `paths` that have no ancestor in the set, sorted.
fn top_most(paths: impl Iterator<Item = VaultPath>) -> Vec<VaultPath> {
    let mut all: Vec<VaultPath> = paths.collect();
    all.sort();
    all.dedup();
    let set: HashSet<VaultPath> = all.iter().cloned().collect();
    all.into_iter()
        .filter(|p| {
            let mut cur = p.parent();
            while let Some(parent) = cur {
                if parent.is_root() {
                    break;
                }
                if set.contains(&parent) {
                    return false;
                }
                cur = parent.parent();
            }
            true
        })
        .collect()
}

/// The disk state of every entry of `fresh` (a new walk), plus `None` for
/// every entry of `base` the walk no longer found: the input of [`plan`]
/// for a full rescan.
pub(crate) fn rescan_disk(base: &PathIndex, fresh: &PathIndex) -> Disk {
    let mut disk = Disk::new();
    for e in fresh.iter() {
        if !e.path.is_root() {
            disk.insert(e.path.clone(), Some((e.kind.clone(), e.meta.clone())));
        }
    }
    for e in base.iter() {
        if !e.path.is_root() && fresh.entry_by_path(e.path.as_str()).is_none() {
            disk.insert(e.path.clone(), None);
        }
    }
    disk
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FileIdentity;

    fn vp(s: &str) -> VaultPath {
        VaultPath::new(s).expect("valid path")
    }

    fn file(ino: u64, size: u64) -> Option<(EntryKind, FileMeta)> {
        Some((
            EntryKind::File,
            FileMeta {
                size,
                mtime: None,
                identity: Some(FileIdentity {
                    dev: 1,
                    ino: u128::from(ino),
                }),
            },
        ))
    }

    fn index(entries: &[(&str, u64)]) -> PathIndex {
        let base = PathIndex::from_paths(Vec::<&str>::new()).expect("index");
        let changes: Vec<Change> = entries
            .iter()
            .map(|(p, ino)| {
                let (kind, meta) = file(*ino, 1).expect("file");
                Change::Upsert {
                    path: vp(p),
                    kind,
                    meta,
                }
            })
            .collect();
        base.apply(&changes).expect("apply")
    }

    #[test]
    fn unchanged_paths_produce_nothing() {
        let base = index(&[("a.md", 1), ("b.md", 2)]);
        let disk: Disk = [(vp("a.md"), file(1, 1))].into_iter().collect();
        let plan = plan(&base, &[], &disk).expect("plan");
        assert!(plan.changes.is_empty());
    }

    #[test]
    fn identity_turns_remove_and_add_into_a_move() {
        let base = index(&[("a.md", 1), ("b.md", 2)]);
        let id = base.id_of("a.md");
        let disk: Disk = [(vp("a.md"), None), (vp("dir/c.md"), file(1, 1))]
            .into_iter()
            .collect();
        let plan = plan(&base, &[], &disk).expect("plan");
        assert_eq!(plan.index.id_of("dir/c.md"), id);
        assert_eq!(plan.moves, [(vp("a.md"), vp("dir/c.md"))]);
        assert!(plan.new_files.is_empty());
    }

    #[test]
    fn rename_onto_a_file_keeps_the_target() {
        let base = index(&[("Note.md", 1), ("Note.md.tmp", 2)]);
        let id = base.id_of("Note.md");
        let disk: Disk = [(vp("Note.md.tmp"), None), (vp("Note.md"), file(2, 9))]
            .into_iter()
            .collect();
        let plan = plan(&base, &[(vp("Note.md.tmp"), vp("Note.md"))], &disk).expect("plan");
        assert_eq!(plan.index.id_of("Note.md"), id);
        assert!(plan.index.id_of("Note.md.tmp").is_none());
        assert_eq!(
            plan.index.entry_by_path("Note.md").map(|e| e.meta.size),
            Some(9)
        );
        assert!(
            !plan
                .changes
                .iter()
                .any(|c| matches!(c, Change::Remove { path } if path.as_str() == "Note.md"))
        );
    }
}
