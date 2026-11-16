// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Line diffs for external changes: the minimal reload transaction, and the
//! 3-way merge of the buffer and a version changed on disk against their
//! common base.
//!
//! Lines end after LF, CRLF or a lone CR (the editor's line model) and keep
//! their terminators, so merged text has exactly the line endings of the
//! side each line came from. Diffs are Myers diffs from `similar`.

use std::ops::Range;

use gv_text::{LineEnding, Transaction};
use similar::{Algorithm, DiffOp, capture_diff_slices};

/// Splits `text` into lines that keep their terminators (LF, CRLF, lone
/// CR). The last line has none if the text does not end with one.
fn split_lines(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&text[start..=i]);
                start = i + 1;
            }
            b'\r' => {
                let end = if bytes.get(i + 1) == Some(&b'\n') {
                    i + 1
                } else {
                    i
                };
                lines.push(&text[start..=end]);
                start = end + 1;
                i = end;
            }
            _ => {}
        }
        i += 1;
    }
    if start < bytes.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// A transaction that turns `current` into `new`, touching only what
/// differs: changed lines are found with a line diff, then each changed
/// run is narrowed to the bytes that differ. Applied to the buffer as one
/// undoable transaction, it keeps anchors, folds and selections in
/// untouched text (reloading a clean buffer after an external change).
pub fn reload_transaction(current: &str, new: &str) -> Transaction {
    let old_lines = split_lines(current);
    let new_lines = split_lines(new);
    let old_starts = line_starts(&old_lines);
    let new_starts = line_starts(&new_lines);
    let mut transaction = Transaction::new();
    for op in capture_diff_slices(Algorithm::Myers, &old_lines, &new_lines) {
        let (old_range, new_range) = match op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => (old_index..old_index + old_len, new_index..new_index),
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => (old_index..old_index, new_index..new_index + new_len),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => (
                old_index..old_index + old_len,
                new_index..new_index + new_len,
            ),
        };
        let old_bytes = old_starts[old_range.start]..old_starts[old_range.end];
        let new_bytes = new_starts[new_range.start]..new_starts[new_range.end];
        let (old_bytes, new_bytes) = narrow(current, new, old_bytes, new_bytes);
        transaction.push(old_bytes, &new[new_bytes]);
    }
    transaction
}

/// Byte offsets of each line start, plus the end.
fn line_starts(lines: &[&str]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(lines.len() + 1);
    let mut pos = 0;
    starts.push(0);
    for line in lines {
        pos += line.len();
        starts.push(pos);
    }
    starts
}

/// Trims the common prefix and suffix (on character boundaries) of two
/// changed runs.
fn narrow(
    old: &str,
    new: &str,
    mut a: Range<usize>,
    mut b: Range<usize>,
) -> (Range<usize>, Range<usize>) {
    let prefix = old[a.clone()]
        .char_indices()
        .zip(new[b.clone()].chars())
        .find(|((_, x), y)| x != y)
        .map_or_else(|| a.len().min(b.len()), |((i, _), _)| i);
    a.start += prefix;
    b.start += prefix;
    let suffix: usize = old[a.clone()]
        .chars()
        .rev()
        .zip(new[b.clone()].chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum();
    a.end -= suffix;
    b.end -= suffix;
    (a, b)
}

/// One region of a 3-way merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeChunk {
    /// Lines both sides left as in the base.
    Unchanged(String),
    /// Only the buffer ("mine") changed these lines.
    Ours(String),
    /// Only the disk version ("theirs") changed these lines.
    Theirs(String),
    /// Both sides made the same change.
    Both(String),
    /// Both sides changed the same lines differently.
    Conflict {
        /// The base's lines.
        base: String,
        /// The buffer's lines.
        ours: String,
        /// The disk version's lines.
        theirs: String,
    },
}

/// The result of [`merge3`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merge {
    chunks: Vec<MergeChunk>,
}

/// The labels of conflict markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkerLabels<'a> {
    /// After `<<<<<<<`: the buffer's side.
    pub ours: &'a str,
    /// After `>>>>>>>`: the disk version's side.
    pub theirs: &'a str,
}

impl Default for MarkerLabels<'_> {
    /// `mine` and `theirs`.
    fn default() -> Self {
        Self {
            ours: "mine",
            theirs: "theirs",
        }
    }
}

impl Merge {
    /// The regions, in order.
    pub fn chunks(&self) -> &[MergeChunk] {
        &self.chunks
    }

    /// Whether no region conflicts.
    pub fn is_clean(&self) -> bool {
        self.conflict_count() == 0
    }

    /// How many regions conflict.
    pub fn conflict_count(&self) -> usize {
        self.chunks
            .iter()
            .filter(|c| matches!(c, MergeChunk::Conflict { .. }))
            .count()
    }

    /// The merged text, if the merge is clean.
    pub fn clean_text(&self) -> Option<String> {
        self.is_clean()
            .then(|| self.text_with_markers(MarkerLabels::default(), LineEnding::Lf))
    }

    /// The merged text with each conflict written as
    /// `<<<<<<< ours` / `=======` / `>>>>>>> theirs` blocks. Marker lines end
    /// with `line_ending` (the file's dominant one); a side whose last line
    /// has no terminator gets one before the next marker.
    pub fn text_with_markers(&self, labels: MarkerLabels<'_>, line_ending: LineEnding) -> String {
        let eol = line_ending.as_str();
        let mut out = String::new();
        for chunk in &self.chunks {
            match chunk {
                MergeChunk::Unchanged(text)
                | MergeChunk::Ours(text)
                | MergeChunk::Theirs(text)
                | MergeChunk::Both(text) => out.push_str(text),
                MergeChunk::Conflict { ours, theirs, .. } => {
                    let side = |out: &mut String, text: &str| {
                        out.push_str(text);
                        if !text.is_empty() && !text.ends_with(['\n', '\r']) {
                            out.push_str(eol);
                        }
                    };
                    if !out.is_empty() && !out.ends_with(['\n', '\r']) {
                        out.push_str(eol);
                    }
                    out.push_str("<<<<<<< ");
                    out.push_str(labels.ours);
                    out.push_str(eol);
                    side(&mut out, ours);
                    out.push_str("=======");
                    out.push_str(eol);
                    side(&mut out, theirs);
                    out.push_str(">>>>>>> ");
                    out.push_str(labels.theirs);
                    out.push_str(eol);
                }
            }
        }
        out
    }
}

/// For each base line, the index of the matching line on the other side.
fn matches(base: &[&str], other: &[&str]) -> Vec<Option<usize>> {
    let mut map = vec![None; base.len()];
    for op in capture_diff_slices(Algorithm::Myers, base, other) {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for k in 0..len {
                map[old_index + k] = Some(new_index + k);
            }
        }
    }
    map
}

/// A line-based 3-way merge (diff3) of `ours` (the buffer) and `theirs`
/// (the disk version) against `base` (the text both started from: the
/// journal base).
///
/// Lines unchanged on both sides are stable; between stable lines, a region
/// changed on one side takes that side, a region changed identically on
/// both takes it once, and anything else is a conflict.
pub fn merge3(base: &str, ours: &str, theirs: &str) -> Merge {
    let single = |chunk: MergeChunk| Merge {
        chunks: vec![chunk],
    };
    if ours == theirs {
        return single(if ours == base {
            MergeChunk::Unchanged(base.to_owned())
        } else {
            MergeChunk::Both(ours.to_owned())
        });
    }
    if base == ours {
        return single(MergeChunk::Theirs(theirs.to_owned()));
    }
    if base == theirs {
        return single(MergeChunk::Ours(ours.to_owned()));
    }

    let mut chunks = Vec::new();
    for region in merge3_regions(base, ours, theirs) {
        push(&mut chunks, region.chunk());
    }
    Merge { chunks }
}

/// One region of a 3-way merge with the text of all three sides: the
/// stable runs (all three equal) between the regions that changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeRegion {
    /// The base's lines.
    pub base: String,
    /// The buffer's lines.
    pub ours: String,
    /// The disk version's lines.
    pub theirs: String,
}

impl MergeRegion {
    /// What the region is in a merge: unchanged, one side's change, the
    /// same change on both, or a conflict.
    pub fn chunk(&self) -> MergeChunk {
        if self.ours == self.theirs {
            if self.ours == self.base {
                MergeChunk::Unchanged(self.base.clone())
            } else {
                MergeChunk::Both(self.ours.clone())
            }
        } else if self.ours == self.base {
            MergeChunk::Theirs(self.theirs.clone())
        } else if self.theirs == self.base {
            MergeChunk::Ours(self.ours.clone())
        } else {
            MergeChunk::Conflict {
                base: self.base.clone(),
                ours: self.ours.clone(),
                theirs: self.theirs.clone(),
            }
        }
    }

    /// Whether all three sides are the same.
    pub fn is_unchanged(&self) -> bool {
        self.ours == self.base && self.theirs == self.base
    }
}

/// The regions of a line-based 3-way merge (as in [`merge3`]), each with
/// the text of the base, the buffer and the disk version, in order. Their
/// texts concatenate to the three inputs. Unlike [`merge3`], a change is
/// never taken as one region of the whole text: every changed run is its
/// own region between stable lines, which is what a compare view shows.
/// For a 2-way comparison pass the other side as the base too
/// (`merge3_regions(theirs, ours, theirs)`): every region is then the
/// buffer's change.
pub fn merge3_regions(base: &str, ours: &str, theirs: &str) -> Vec<MergeRegion> {
    let b = split_lines(base);
    let o = split_lines(ours);
    let t = split_lines(theirs);
    let to_ours = matches(&b, &o);
    let to_theirs = matches(&b, &t);

    let mut regions = Vec::new();
    let (mut i, mut j, mut k) = (0, 0, 0);
    loop {
        // The length of the stable run at (i, j, k).
        let mut run = 0;
        while i + run < b.len()
            && to_ours[i + run] == Some(j + run)
            && to_theirs[i + run] == Some(k + run)
        {
            run += 1;
        }
        if run > 0 {
            let text = b[i..i + run].concat();
            regions.push(MergeRegion {
                base: text.clone(),
                ours: text.clone(),
                theirs: text,
            });
            i += run;
            j += run;
            k += run;
            continue;
        }
        // The next base line matched on both sides starts the next stable
        // run; everything before it is one unstable region.
        let next = (i..b.len()).find_map(|n| match (to_ours[n], to_theirs[n]) {
            (Some(jn), Some(kn)) if jn >= j && kn >= k => Some((n, jn, kn)),
            _ => None,
        });
        let (ni, nj, nk) = next.unwrap_or((b.len(), o.len(), t.len()));
        if (ni, nj, nk) == (i, j, k) {
            break;
        }
        regions.push(MergeRegion {
            base: b[i..ni].concat(),
            ours: o[j..nj].concat(),
            theirs: t[k..nk].concat(),
        });
        (i, j, k) = (ni, nj, nk);
    }
    regions
}

/// Appends a chunk, dropping empty clean ones.
fn push(chunks: &mut Vec<MergeChunk>, chunk: MergeChunk) {
    let empty = match &chunk {
        MergeChunk::Unchanged(t)
        | MergeChunk::Ours(t)
        | MergeChunk::Theirs(t)
        | MergeChunk::Both(t) => t.is_empty(),
        MergeChunk::Conflict { .. } => false,
    };
    if !empty {
        chunks.push(chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gv_text::Buffer;

    #[test]
    fn lines_keep_every_terminator() {
        assert_eq!(split_lines("a\nb\r\nc\rd"), ["a\n", "b\r\n", "c\r", "d"]);
        assert_eq!(split_lines("\r\r\n\n"), ["\r", "\r\n", "\n"]);
        assert!(split_lines("").is_empty());
    }

    fn apply(current: &str, new: &str) -> String {
        let mut buffer = Buffer::new(current);
        buffer
            .apply(&reload_transaction(current, new))
            .expect("valid transaction");
        buffer.to_string()
    }

    #[test]
    fn reload_touches_only_the_difference() {
        let current = "# Title\r\n\r\nSome text here.\r\nMore.\r\n";
        let new = "# Title\r\n\r\nSome new text here.\r\nMore.\r\nAdded\r\n";
        let transaction = reload_transaction(current, new);
        assert_eq!(apply(current, new), new);
        let ranges: Vec<_> = transaction
            .replacements()
            .iter()
            .map(|r| (r.range.clone(), r.text.as_str()))
            .collect();
        assert_eq!(ranges, [(16..16, "new "), (35..35, "Added\r\n")]);
        for (a, b) in [
            ("", "x"),
            ("x", ""),
            ("a\nb\nc", "c\nb\na"),
            ("äöü\n", "äxü\n"),
            ("a\rb", "a\r\nb"),
            ("same", "same"),
        ] {
            assert_eq!(apply(a, b), b, "{a:?} -> {b:?}");
        }
        assert!(reload_transaction("same\n", "same\n").is_empty());
    }

    #[test]
    fn non_overlapping_changes_merge_cleanly() {
        let base = "one\ntwo\nthree\nfour\nfive\n";
        let ours = "ONE\ntwo\nthree\nfour\nfive\n";
        let theirs = "one\ntwo\nthree\nfour\nFIVE\nsix\n";
        let merge = merge3(base, ours, theirs);
        assert_eq!(
            merge.clean_text().as_deref(),
            Some("ONE\ntwo\nthree\nfour\nFIVE\nsix\n")
        );
        assert_eq!(merge.chunks().len(), 3);
    }

    #[test]
    fn same_change_on_both_sides_is_taken_once() {
        let merge = merge3("a\nb\nc\n", "a\nX\nc\nd\n", "a\nX\nc\n");
        assert_eq!(merge.clean_text().as_deref(), Some("a\nX\nc\nd\n"));
    }

    #[test]
    fn overlapping_changes_conflict_with_markers() {
        let base = "title\r\nbody\r\nend";
        let ours = "title\r\nmy body\r\nend";
        let theirs = "title\r\ntheir body\r\nend";
        let merge = merge3(base, ours, theirs);
        assert!(!merge.is_clean());
        assert_eq!(merge.conflict_count(), 1);
        assert_eq!(merge.clean_text(), None);
        assert_eq!(
            merge.text_with_markers(MarkerLabels::default(), LineEnding::CrLf),
            "title\r\n<<<<<<< mine\r\nmy body\r\n=======\r\ntheir body\r\n>>>>>>> theirs\r\nend"
        );
        assert_eq!(
            merge.chunks()[1],
            MergeChunk::Conflict {
                base: "body\r\n".to_owned(),
                ours: "my body\r\n".to_owned(),
                theirs: "their body\r\n".to_owned()
            }
        );
    }

    #[test]
    fn conflict_on_a_last_line_without_terminator() {
        let merge = merge3("a\nb", "a\nc", "a\nd");
        assert_eq!(
            merge.text_with_markers(MarkerLabels::default(), LineEnding::Lf),
            "a\n<<<<<<< mine\nc\n=======\nd\n>>>>>>> theirs\n"
        );
    }

    #[test]
    fn deletion_against_edit_conflicts_and_trivial_cases() {
        let merge = merge3("a\nb\nc\n", "a\nc\n", "a\nB\nc\n");
        assert_eq!(merge.conflict_count(), 1);
        assert_eq!(
            merge3("x\n", "x\n", "y\n").clean_text().as_deref(),
            Some("y\n")
        );
        assert_eq!(
            merge3("x\n", "y\n", "x\n").clean_text().as_deref(),
            Some("y\n")
        );
        assert_eq!(merge3("", "a", "a").clean_text().as_deref(), Some("a"));
        // Both appended different lines at the end: a conflict.
        assert_eq!(merge3("a\n", "a\nb\n", "a\nc\n").conflict_count(), 1);
        // Inserts at different places merge.
        assert_eq!(
            merge3("a\nb\nc\nd\n", "a\nX\nb\nc\nd\n", "a\nb\nc\nY\nd\n")
                .clean_text()
                .as_deref(),
            Some("a\nX\nb\nc\nY\nd\n")
        );
    }

    #[test]
    fn regions_carry_every_side_and_rebuild_the_inputs() {
        let base = "one\ntwo\nthree\nfour\nfive\n";
        let ours = "ONE\ntwo\nthree\nfour\nfive\n";
        let theirs = "one\ntwo\nthree\nfour\nFIVE\nsix\n";
        let regions = merge3_regions(base, ours, theirs);
        let kinds: Vec<_> = regions.iter().map(MergeRegion::chunk).collect();
        assert_eq!(
            kinds,
            [
                MergeChunk::Ours("ONE\n".to_owned()),
                MergeChunk::Unchanged("two\nthree\nfour\n".to_owned()),
                MergeChunk::Theirs("FIVE\nsix\n".to_owned()),
            ]
        );
        assert_eq!(regions[2].base, "five\n");
        let join = |f: fn(&MergeRegion) -> &str| regions.iter().map(f).collect::<String>();
        assert_eq!(join(|r| &r.base), base);
        assert_eq!(join(|r| &r.ours), ours);
        assert_eq!(join(|r| &r.theirs), theirs);
    }

    #[test]
    fn two_way_regions_are_all_the_buffers_changes() {
        // A whole-text change in `merge3` is one chunk; here every changed
        // run is its own region.
        let disk = "a\nb\nc\nd\n";
        let mine = "a\nB\nc\nd\ne\n";
        let regions = merge3_regions(disk, mine, disk);
        let changed: Vec<_> = regions.iter().filter(|r| !r.is_unchanged()).collect();
        assert_eq!(changed.len(), 2);
        assert!(
            changed
                .iter()
                .all(|r| matches!(r.chunk(), MergeChunk::Ours(_)))
        );
        assert_eq!(
            (changed[0].ours.as_str(), changed[0].theirs.as_str()),
            ("B\n", "b\n")
        );
        assert_eq!(
            (changed[1].ours.as_str(), changed[1].theirs.as_str()),
            ("e\n", "")
        );
        assert_eq!(merge3_regions("", "", ""), []);
        assert_eq!(merge3_regions("x", "x", "x").len(), 1);
    }
}
