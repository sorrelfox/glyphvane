// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Planning a rename or move: the moved entries, and the links that must
//! be rewritten to keep pointing at them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{FileEdit, LinkChange, Move, RefactorError, RefactorPlan, Skipped};
use crate::content::check::{encode_href_segment, encode_segment, path_part, rewrite_link};
use crate::content::{ContentSnapshot, Span, extract};
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
    if let Some(content) = content {
        sources.extend(html_href_sources(content, paths, params, |p| {
            moved.contains_key(p)
        }));
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

    // Canvas files name vault paths in their file nodes and group
    // backgrounds: those follow the moves too.
    for entry in paths.iter() {
        if entry.kind.is_dir()
            || !entry
                .path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("canvas"))
        {
            continue;
        }
        plan_canvas(root, &moved, &entry.path, &mut edits, &mut skipped);
    }

    // Bases name files in `link("…")` and `file("…")` literals, folders in
    // `.inFolder("…")` ones: those follow the moves too, by byte ranges.
    for entry in paths.iter() {
        if entry.kind.is_dir()
            || !entry
                .path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("base"))
        {
            continue;
        }
        plan_base(
            root,
            paths,
            &post,
            params,
            &moved,
            &entry.path,
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
    // Reference links take their destination from a definition
    // (`[label]: dest`): the definition is rewritten like an inline link,
    // and the uses that read it need nothing of their own.
    let mut defined: BTreeSet<&str> = BTreeSet::new();
    let defs = ref_defs(&text, params.profile);
    for def in &defs {
        let raw = def.def.destination.as_str();
        let (status, Some(target)) = target_of(pre, LinkKind::Reference, raw, source) else {
            continue;
        };
        if !matches!(status.0, Status::Exact | Status::CaseFold | Status::Loose) {
            continue;
        }
        defined.insert(raw);
        let new_target = moved.get(&target).unwrap_or(&target);
        if new_source == source && *new_target == target {
            continue;
        }
        let (post_status, post_target) = target_of(post, LinkKind::Reference, raw, new_source);
        if post_status == status && post_target.as_ref() == Some(new_target) {
            continue;
        }
        let (Some(construct), Some(dest)) = (
            text.get(def.range.clone()),
            text.get(def.destination_range.clone()),
        ) else {
            continue;
        };
        let skip = |reason: &str| Skipped {
            source: source.clone(),
            line: line_of(&text, def.range.start),
            link: construct.to_owned(),
            reason: reason.to_owned(),
        };
        let written = path_part(LinkKind::Inline, raw);
        let Some(path) = generate(
            LinkKind::Inline,
            written,
            new_source,
            new_target,
            post,
            params,
        ) else {
            skipped.push(skip("no link to the new place could be written"));
            continue;
        };
        // The destination as written: only its path part is replaced.
        match rewrite_link(dest, LinkKind::Inline, raw, &path) {
            Some(new_dest) if new_dest != dest => {
                let at = def.destination_range.start - def.range.start;
                let end = def.destination_range.end - def.range.start;
                changes.push(LinkChange {
                    span: Span::of(def.range.clone()),
                    line: line_of(&text, def.range.start),
                    old: construct.to_owned(),
                    new: format!("{}{new_dest}{}", &construct[..at], &construct[end..]),
                });
            }
            Some(_) => {}
            None => skipped.push(skip("the definition could not be rewritten as written")),
        }
    }
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
        // A reference link or image whose definition was planned above.
        let by_reference = link.kind == LinkKind::Reference
            || (link.kind == LinkKind::Image && !construct.ends_with(')'));
        if by_reference && defined.contains(&*link.raw) {
            continue;
        }
        let skip = |reason: &str| Skipped {
            source: source.clone(),
            line: link.line,
            link: construct.to_owned(),
            reason: reason.to_owned(),
        };
        let written = path_part(link.kind, &link.raw);
        let Some(mut path) = generate(link.kind, written, new_source, new_target, post, params)
        else {
            skipped.push(skip("no link to the new place could be written"));
            continue;
        };
        // In front matter the link sits in a YAML scalar: escaped for its
        // quotes.
        if link.front_matter {
            path = yaml_escaped(&path, yaml_quote_at(&text, link.span.start as usize));
        }
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
    // `<a href>`s: not links to Obsidian (see `NoteData::html_links`),
    // but rewritten all the same so they keep working. The attribute's
    // value is the construct; its path part is replaced, percent-encoded.
    for link in &data.html_links {
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
        let Some(value) = text.get(link.span.range()) else {
            continue;
        };
        let skip = |reason: &str| Skipped {
            source: source.clone(),
            line: link.line,
            link: value.to_owned(),
            reason: reason.to_owned(),
        };
        if value != &*link.raw {
            skipped.push(skip("the link has character references: update it by hand"));
            continue;
        }
        let written = path_part(link.kind, &link.raw);
        if written.is_empty() {
            continue;
        }
        let Some(path) = generate(link.kind, written, new_source, new_target, post, params) else {
            skipped.push(skip("no link to the new place could be written"));
            continue;
        };
        let new = format!("{path}{}", &value[written.len()..]);
        if new != value {
            changes.push(LinkChange {
                span: link.span,
                line: link.line,
                old: value.to_owned(),
                new,
            });
        }
    }
    if changes.is_empty() {
        return;
    }
    changes.sort_by_key(|c| c.span.start);
    // One change per span; an `<a href>` inside a rewritten link (in its
    // text) goes with that link's change.
    changes.dedup_by(|c, kept| c.span.start < kept.span.end || c.span == kept.span);
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

/// The notes with an `<a href>` that resolves (in `index`) to an entry
/// `is_target` accepts. Those are not backlinks (see
/// [`crate::content::NoteData::html_links`]), so the records are searched;
/// only notes with an `<a href>` are resolved.
pub(super) fn html_href_sources(
    content: &ContentSnapshot,
    index: &PathIndex,
    params: &ProfileParams,
    is_target: impl Fn(&VaultPath) -> bool,
) -> BTreeSet<VaultPath> {
    content
        .notes()
        .into_iter()
        .filter(|record| {
            record.data.html_links.iter().any(|l| {
                let lref = LinkRef::new(l.kind, &*l.raw, record.path.clone(), params.profile);
                let r = resolve(index, &lref, params);
                r.status.is_resolved()
                    && r.target
                        .and_then(|id| index.get(id))
                        .is_some_and(|e| is_target(&e.path))
            })
        })
        .map(|record| record.path.clone())
        .collect()
}

/// The quote of the YAML scalar that holds byte `at` of `text` (in front
/// matter), `None` for a plain scalar. Read from the start of `at`'s
/// line: a quote opens a scalar only where one can start (the line start,
/// or after `:`, `-`, `[`, `{` or `,` and spaces), so a quoted scalar
/// running over several lines reads as plain.
pub(crate) fn yaml_quote_at(text: &str, at: usize) -> Option<char> {
    let at = at.min(text.len());
    let b = text.as_bytes();
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let mut quote: Option<u8> = None;
    // The last byte other than a space outside quotes.
    let mut prev: Option<u8> = None;
    let mut i = start;
    while i < at {
        let c = b[i];
        match quote {
            Some(b'"') if c == b'\\' => i += 1,
            Some(b'\'') if c == b'\'' && b.get(i + 1) == Some(&b'\'') => i += 1,
            Some(q) if c == q => {
                quote = None;
                prev = Some(c);
            }
            Some(_) => {}
            None if (c == b'"' || c == b'\'')
                && prev.is_none_or(|p| matches!(p, b':' | b'-' | b'[' | b'{' | b',')) =>
            {
                quote = Some(c);
            }
            None if c == b' ' || c == b'\t' => {}
            None => prev = Some(c),
        }
        i += 1;
    }
    quote.map(char::from)
}

/// `path` as it is written inside a YAML scalar quoted with `quote`:
/// `''` for `'` in single quotes; `\\` and `\"` in double quotes.
fn yaml_escaped(path: &str, quote: Option<char>) -> String {
    match quote {
        Some('\'') => path.replace('\'', "''"),
        Some('"') => path.replace('\\', "\\\\").replace('"', "\\\""),
        _ => path.to_owned(),
    }
}

/// The link reference definitions of `text` with their document ranges.
fn ref_defs(text: &str, profile: crate::profile::ProfileId) -> Vec<gv_syntax::inline::RefDefSpans> {
    struct Defs<'t> {
        text: &'t str,
        out: Vec<gv_syntax::inline::RefDefSpans>,
    }
    impl gv_syntax::Visitor for Defs<'_> {
        fn leaf(&mut self, leaf: &gv_syntax::Leaf<'_>) {
            if let Some(def) = leaf.link_ref_def(self.text) {
                self.out.push(def);
            }
        }
    }
    if !text.contains("]:") {
        return Vec::new();
    }
    let tree = gv_syntax::parse_with(text, gv_syntax::ParseOptions::builtin(profile));
    let mut defs = Defs {
        text,
        out: Vec::new(),
    };
    gv_syntax::walk(&tree, &mut defs);
    defs.out
}

/// Plans the edits of the canvas file `source`: every `file` (and group
/// `background`) that names a moved entry is replaced by its new path,
/// touching only the bytes of that string.
fn plan_canvas(
    root: &Path,
    moved: &BTreeMap<VaultPath, VaultPath>,
    source: &VaultPath,
    edits: &mut Vec<FileEdit>,
    skipped: &mut Vec<Skipped>,
) {
    let skip_canvas = |reason: String, skipped: &mut Vec<Skipped>| {
        skipped.push(Skipped {
            source: source.clone(),
            line: 0,
            link: String::new(),
            reason,
        });
    };
    let bytes = match std::fs::read(source.to_fs_path(root)) {
        Ok(bytes) => bytes,
        Err(err) => {
            skip_canvas(format!("cannot be read: {err}"), skipped);
            return;
        }
    };
    let Ok(text) = String::from_utf8(bytes) else {
        skip_canvas(
            "is not UTF-8: its file cards were not updated".to_owned(),
            skipped,
        );
        return;
    };
    let mut changes = Vec::new();
    let planned = gv_canvas::patch::plan_path_patches(&text, |r| {
        // Paths are vault-relative; a leading slash is kept as written.
        let rooted = r.value.starts_with('/');
        let path = VaultPath::new(r.value.trim_start_matches('/')).ok()?;
        let to = moved.get(&path)?;
        let new = if rooted {
            format!("/{}", to.as_str())
        } else {
            to.as_str().to_owned()
        };
        changes.push(LinkChange {
            span: Span::of(r.range.clone()),
            line: line_of(&text, r.range.start),
            old: text.get(r.range.clone()).unwrap_or_default().to_owned(),
            new: gv_canvas::patch::encode_json_string(&new),
        });
        Some(new)
    });
    let patches = match planned {
        Ok(patches) => patches,
        Err(err) => {
            if text.trim().is_empty() {
                return;
            }
            skip_canvas(
                format!("is not a valid canvas ({err}): its file cards were not updated"),
                skipped,
            );
            return;
        }
    };
    if patches.is_empty() {
        return;
    }
    // Only the refs that got a patch (an unchanged value gives none).
    changes.retain(|c| patches.iter().any(|p| Span::of(p.range.clone()) == c.span));
    let after = match gv_canvas::patch::apply_patches(&text, &patches) {
        Ok(after) => after,
        Err(err) => {
            skip_canvas(
                format!("its file cards could not be updated: {err}"),
                skipped,
            );
            return;
        }
    };
    edits.push(FileEdit {
        path: source.clone(),
        moved_to: moved.get(source).unwrap_or(source).clone(),
        before_hash: ContentHash::of(text.as_bytes()),
        before: text,
        after,
        changes,
    });
}

/// A file or folder named by a Bases function in a `.base` file: the
/// literal of `link("…")` or `file("…")`, or of `.inFolder("…")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BaseRef {
    /// The bytes of the literal's text, between its quotes.
    pub range: std::ops::Range<usize>,
    /// The quote it is written in.
    pub quote: char,
    /// A folder (`inFolder`), not a file.
    pub folder: bool,
    /// Why it cannot be rewritten in place (escapes, YAML-doubled quotes).
    pub unsafe_reason: Option<&'static str>,
}

/// The `link("…")` and `file("…")` literals, and the `.inFolder("…")`
/// ones, of a `.base` file's text.
///
/// The file is YAML whose scalars hold expressions, so a literal may sit
/// inside a quoted YAML scalar. Only literals whose bytes are exactly the
/// path (no backslash escapes, no YAML-doubled quotes) can be patched by
/// byte range; the others are listed with a reason.
pub(crate) fn base_refs(text: &str) -> Vec<BaseRef> {
    let bytes = text.as_bytes();
    let mut refs = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let Some(rel) = text[at..].find(['l', 'f', 'i']) else {
            break;
        };
        let start = at + rel;
        at = start + 1;
        let prev = start.checked_sub(1).map(|i| bytes[i]);
        let folder = text[start..].starts_with("inFolder(");
        if folder {
            // A method: `file.inFolder(…)`.
            if prev != Some(b'.') {
                continue;
            }
        } else if text[start..].starts_with("link(") || text[start..].starts_with("file(") {
            // A function, not the end of a longer name or a method.
            if prev.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.') {
                continue;
            }
        } else {
            continue;
        }
        let mut i = start
            + if folder {
                "inFolder(".len()
            } else {
                "link(".len()
            };
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        // `\"` (in a double-quoted YAML scalar) or `''` (in a
        // single-quoted one) open a literal that YAML escapes.
        let mut escaped = false;
        if bytes.get(i) == Some(&b'\\') {
            escaped = true;
            i += 1;
        }
        let Some(&q) = bytes.get(i).filter(|&&b| b == b'"' || b == b'\'') else {
            continue;
        };
        if bytes.get(i + 1) == Some(&q) {
            escaped = true;
            i += 1;
        }
        let open = i;
        let Some(len) = text[open + 1..].find([q as char, '\n']) else {
            break;
        };
        let mut close = open + 1 + len;
        if bytes[close] != q {
            at = close;
            continue;
        }
        if bytes[close - 1] == b'\\' {
            escaped = true;
            close -= 1;
        }
        let value = &text[open + 1..close];
        if bytes.get(close + 1) == Some(&q) {
            escaped = true;
        }
        let unsafe_reason = if escaped || value.contains('\\') {
            Some("its quotes are escaped in the YAML: update it by hand")
        } else if value.is_empty() {
            continue;
        } else {
            None
        };
        refs.push(BaseRef {
            range: open + 1..close,
            quote: q as char,
            folder,
            unsafe_reason,
        });
        at = close + 1;
    }
    refs
}

/// Whether `value` can stand in a base literal written in `quote`
/// without changing what the YAML around it means.
pub(super) fn fits_base_literal(value: &str, quote: char) -> bool {
    !value.is_empty()
        && !value.contains(['"', '\'', '\\', '\n', '\r'])
        && !value.contains(" #")
        && !value.contains(": ")
        && !value.contains(quote)
}

/// Plans the edits of the base file `source`: every `link("…")` or
/// `file("…")` literal that resolves (as a wikilink of the base) to a
/// moved file gets the moved file's new path, and every `.inFolder("…")`
/// literal naming a moved folder its new path, touching only the bytes of
/// the literal.
#[allow(clippy::too_many_arguments)]
fn plan_base(
    root: &Path,
    pre: &PathIndex,
    post: &PathIndex,
    params: &ProfileParams,
    moved: &BTreeMap<VaultPath, VaultPath>,
    source: &VaultPath,
    edits: &mut Vec<FileEdit>,
    skipped: &mut Vec<Skipped>,
) {
    let Ok(bytes) = std::fs::read(source.to_fs_path(root)) else {
        // An unreadable base is reported only when it names a moved file,
        // which cannot be known: it is left alone like an unindexed note.
        return;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return;
    };
    if !text.contains("link(") && !text.contains("file(") && !text.contains("inFolder(") {
        return;
    }
    let new_source = moved.get(source).unwrap_or(source);
    let target_of = |index: &PathIndex, raw: &str, from: &VaultPath| {
        let lref = LinkRef::new(LinkKind::Wikilink, raw, from.clone(), params.profile);
        let r = resolve(index, &lref, params);
        let ok = matches!(r.status, Status::Exact | Status::CaseFold | Status::Loose);
        r.target
            .filter(|_| ok)
            .and_then(|id| index.get(id))
            .map(|e| e.path.clone())
    };
    let mut changes = Vec::new();
    for r in base_refs(&text) {
        let value = &text[r.range.clone()];
        if r.folder {
            // A path from the vault root, maybe with a slash at its end.
            let trimmed = value.trim_end_matches('/');
            let Some(new_folder) = VaultPath::new(trimmed)
                .ok()
                .filter(|f| {
                    pre.entry_by_path(f.as_str())
                        .is_some_and(|e| e.kind.is_dir())
                })
                .and_then(|f| moved.get(&f))
            else {
                continue;
            };
            let new = format!("{}{}", new_folder.as_str(), &value[trimmed.len()..]);
            let line = line_of(&text, r.range.start);
            if let Some(reason) = r.unsafe_reason {
                skipped.push(Skipped {
                    source: source.clone(),
                    line,
                    link: value.to_owned(),
                    reason: reason.to_owned(),
                });
            } else if !fits_base_literal(&new, r.quote) {
                skipped.push(Skipped {
                    source: source.clone(),
                    line,
                    link: value.to_owned(),
                    reason: "the new path needs quoting in the base: update it by hand".to_owned(),
                });
            } else if new != value {
                changes.push(LinkChange {
                    span: Span::of(r.range.clone()),
                    line,
                    old: value.to_owned(),
                    new,
                });
            }
            continue;
        }
        let (path_text, fragment) = match value.find('#') {
            Some(i) => (&value[..i], &value[i..]),
            None => (value, ""),
        };
        let Some(target) = target_of(pre, path_text, source) else {
            continue;
        };
        let Some(new_target) = moved.get(&target) else {
            continue;
        };
        if target_of(post, path_text, new_source).as_ref() == Some(new_target) {
            continue;
        }
        let skip = |reason: &str, skipped: &mut Vec<Skipped>| {
            skipped.push(Skipped {
                source: source.clone(),
                line: line_of(&text, r.range.start),
                link: value.to_owned(),
                reason: reason.to_owned(),
            });
        };
        if let Some(reason) = r.unsafe_reason {
            skip(reason, skipped);
            continue;
        }
        let Some(path) = generate(
            LinkKind::Wikilink,
            path_text,
            new_source,
            new_target,
            post,
            params,
        ) else {
            skip("no link to the new place could be written", skipped);
            continue;
        };
        let new = format!("{path}{fragment}");
        if !fits_base_literal(&new, r.quote) {
            skip(
                "the new path needs quoting in the base: update it by hand",
                skipped,
            );
            continue;
        }
        if new != value {
            changes.push(LinkChange {
                span: Span::of(r.range.clone()),
                line: line_of(&text, r.range.start),
                old: value.to_owned(),
                new,
            });
        }
    }
    if changes.is_empty() {
        return;
    }
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

/// The 0-based line of byte `offset` in `text`.
fn line_of(text: &str, offset: usize) -> u32 {
    let end = offset.min(text.len());
    let lines = text.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    u32::try_from(lines).unwrap_or(u32::MAX)
}

/// `text` with each change's span replaced (spans in order, not
/// overlapping).
pub(super) fn apply_changes(text: &str, changes: &[LinkChange]) -> String {
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
    // Markdown links: percent-encoded unless the old one had literal
    // spaces. An HTML `href` is a URL: always encoded, quotes and `&` too.
    let href = kind == LinkKind::HtmlHref;
    let encode = href || !written.contains(' ');
    let join = |parts: &[&str]| {
        parts
            .iter()
            .map(|s| {
                if href {
                    encode_href_segment(s)
                } else if encode {
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
