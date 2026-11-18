// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Git conflict markers in a note (plan 10 § Git awareness): finding them,
//! and splitting the text at them into a comparison for the compare and
//! merge view, the lines between `<<<<<<<` and `=======` on the left
//! ("ours") and those between `=======` and `>>>>>>>` on the right
//! ("theirs").
//!
//! - A marker is a line that starts with exactly seven `<`, `=` or `>`
//!   (then the end of the line, or a space and a label for `<` and `>`),
//!   with no indentation.
//! - Markers inside a fenced code block are text, not markers (a note may
//!   show a conflict as an example). Fences are only followed outside
//!   conflicts: the two sides of a conflict may each open or close one.
//! - A diff3 base section (`|||||||` up to `=======`) is left out: the
//!   view compares the two sides.
//! - A conflict that is not closed is text.

use gv_text::LineEnding;

use super::{Comparison, Hunk, HunkKind, Region, Sides};

/// Whether `text` holds at least one complete conflict.
pub fn has_conflict_markers(text: &str) -> bool {
    // Most notes have no marker at all: one fast search decides.
    text.contains("<<<<<<<") && parse(text).is_some()
}

/// `text` split at its conflict markers, each conflict a changed region
/// nobody chose a side for yet, or `None` when it has none. The sides are
/// named after the first conflict's labels (`HEAD`, a branch), else
/// "Ours" and "Theirs".
pub fn conflict_comparison(text: &str, line_ending: LineEnding) -> Option<Comparison> {
    let (regions, sides) = parse(text)?;
    Some(Comparison::from_regions(regions, sides, line_ending))
}

/// A line's kind of marker, with its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker<'a> {
    Start(&'a str),
    Base,
    Middle,
    End(&'a str),
}

/// The marker `line` (without its terminator) is, if any.
fn marker(line: &str) -> Option<Marker<'_>> {
    let lead = line.as_bytes().first()?;
    let kind = match lead {
        b'<' | b'|' | b'=' | b'>' => *lead,
        _ => return None,
    };
    let run = line.bytes().take_while(|b| *b == kind).count();
    if run != 7 {
        return None;
    }
    let rest = &line[7..];
    let label = match rest.chars().next() {
        None => "",
        Some(' ' | '\t') => rest.trim(),
        Some(_) => return None,
    };
    match kind {
        b'<' => Some(Marker::Start(label)),
        b'|' => Some(Marker::Base),
        // Git writes it alone on its line.
        b'=' if label.is_empty() => Some(Marker::Middle),
        b'>' => Some(Marker::End(label)),
        _ => None,
    }
}

/// A fence opening line: its character and length.
fn fence_open(line: &str) -> Option<(u8, usize)> {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let ch = *rest.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let run = rest.bytes().take_while(|b| *b == ch).count();
    if run < 3 {
        return None;
    }
    // A backtick fence's info string has no backtick.
    if ch == b'`' && rest[run..].contains('`') {
        return None;
    }
    Some((ch, run))
}

/// Whether `line` closes a fence of `ch` at least `len` long.
fn fence_close(line: &str, (ch, len): (u8, usize)) -> bool {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    let run = rest.bytes().take_while(|b| *b == ch).count();
    run >= len && rest[run..].trim().is_empty()
}

/// Where the scan is inside a conflict.
enum Part {
    Ours,
    Base,
    Theirs,
}

/// A conflict being read.
struct Open<'a> {
    part: Part,
    /// The text from its `<<<<<<<` line on, kept for a conflict that never
    /// closes.
    raw: String,
    ours: String,
    theirs: String,
    start: &'a str,
}

fn parse(text: &str) -> Option<(Vec<Region>, Sides)> {
    let mut regions = Vec::new();
    let mut plain = String::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut open: Option<Open<'_>> = None;
    let mut labels: Option<(String, String)> = None;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if let Some(conflict) = open.as_mut() {
            conflict.raw.push_str(line);
            match (marker(bare), &conflict.part) {
                (Some(Marker::Base), Part::Ours) => conflict.part = Part::Base,
                (Some(Marker::Middle), Part::Ours | Part::Base) => conflict.part = Part::Theirs,
                (Some(Marker::End(end)), Part::Theirs) => {
                    let Open {
                        ours,
                        theirs,
                        start,
                        ..
                    } = open.take()?;
                    if labels.is_none() {
                        labels = Some((start.to_owned(), end.to_owned()));
                    }
                    if !plain.is_empty() {
                        regions.push(Region::Unchanged(std::mem::take(&mut plain)));
                    }
                    regions.push(Region::Changed(Hunk {
                        kind: HunkKind::Conflict,
                        left: ours,
                        right: theirs,
                        pick: None,
                    }));
                }
                (_, Part::Ours) => conflict.ours.push_str(line),
                (_, Part::Base) => {}
                (_, Part::Theirs) => conflict.theirs.push_str(line),
            }
            continue;
        }
        if let Some(f) = fence {
            if fence_close(bare, f) {
                fence = None;
            }
            plain.push_str(line);
            continue;
        }
        if let Some(Marker::Start(label)) = marker(bare) {
            open = Some(Open {
                part: Part::Ours,
                raw: line.to_owned(),
                ours: String::new(),
                theirs: String::new(),
                start: label,
            });
            continue;
        }
        fence = fence_open(bare);
        plain.push_str(line);
    }
    // A conflict that never closed is text.
    if let Some(conflict) = open {
        plain.push_str(&conflict.raw);
    }
    let (start, end) = labels?;
    if !plain.is_empty() {
        regions.push(Region::Unchanged(plain));
    }
    let name = |label: String, fallback: &str| {
        if label.is_empty() {
            fallback.to_owned().into()
        } else {
            label.into()
        }
    };
    let sides = Sides {
        left: name(start, "Ours"),
        right: name(end, "Theirs"),
    };
    Some((regions, sides))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::Pick;

    const NOTE: &str = "# Plan\n\
        intro\n\
        <<<<<<< HEAD\n\
        ours one\n\
        ours two\n\
        =======\n\
        theirs\n\
        >>>>>>> feature\n\
        outro\n";

    #[test]
    fn finds_a_complete_conflict() {
        assert!(has_conflict_markers(NOTE));
        assert!(!has_conflict_markers("# Plan\n\nno conflict\n"));
        // Not closed: text.
        assert!(!has_conflict_markers("<<<<<<< HEAD\na\n=======\nb\n"));
        // Eight `<` is not a marker, nor an indented one.
        assert!(!has_conflict_markers(
            "<<<<<<<< x\na\n=======\nb\n>>>>>>> y\n"
        ));
        assert!(!has_conflict_markers(
            " <<<<<<< x\na\n=======\nb\n>>>>>>> y\n"
        ));
    }

    #[test]
    fn markers_in_a_code_fence_are_text() {
        let text = "```diff\n<<<<<<< HEAD\na\n=======\nb\n>>>>>>> x\n```\n";
        assert!(!has_conflict_markers(text));
        let tilde = "~~~~\n<<<<<<< HEAD\na\n=======\nb\n>>>>>>> x\n~~~~\nafter\n";
        assert!(!has_conflict_markers(tilde));
        // After the fence closes, markers count again.
        let after = "```\ncode\n```\n<<<<<<< HEAD\na\n=======\nb\n>>>>>>> x\n";
        assert!(has_conflict_markers(after));
    }

    #[test]
    fn splits_ours_and_theirs_and_names_the_sides() {
        let c = conflict_comparison(NOTE, LineEnding::Lf).expect("a conflict");
        assert_eq!(c.sides().left.as_ref(), "HEAD");
        assert_eq!(c.sides().right.as_ref(), "feature");
        assert_eq!(c.hunk_count(), 1);
        assert_eq!(c.unresolved(), 1);
        let hunk = c.hunks().next().expect("hunk");
        assert_eq!(hunk.left, "ours one\nours two\n");
        assert_eq!(hunk.right, "theirs\n");
        assert_eq!(
            c.regions().first(),
            Some(&Region::Unchanged("# Plan\nintro\n".into()))
        );
    }

    #[test]
    fn picks_make_the_merged_text() {
        let mut c = conflict_comparison(NOTE, LineEnding::Lf).expect("a conflict");
        c.pick(0, Pick::Right);
        assert_eq!(c.result(), "# Plan\nintro\ntheirs\noutro\n");
        c.pick(0, Pick::Both);
        assert_eq!(
            c.result(),
            "# Plan\nintro\nours one\nours two\ntheirs\noutro\n"
        );
    }

    #[test]
    fn unresolved_conflicts_keep_their_markers() {
        let c = conflict_comparison(NOTE, LineEnding::Lf).expect("a conflict");
        assert_eq!(c.result(), NOTE);
    }

    #[test]
    fn diff3_base_is_left_out_and_crlf_is_kept() {
        let text = "a\r\n<<<<<<< ours\r\nx\r\n||||||| base\r\nold\r\n=======\r\ny\r\n>>>>>>> theirs\r\nb\r\n";
        let mut c = conflict_comparison(text, LineEnding::CrLf).expect("a conflict");
        let hunk = c.hunks().next().expect("hunk");
        assert_eq!(hunk.left, "x\r\n");
        assert_eq!(hunk.right, "y\r\n");
        c.pick_all(Pick::Left);
        assert_eq!(c.result(), "a\r\nx\r\nb\r\n");
    }

    #[test]
    fn several_conflicts_and_unnamed_sides() {
        let text = "<<<<<<<\na\n=======\nb\n>>>>>>>\nmid\n<<<<<<< x\nc\n=======\n>>>>>>> y\n";
        let mut c = conflict_comparison(text, LineEnding::Lf).expect("conflicts");
        assert_eq!(c.sides().left.as_ref(), "Ours");
        assert_eq!(c.sides().right.as_ref(), "Theirs");
        assert_eq!(c.hunk_count(), 2);
        c.pick(0, Pick::Left);
        c.pick(1, Pick::Right);
        assert_eq!(c.result(), "a\nmid\n");
    }
}
