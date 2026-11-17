// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Planning a rename or move: the moved entries, and the links that must
//! be rewritten to keep pointing at them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{FileEdit, LinkChange, Move, RefactorError, RefactorPlan, Skipped};
use crate::content::check::{encode_segment, path_part, rewrite_link};
use crate::content::{ContentSnapshot, extract};
use crate::files::ContentHash;
use crate::index::{Change, PathIndex};
use crate::keys::fold_key;
use crate::link::{LinkKind, LinkRef};
use crate::path::{VaultPath, extension_of};
use crate::profile::ProfileParams;
use crate::resolve::{Note, Status, resolve};

/// Plans `moves` in the vault at `root`, whose current path index is
/// `paths`. `content` finds the notes that link to the moved files; without
/// it only the moved notes' own links are updated (a warning says so).
/// Each affected note is read from disk.
pub fn plan_moves(
    root: &Path,
    paths: &PathIndex,
    content: Option<&ContentSnapshot>,
    params: &ProfileParams,
    moves: &[Move],
) -> Result<RefactorPlan, RefactorError> {
    let mut moved: BTreeMap<VaultPath, VaultPath> = BTreeMap::new();
    let mut changes = Vec::new();
    for m in moves {
        check_move(root, paths, m)?;
        for entry in paths.iter() {
            if let Some(rest) = entry.path.strip_prefix(&m.from) {
                let to = if rest.is_empty() {
                    m.to.clone()
                } else {
                    VaultPath::new(&format!("{}/{rest}", m.to.as_str()))
                        .map_err(|e| RefactorError::InvalidName(e.to_string()))?
                };
                moved.insert(entry.path.clone(), to);
            }
        }
        changes.push(Change::Rename {
            from: m.from.clone(),
            to: m.to.clone(),
        });
    }
    let post = paths
        .apply(&changes)
        .map_err(|e| RefactorError::InvalidName(e.to_string()))?;

    let mut warnings = Vec::new();
    // The notes to read: those that link to a moved file, and the moved
    // notes themselves.
    let mut sources: BTreeSet<VaultPath> = BTreeSet::new();
    let note_exts: Vec<String> = content.map_or_else(
        || vec!["md".into(), "markdown".into()],
        |c| c.options().note_extensions.clone(),
    );
    let is_note = |p: &VaultPath| {
        p.extension()
            .is_some_and(|e| note_exts.iter().any(|n| n.eq_ignore_ascii_case(e)))
    };
    for from in moved.keys() {
        let is_file = paths
            .entry_by_path(from.as_str())
            .is_some_and(|e| !e.kind.is_dir());
        if is_file && is_note(from) {
            sources.insert(from.clone());
        }
        if let Some(content) = content {
            sources.extend(content.backlink_sources(from));
        }
    }
    if content.is_none() {
        warnings
            .push("The link index is not ready: links in other notes were not updated.".to_owned());
    }

    let mut edits = Vec::new();
    let mut skipped = Vec::new();
    for source in &sources {
        plan_note(
            root,
            paths,
            &post,
            params,
            &moved,
            source,
            &mut edits,
            &mut skipped,
        );
    }

    // A moved file whose name another file already has: bare links may
    // become ambiguous.
    for (from, to) in &moved {
        let (Some(old), Some(new)) = (from.file_name(), to.file_name()) else {
            continue;
        };
        let key = fold_key(new);
        if fold_key(old) != key && post.by_name_fold(&key).len() > 1 {
            warnings.push(format!(
                "Another file is also named “{new}”: links by name alone may now be ambiguous."
            ));
        }
    }

    Ok(RefactorPlan {
        root: root.to_path_buf(),
        moves: moves.to_vec(),
        moved: moved.into_iter().collect(),
        edits,
        skipped,
        warnings,
    })
}

/// Refuses a move that cannot be done: a missing source, the root, a
/// taken destination (in any letter case), a folder into itself, a
/// missing destination folder, a name not valid on every platform.
fn check_move(root: &Path, paths: &PathIndex, m: &Move) -> Result<(), RefactorError> {
    if m.from.is_root() || m.to.is_root() {
        return Err(RefactorError::Root);
    }
    if m.from == m.to {
        return Err(RefactorError::Unchanged);
    }
    let from_fs = m.from.to_fs_path(root);
    if paths.entry_by_path(m.from.as_str()).is_none()
        || std::fs::symlink_metadata(&from_fs).is_err()
    {
        return Err(RefactorError::Missing(m.from.clone()));
    }
    if m.to.starts_with(&m.from) {
        return Err(RefactorError::IntoItself);
    }
    if let Some(name) = m.to.file_name() {
        super::validate_name(name)?;
    }
    let parent = m.to.parent().unwrap_or_default();
    if !parent.is_root()
        && !paths
            .entry_by_path(parent.as_str())
            .is_some_and(|e| e.kind.is_dir())
    {
        return Err(RefactorError::NoFolder(parent));
    }
    let case_only = m.is_case_only();
    let taken = paths
        .by_fold(&fold_key(m.to.as_str()))
        .iter()
        .filter_map(|id| paths.get(*id))
        .any(|e| e.path != m.from);
    let on_disk = !case_only && std::fs::symlink_metadata(m.to.to_fs_path(root)).is_ok();
    if taken || on_disk {
        return Err(RefactorError::Exists(m.to.clone()));
    }
    Ok(())
}

/// Plans the edits of one note.
#[allow(clippy::too_many_arguments)]
fn plan_note(
    root: &Path,
    pre: &PathIndex,
    post: &PathIndex,
    params: &ProfileParams,
    moved: &BTreeMap<VaultPath, VaultPath>,
    source: &VaultPath,
    edits: &mut Vec<FileEdit>,
    skipped: &mut Vec<Skipped>,
) {
    let skip_note = |reason: &str, skipped: &mut Vec<Skipped>| {
        skipped.push(Skipped {
            source: source.clone(),
            line: 0,
            link: String::new(),
            reason: reason.to_owned(),
        });
    };
    let bytes = match std::fs::read(source.to_fs_path(root)) {
        Ok(bytes) => bytes,
        Err(err) => {
            skip_note(&format!("cannot be read: {err}"), skipped);
            return;
        }
    };
    let Ok(text) = String::from_utf8(bytes) else {
        skip_note("is not UTF-8: its links were not updated", skipped);
        return;
    };
    let data = extract(&text, params.profile);
    let new_source = moved.get(source).unwrap_or(source);
    // The status (with whether the letter case differs) and the target.
    let target_of = |index: &PathIndex, kind: LinkKind, raw: &str, from: &VaultPath| {
        let lref = LinkRef::new(kind, raw, from.clone(), params.profile);
        let r = resolve(index, &lref, params);
        let target = r
            .target
            .and_then(|id| index.get(id))
            .map(|e| e.path.clone());
        ((r.status, r.notes.contains(&Note::CaseDiffers)), target)
    };
    let mut changes: Vec<LinkChange> = Vec::new();
    for link in &data.links {
        let (status, Some(target)) = target_of(pre, link.kind, &link.raw, source) else {
            continue;
        };
        if !matches!(status.0, Status::Exact | Status::CaseFold | Status::Loose) {
            continue;
        }
        let new_target = moved.get(&target).unwrap_or(&target);
        if new_source == source && *new_target == target {
            continue;
        }
        let (post_status, post_target) = target_of(post, link.kind, &link.raw, new_source);
        if post_status == status && post_target.as_ref() == Some(new_target) {
            continue;
        }
        let Some(construct) = text.get(link.span.range()) else {
            continue;
        };
        let skip = |reason: &str| Skipped {
            source: source.clone(),
            line: link.line,
            link: construct.to_owned(),
            reason: reason.to_owned(),
        };
        let written = path_part(link.kind, &link.raw);
        let Some(path) = generate(link.kind, written, new_source, new_target, post, params) else {
            skipped.push(skip("no link to the new place could be written"));
            continue;
        };
        match rewrite_link(construct, link.kind, &link.raw, &path) {
            Some(new) if new != construct => changes.push(LinkChange {
                span: link.span,
                line: link.line,
                old: construct.to_owned(),
                new,
            }),
            Some(_) => {}
            None => skipped.push(skip(if link.kind == LinkKind::Reference {
                "a reference link: update its definition by hand"
            } else {
                "the link could not be rewritten as written"
            })),
        }
    }
    if changes.is_empty() {
        return;
    }
    changes.sort_by_key(|c| c.span.start);
    changes.dedup_by_key(|c| c.span);
    let after = apply_changes(&text, &changes);
    edits.push(FileEdit {
        path: source.clone(),
        moved_to: new_source.clone(),
        before_hash: ContentHash::of(text.as_bytes()),
        before: text,
        after,
        changes,
    });
}

/// `text` with each change's span replaced (spans in order, not
/// overlapping).
fn apply_changes(text: &str, changes: &[LinkChange]) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let mut at = 0;
    for c in changes {
        let range = c.span.range();
        if range.start < at {
            continue;
        }
        out.push_str(&text[at..range.start]);
        out.push_str(&c.new);
        at = range.end;
    }
    out.push_str(&text[at..]);
    out
}

/// The path text of a link of `kind`, written like `written` (the old
/// path part), that resolves exactly to `target` from `source` in
/// `index`. `None` when no such text is found.
fn generate(
    kind: LinkKind,
    written: &str,
    source: &VaultPath,
    target: &VaultPath,
    index: &PathIndex,
    params: &ProfileParams,
) -> Option<String> {
    let resolves = |text: &str| {
        let lref = LinkRef::new(kind, text, source.clone(), params.profile);
        let r = resolve(index, &lref, params);
        r.status == Status::Exact
            && !r.notes.contains(&Note::CaseDiffers)
            && r.target.and_then(|id| index.get(id)).map(|e| &e.path) == Some(target)
    };
    let segments: Vec<&str> = target.segments().collect();
    let last_written = written.rsplit('/').next().unwrap_or(written);
    let target_ext = target.extension();
    // The extension stays off when it was left off.
    let drop_ext = target_ext.is_some_and(|ext| {
        let is_note = ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown");
        let written_ext = extension_of(last_written);
        is_note && !written_ext.is_some_and(|w| w.eq_ignore_ascii_case(ext))
    });
    let strip = |mut text: String| {
        if drop_ext && let Some(ext) = target_ext {
            text.truncate(text.len() - ext.len() - 1);
        }
        text
    };
    if kind.is_wiki() {
        let absolute = written.starts_with('/');
        let written_depth = written
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .count()
            .max(1);
        let start = if absolute {
            segments.len()
        } else {
            written_depth.min(segments.len())
        };
        for n in start..=segments.len() {
            let mut text = strip(segments[segments.len() - n..].join("/"));
            if absolute {
                text.insert(0, '/');
            }
            if text.contains(['|', '#', '[', ']', '^']) {
                return None;
            }
            if resolves(&text) {
                return Some(text);
            }
        }
        return None;
    }
    // Markdown links: percent-encoded unless the old one had literal spaces.
    let encode = !written.contains(' ');
    let join = |parts: &[&str]| {
        parts
            .iter()
            .map(|s| {
                if encode {
                    encode_segment(s)
                } else {
                    (*s).to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    };
    let mut candidates = Vec::new();
    let rooted = format!("/{}", join(&segments));
    let relative = relative_parts(source, target);
    let relative = join(&relative.iter().map(String::as_str).collect::<Vec<_>>());
    let dotted = (!relative.starts_with("..")).then(|| format!("./{relative}"));
    if written.starts_with('/') {
        candidates.push(rooted.clone());
    } else if written.starts_with("./") {
        candidates.extend(dotted.clone());
        candidates.push(relative.clone());
    } else {
        candidates.push(relative.clone());
        candidates.extend(dotted.clone());
    }
    candidates.push(rooted);
    for text in candidates {
        let stripped = strip(text.clone());
        if stripped != text && resolves(&stripped) {
            return Some(stripped);
        }
        if resolves(&text) {
            return Some(text);
        }
    }
    None
}

/// The segments of `to` relative to the folder of `from` (`..` first).
fn relative_parts(from: &VaultPath, to: &VaultPath) -> Vec<String> {
    let mut from_dir: Vec<&str> = from.segments().collect();
    from_dir.pop();
    let to_segments: Vec<&str> = to.segments().collect();
    let common = from_dir
        .iter()
        .zip(&to_segments)
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = vec!["..".into(); from_dir.len() - common];
    parts.extend(to_segments[common..].iter().map(|s| (*s).to_owned()));
    parts
}
