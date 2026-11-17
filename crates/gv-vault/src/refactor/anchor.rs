// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Renaming a heading or a block id: every link to it follows.
//!
//! [`plan_anchor_rename`] takes the note's current text (its editor
//! buffer, saved or not) and finds the links that name the heading or the
//! block: the note's own links and those of the notes that link to it
//! (the content index's backlinks), embeds included, and the `subpath` of
//! canvas file cards. A link is rewritten only when its fragment would no
//! longer land on the same heading, and then in its own style:
//!
//! - a part that matched by **heading text** (Obsidian) gets the new
//!   heading text (characters a subpath cannot hold become spaces;
//!   percent-encoded in a Markdown link);
//! - a part that matched by a **slug** gets the new slug of the same
//!   algorithm, `user-content-` kept. Slugs are computed over the whole
//!   note again, so the `-1`, `-2` suffixes of later headings with the
//!   same text are recomputed and the links to those follow too;
//! - chains (`[[Note#A#B]]`) change only the parts that need it; display
//!   text, the path and everything else stay as written.
//!
//! A block-id rename replaces the `^id` marker and the id in every
//! `#^id` fragment.
//!
//! The result, an [`AnchorPlan`], has the note's own edit (applied to its
//! buffer as one undo step) and a [`RefactorPlan`] without moves for the
//! other files, which [`super::apply`] journals and writes and
//! [`super::undo`] reverts, like a file rename.

use std::ops::Range;
use std::path::Path;

use gv_model::slug::obsidian::heading_key;

use super::{FileEdit, LinkChange, RefactorError, RefactorPlan, Skipped};
use crate::content::check::{encode_segment, heading_chain, heading_slugs};
use crate::content::{ContentSnapshot, IndexedHeading, NoteData, RecordKind, Span, extract};
use crate::files::ContentHash;
use crate::fragment::{Fragment, FragmentKind};
use crate::index::PathIndex;
use crate::keys::nfc_key;
use crate::link::{LinkKind, LinkRef};
use crate::path::VaultPath;
use crate::profile::{Anchoring, ProfileParams};
use crate::resolve::resolve;

/// What is renamed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorTarget {
    /// The heading at this index of the note's headings (document order).
    Heading(usize),
    /// The block id, without `^`.
    Block(String),
}

/// A heading or block-id rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorRename {
    /// The note that holds the heading or block.
    pub note: VaultPath,
    pub target: AnchorTarget,
    /// The new heading text (its Markdown source) or the new id (without
    /// `^`).
    pub new_name: String,
}

/// What a heading or block-id rename will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorPlan {
    /// The note that holds the heading or block.
    pub note: VaultPath,
    /// Whether a block id is renamed (else a heading).
    pub is_block: bool,
    /// The name before (heading source text, or the id).
    pub old_name: String,
    /// The name after.
    pub new_name: String,
    /// The note's own edit: the heading or `^id` itself first, then the
    /// note's links to it. `before` is the text the plan was made from.
    pub own: FileEdit,
    /// The other notes and canvases (no moves), for [`super::apply`].
    pub others: RefactorPlan,
}

impl AnchorPlan {
    /// The number of links rewritten, in the note and elsewhere.
    #[must_use]
    pub fn link_count(&self) -> usize {
        self.own.changes.len().saturating_sub(1) + self.others.link_count()
    }

    /// The number of other files edited.
    #[must_use]
    pub fn other_files(&self) -> usize {
        self.others.edits.len()
    }
}

/// Refuses an id a block cannot have: Obsidian's block ids are letters,
/// digits and dashes.
pub fn validate_block_id(id: &str) -> Result<(), RefactorError> {
    if id.is_empty() {
        return Err(RefactorError::InvalidName("An id is needed".into()));
    }
    if let Some(c) = id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-'))
    {
        return Err(RefactorError::InvalidName(format!(
            "“{c}” is not allowed in a block id (letters, digits and - only)"
        )));
    }
    Ok(())
}

/// Refuses heading text that is empty or spans lines.
pub fn validate_heading_text(text: &str) -> Result<(), RefactorError> {
    if text.trim().is_empty() {
        return Err(RefactorError::InvalidName("A heading needs text".into()));
    }
    if text.contains(['\n', '\r']) {
        return Err(RefactorError::InvalidName(
            "A heading is one line of text".into(),
        ));
    }
    Ok(())
}

/// The byte range of the text of heading `h` in `text`: what a rename
/// replaces (the `#` marks, a closing sequence and a setext underline
/// stay). `None` for a heading whose text spans lines or is empty.
#[must_use]
pub fn heading_text_range(text: &str, h: &IndexedHeading) -> Option<Range<usize>> {
    if h.source.is_empty() || h.source.contains('\n') {
        return None;
    }
    let line = line_range(text, h.line as usize)?;
    let line_text = &text[line.clone()];
    // After the opening `#` run of an ATX heading (inside any container
    // markers), else from the line's start (setext).
    let from = match line_text.find('#') {
        Some(at) if h.span.contains(line.start + at) || h.span.start as usize <= line.start => {
            let rest = &line_text[at..];
            at + rest.len() - rest.trim_start_matches('#').len()
        }
        _ => 0,
    };
    let from = if line_text[from..].contains(&*h.source) {
        from
    } else {
        0
    };
    let at = from + line_text[from..].find(&*h.source)?;
    Some(line.start + at..line.start + at + h.source.len())
}

/// The byte range of the `^id` marker of block id `id` (the `^`
/// included), searching from the block's start to the end of the line
/// after it.
#[must_use]
pub fn block_marker_range(text: &str, span: Span, id: &str) -> Option<Range<usize>> {
    let start = (span.start as usize).min(text.len());
    let end = span.end as usize;
    // The marker ends the block's last line, or is the next line alone.
    let limit = text[end.min(text.len())..]
        .find('\n')
        .map_or(text.len(), |n| {
            let next = end + n + 1;
            text[next.min(text.len())..]
                .find('\n')
                .map_or(text.len(), |m| next + m)
        });
    let needle = format!("^{id}");
    let mut at = start;
    while let Some(rel) = text[at..limit.max(at)].find(&needle) {
        let pos = at + rel;
        let after = pos + needle.len();
        let before_ok = pos == 0 || text[..pos].ends_with([' ', '\t', '\n']);
        let after_ok = text[after..]
            .chars()
            .next()
            .is_none_or(|c| c == '\n' || c == '\r' || c == ' ' || c == '\t');
        if before_ok && after_ok {
            return Some(pos..after);
        }
        at = pos + 1;
    }
    None
}

/// Plans renaming the heading or block id `rename` names, in the note
/// whose current text is `note_text`, in the vault at `root` whose path
/// index is `paths`. `content` finds the notes that link to it; without
/// it only the note's own links change (a warning says so). The other
/// notes are read from disk; one with unsaved changes in an editor is
/// refused ([`RefactorError::Unsaved`]).
pub fn plan_anchor_rename(
    root: &Path,
    paths: &PathIndex,
    content: Option<&ContentSnapshot>,
    params: &ProfileParams,
    note_text: &str,
    rename: &AnchorRename,
) -> Result<AnchorPlan, RefactorError> {
    let note = &rename.note;
    let before = extract(note_text, params.profile);
    let (anchor, old_name, new_name) = anchor_change(note_text, &before, rename)?;
    let mut mid_text = String::with_capacity(note_text.len() + anchor.new.len());
    mid_text.push_str(&note_text[..anchor.span.start as usize]);
    mid_text.push_str(&anchor.new);
    mid_text.push_str(&note_text[anchor.span.end as usize..]);
    let after = extract(&mid_text, params.profile);

    let mut warnings = Vec::new();
    let ctx = match &rename.target {
        AnchorTarget::Heading(i) => {
            let same_shape = after.headings.len() == before.headings.len()
                && after.headings[*i].level == before.headings[*i].level;
            if !same_shape {
                return Err(RefactorError::InvalidName(
                    "That text would not stay a heading".into(),
                ));
            }
            let key = heading_key(&after.headings[*i].source);
            if after
                .headings
                .iter()
                .enumerate()
                .any(|(j, h)| j != *i && heading_key(&h.source) == key)
            {
                warnings.push(
                    "Another heading has the same text: links by heading text go to the first one."
                        .to_owned(),
                );
            }
            Anchor::Heading {
                before: &before.headings,
                after: &after.headings,
                primary: params.anchoring,
            }
        }
        AnchorTarget::Block(old) => {
            if !after.block_ids.iter().any(|b| *b.id == *new_name) {
                return Err(RefactorError::InvalidName(
                    "That id would not stay a block id".into(),
                ));
            }
            Anchor::Block {
                old: old.clone(),
                new: new_name.clone(),
            }
        }
    };
    let ctx = Ctx { note, anchor: ctx };

    let mut skipped = Vec::new();
    // The note itself: its own links, against the text given.
    let mut own_changes = link_changes(paths, params, note, note_text, &before, &ctx, &mut skipped);
    own_changes.retain(|c| c.span.end <= anchor.span.start || c.span.start >= anchor.span.end);
    own_changes.push(anchor);
    own_changes.sort_by_key(|c| c.span.start);
    let own_after = super::plan::apply_changes(note_text, &own_changes);
    let own = FileEdit {
        path: note.clone(),
        moved_to: note.clone(),
        before: note_text.to_owned(),
        after: own_after,
        before_hash: ContentHash::of(note_text.as_bytes()),
        changes: own_changes,
    };

    let mut edits = Vec::new();
    match content {
        Some(content) => {
            for source in content.backlink_sources(note) {
                if source == *note {
                    continue;
                }
                if content
                    .note(&source)
                    .is_some_and(|r| r.kind == RecordKind::Overlay)
                {
                    return Err(RefactorError::Unsaved(source));
                }
                plan_other(root, paths, params, &source, &ctx, &mut edits, &mut skipped);
            }
        }
        None => warnings
            .push("The link index is not ready: links in other notes were not updated.".to_owned()),
    }
    for entry in paths.iter() {
        if entry.kind.is_dir()
            || !entry
                .path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("canvas"))
        {
            continue;
        }
        plan_canvas(root, note, &entry.path, &ctx, &mut edits, &mut skipped);
    }

    Ok(AnchorPlan {
        note: note.clone(),
        is_block: matches!(rename.target, AnchorTarget::Block(_)),
        old_name,
        new_name,
        own,
        others: RefactorPlan {
            root: root.to_path_buf(),
            moves: Vec::new(),
            moved: Vec::new(),
            edits,
            skipped,
            warnings,
        },
    })
}

/// What a fragment is checked against.
struct Ctx<'a> {
    /// The note the heading or block is in.
    note: &'a VaultPath,
    anchor: Anchor<'a>,
}

/// The renamed heading's note before and after, or the block ids.
enum Anchor<'a> {
    Heading {
        before: &'a [IndexedHeading],
        after: &'a [IndexedHeading],
        primary: Anchoring,
    },
    Block {
        old: String,
        new: String,
    },
}

/// The edit of the heading's text or the block's marker, with the old and
/// new names.
fn anchor_change(
    text: &str,
    data: &NoteData,
    rename: &AnchorRename,
) -> Result<(LinkChange, String, String), RefactorError> {
    match &rename.target {
        AnchorTarget::Heading(i) => {
            let h = data
                .headings
                .get(*i)
                .ok_or_else(|| RefactorError::NoAnchor("The heading is no longer there".into()))?;
            let new = nfc_key(rename.new_name.trim()).into_owned();
            validate_heading_text(&new)?;
            if new == *h.source {
                return Err(RefactorError::Unchanged);
            }
            let range = heading_text_range(text, h).ok_or_else(|| {
                RefactorError::NoAnchor("A heading on several lines cannot be renamed here".into())
            })?;
            Ok((
                LinkChange {
                    span: Span::of(range.clone()),
                    line: h.line,
                    old: text[range].to_owned(),
                    new: new.clone(),
                },
                h.source.to_string(),
                new,
            ))
        }
        AnchorTarget::Block(old) => {
            let new = rename.new_name.trim().trim_start_matches('^').to_owned();
            validate_block_id(&new)?;
            if new == *old {
                return Err(RefactorError::Unchanged);
            }
            let block = data
                .block_ids
                .iter()
                .find(|b| *b.id == **old)
                .ok_or_else(|| RefactorError::NoAnchor(format!("No block has the id ^{old}")))?;
            if data
                .block_ids
                .iter()
                .any(|b| *b.id != **old && b.id.eq_ignore_ascii_case(&new))
            {
                return Err(RefactorError::InvalidName(format!(
                    "Another block already has the id ^{new}"
                )));
            }
            let range = block_marker_range(text, block.span, old).ok_or_else(|| {
                RefactorError::NoAnchor(format!("The marker ^{old} was not found"))
            })?;
            let line = line_of(text, range.start);
            Ok((
                LinkChange {
                    span: Span::of(range),
                    line,
                    old: format!("^{old}"),
                    new: format!("^{new}"),
                },
                old.clone(),
                new,
            ))
        }
    }
}

/// The changes to the links of `data` (the text of note `source`) that
/// name the renamed heading or block.
fn link_changes(
    paths: &PathIndex,
    params: &ProfileParams,
    source: &VaultPath,
    text: &str,
    data: &NoteData,
    ctx: &Ctx<'_>,
    skipped: &mut Vec<Skipped>,
) -> Vec<LinkChange> {
    let mut changes = Vec::new();
    for link in &data.links {
        let lref = LinkRef::new(link.kind, &*link.raw, source.clone(), params.profile);
        let r = resolve(paths, &lref, params);
        if !r.status.is_resolved() {
            continue;
        }
        let target = r
            .target
            .and_then(|id| paths.get(id))
            .map(|e| e.path.clone())
            .or(r.target_path);
        let Some(fragment) = r.fragment else {
            continue;
        };
        if target.as_ref() != Some(ctx.note) {
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
        let style = if link.kind.is_wiki() {
            Style::Wiki
        } else {
            Style::Markdown
        };
        let new_fragment = match new_fragment(&fragment, style, ctx) {
            None => continue,
            Some(Err(reason)) => {
                skipped.push(skip(reason));
                continue;
            }
            Some(Ok(f)) => f,
        };
        // The fragment as written inside the raw destination.
        let raw = &*link.raw;
        let Some(hash) = raw.find('#') else {
            continue;
        };
        let start = hash + 1;
        let end = start + fragment.raw.len();
        if raw.get(start..end) != Some(fragment.raw.as_str()) {
            skipped.push(skip("the link could not be rewritten as written"));
            continue;
        }
        let new_raw = format!("{}{new_fragment}{}", &raw[..start], &raw[end..]);
        // Checked: the new link lands on the same heading or block.
        let check = LinkRef::new(link.kind, new_raw.as_str(), source.clone(), params.profile);
        let checked = resolve(paths, &check, params);
        if !lands_right(checked.fragment.as_ref(), &fragment, ctx) {
            skipped.push(skip("no link to the new name could be written"));
            continue;
        }
        let at = if link.kind.is_wiki() {
            construct.find("[[").map(|o| o + 2)
        } else {
            construct.find("](").map(|o| o + 2)
        };
        let Some(pos) = at.and_then(|from| construct[from..].find(raw).map(|p| from + p)) else {
            skipped.push(skip(if link.kind == LinkKind::Reference {
                "a reference link: update its definition by hand"
            } else {
                "the link could not be rewritten as written"
            }));
            continue;
        };
        let new = format!(
            "{}{new_raw}{}",
            &construct[..pos],
            &construct[pos + raw.len()..]
        );
        if new != construct {
            changes.push(LinkChange {
                span: link.span,
                line: link.line,
                old: construct.to_owned(),
                new,
            });
        }
    }
    changes.sort_by_key(|c| c.span.start);
    changes.dedup_by_key(|c| c.span);
    changes
}

/// How a fragment is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    /// A wikilink subpath or a canvas `subpath`: as typed.
    Wiki,
    /// A Markdown link: percent-encoded.
    Markdown,
}

/// The new text of `fragment` (after its `#`), `None` when the rename
/// does not change where it lands, or why it cannot be rewritten.
fn new_fragment(
    fragment: &Fragment,
    style: Style,
    ctx: &Ctx<'_>,
) -> Option<Result<String, &'static str>> {
    match (&ctx.anchor, &fragment.kind) {
        (Anchor::Block { old, new }, FragmentKind::Block(id)) => {
            if id != old && !id.eq_ignore_ascii_case(old) {
                return None;
            }
            let at = fragment.raw.rfind('^')?;
            Some(Ok(format!("{}^{new}", &fragment.raw[..at])))
        }
        (
            Anchor::Heading {
                before,
                after,
                primary,
            },
            FragmentKind::Heading(parts),
        ) => {
            let chain = heading_chain(before, parts, *primary)?;
            let changed: Vec<usize> = chain
                .iter()
                .enumerate()
                .filter(|(_, (i, how))| key(before, *i, *how) != key(after, *i, *how))
                .map(|(j, _)| j)
                .collect();
            if changed.is_empty() {
                return None;
            }
            let pieces = pieces(&fragment.raw);
            if pieces.len() != parts.len() {
                return Some(Err("the link could not be rewritten as written"));
            }
            let mut out = String::with_capacity(fragment.raw.len() + 16);
            let mut at = 0;
            for (j, piece) in pieces.iter().enumerate() {
                out.push_str(&fragment.raw[at..piece.start]);
                let written = &fragment.raw[piece.clone()];
                if changed.contains(&j) {
                    let (i, how) = chain[j];
                    match piece_text(after, i, how, style, written) {
                        Some(text) => out.push_str(&text),
                        None => return Some(Err("no link to the new name could be written")),
                    }
                } else {
                    out.push_str(written);
                }
                at = piece.end;
            }
            out.push_str(&fragment.raw[at..]);
            Some(Ok(out))
        }
        _ => None,
    }
}

/// Whether `checked` (the fragment of a rewritten link, resolved again)
/// lands where `old` landed before the rename.
fn lands_right(checked: Option<&Fragment>, old: &Fragment, ctx: &Ctx<'_>) -> bool {
    let Some(checked) = checked else {
        return false;
    };
    match (&ctx.anchor, &checked.kind, &old.kind) {
        (Anchor::Block { new, .. }, FragmentKind::Block(id), _) => id == new,
        (
            Anchor::Heading {
                before,
                after,
                primary,
            },
            FragmentKind::Heading(new_parts),
            FragmentKind::Heading(old_parts),
        ) => {
            let was = heading_chain(before, old_parts, *primary);
            let now = heading_chain(after, new_parts, *primary);
            match (was, now) {
                (Some(was), Some(now)) => {
                    was.len() == now.len() && was.iter().zip(&now).all(|(a, b)| a.0 == b.0)
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// How heading `i` of `headings` is named under `how`.
fn key(headings: &[IndexedHeading], i: usize, how: Anchoring) -> String {
    match how {
        Anchoring::HeadingText => nfc_key(&heading_key(&headings[i].source)).into_owned(),
        Anchoring::Slug(alg) => heading_slugs(headings, alg)
            .into_iter()
            .nth(i)
            .unwrap_or_default(),
        Anchoring::None => String::new(),
    }
}

/// The byte ranges of the non-blank `#`-separated parts of a fragment's
/// raw text.
fn pieces(raw: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    for part in raw.split('#') {
        if !part.trim().is_empty() {
            out.push(at..at + part.len());
        }
        at += part.len() + 1;
    }
    out
}

/// The new text of one fragment part naming heading `i` of `headings`
/// under `how`, written like `written` (the old part).
fn piece_text(
    headings: &[IndexedHeading],
    i: usize,
    how: Anchoring,
    style: Style,
    written: &str,
) -> Option<String> {
    let text = match how {
        Anchoring::HeadingText => subpath_text(&headings[i].source),
        Anchoring::Slug(alg) => {
            let slug = heading_slugs(headings, alg).into_iter().nth(i)?;
            let prefix = if written.starts_with("user-content-") {
                "user-content-"
            } else {
                ""
            };
            format!("{prefix}{slug}")
        }
        Anchoring::None => return None,
    };
    if text.is_empty() {
        return None;
    }
    Some(match style {
        Style::Wiki => text,
        // Literal spaces stay literal (an angle-bracket destination).
        Style::Markdown if written.contains(' ') && !text.contains(['<', '>']) => text,
        Style::Markdown => encode_segment(&text),
    })
}

/// Heading text as a subpath writes it: without `[`, `]`, `|`, `#`, `^`,
/// whitespace runs as one space.
fn subpath_text(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if matches!(c, '[' | ']' | '|' | '#' | '^') {
                ' '
            } else {
                c
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Plans the edits of note `source`, read from disk.
fn plan_other(
    root: &Path,
    paths: &PathIndex,
    params: &ProfileParams,
    source: &VaultPath,
    ctx: &Ctx<'_>,
    edits: &mut Vec<FileEdit>,
    skipped: &mut Vec<Skipped>,
) {
    let skip_note = |reason: String, skipped: &mut Vec<Skipped>| {
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
            skip_note(format!("cannot be read: {err}"), skipped);
            return;
        }
    };
    let Ok(text) = String::from_utf8(bytes) else {
        skip_note(
            "is not UTF-8: its links were not updated".to_owned(),
            skipped,
        );
        return;
    };
    let data = extract(&text, params.profile);
    let changes = link_changes(paths, params, source, &text, &data, ctx, skipped);
    if changes.is_empty() {
        return;
    }
    let after = super::plan::apply_changes(&text, &changes);
    edits.push(FileEdit {
        path: source.clone(),
        moved_to: source.clone(),
        before_hash: ContentHash::of(text.as_bytes()),
        before: text,
        after,
        changes,
    });
}

/// Plans the edits of canvas `source`: the `subpath` of every file card
/// showing the note, when it names the renamed heading or block.
fn plan_canvas(
    root: &Path,
    note: &VaultPath,
    source: &VaultPath,
    ctx: &Ctx<'_>,
    edits: &mut Vec<FileEdit>,
    skipped: &mut Vec<Skipped>,
) {
    let Ok(bytes) = std::fs::read(source.to_fs_path(root)) else {
        return;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return;
    };
    if !text.contains("\"subpath\"") {
        return;
    }
    let Ok(refs) = gv_canvas::patch::subpath_refs(&text) else {
        return;
    };
    let mut changes = Vec::new();
    let mut patches = Vec::new();
    for r in refs {
        let shows_note = r
            .file
            .as_deref()
            .and_then(|f| VaultPath::new(f.trim_start_matches('/')).ok())
            .is_some_and(|f| f == *note);
        let Some(sub) = r.value.strip_prefix('#').filter(|_| shows_note) else {
            continue;
        };
        let fragment = Fragment::from_wikilink(sub);
        let line = line_of(&text, r.range.start);
        let new = match new_fragment(&fragment, Style::Wiki, ctx) {
            None => continue,
            Some(Err(reason)) => {
                skipped.push(Skipped {
                    source: source.clone(),
                    line,
                    link: r.value.clone(),
                    reason: reason.to_owned(),
                });
                continue;
            }
            Some(Ok(new)) => format!("#{new}"),
        };
        if !lands_right(Some(&Fragment::from_wikilink(&new[1..])), &fragment, ctx) {
            skipped.push(Skipped {
                source: source.clone(),
                line,
                link: r.value.clone(),
                reason: "no subpath with the new name could be written".to_owned(),
            });
            continue;
        }
        let encoded = gv_canvas::patch::encode_json_string(&new);
        changes.push(LinkChange {
            span: Span::of(r.range.clone()),
            line,
            old: text[r.range.clone()].to_owned(),
            new: encoded.clone(),
        });
        patches.push(gv_canvas::patch::Patch {
            range: r.range,
            replacement: encoded,
        });
    }
    if patches.is_empty() {
        return;
    }
    let Ok(after) = gv_canvas::patch::apply_patches(&text, &patches) else {
        skipped.push(Skipped {
            source: source.clone(),
            line: 0,
            link: String::new(),
            reason: "its file cards could not be updated".to_owned(),
        });
        return;
    };
    edits.push(FileEdit {
        path: source.clone(),
        moved_to: source.clone(),
        before_hash: ContentHash::of(text.as_bytes()),
        before: text,
        after,
        changes,
    });
}

/// The byte range of 0-based line `line` of `text`, without its line
/// break.
fn line_range(text: &str, line: usize) -> Option<Range<usize>> {
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    let end = text[start..].find('\n').map_or(text.len(), |n| start + n);
    let end = if text[start..end].ends_with('\r') {
        end - 1
    } else {
        end
    };
    Some(start..end)
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
