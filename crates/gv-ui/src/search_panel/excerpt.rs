// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! One-line excerpts of note text for result lists (search results and
//! backlinks): the inline markup of the profile is removed (emphasis and
//! code markers, link destinations, math delimiters, comments, block ids;
//! wikilinks read as their text), the marked ranges follow, and the start
//! is cut so the first mark stays in view. Formulas keep their TeX in the
//! text, and where it is ([`Formula`]), so a row can typeset them.

use std::ops::Range;

use gv_syntax::ParseOptions;
use gv_syntax::inline::{InlineInput, InlineKind, InlineOptions, parse_inlines_with};
use gv_vault::ProfileId;

/// Text with marked byte ranges.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Marked {
    pub text: String,
    pub marks: Vec<Range<usize>>,
    /// The formulas (`$…$`, `$$…$$`, as the profile reads math), in order.
    pub math: Vec<Formula>,
}

/// A formula of an excerpt: its TeX, shown at `out` while not typeset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Formula {
    /// The bytes of the excerpt's text that show the TeX.
    pub out: Range<usize>,
    /// The TeX, without its delimiters.
    pub tex: String,
    /// Written as display math (`$$…$$`).
    pub display: bool,
}

/// A piece of the output: source bytes `src` shown as `out` (the same
/// bytes when `verbatim`).
struct Piece {
    src: Range<usize>,
    out: Range<usize>,
    verbatim: bool,
}

/// The inline switches `profile` reads a line with.
pub(crate) fn inline_options(profile: ProfileId) -> InlineOptions {
    ParseOptions::builtin(profile).inline()
}

/// `src` (inline content on one line) as it reads, without its markup,
/// with `marks` (byte ranges of `src`) moved along. A mark that covers
/// only markup is dropped.
pub(crate) fn strip(src: &str, marks: &[Range<usize>], options: InlineOptions) -> Marked {
    let whole = 0..src.len();
    let tree = parse_inlines_with(
        src,
        InlineInput {
            segments: std::slice::from_ref(&whole),
        },
        &(),
        options,
    );
    let mut text = String::with_capacity(src.len());
    let mut pieces: Vec<Piece> = Vec::new();
    let mut push = |src_range: Range<usize>, shown: Option<&str>, text: &mut String| {
        let start = text.len();
        let verbatim = shown.is_none();
        text.push_str(shown.unwrap_or(&src[src_range.clone()]));
        if text.len() > start {
            pieces.push(Piece {
                src: src_range,
                out: start..text.len(),
                verbatim,
            });
        }
    };
    let mut skip_to = 0;
    // Formulas: source range, TeX, display.
    let mut formulas: Vec<(Range<usize>, String, bool)> = Vec::new();
    for node in tree.nodes() {
        let r = node.range();
        if r.start < skip_to || r.end > src.len() {
            continue;
        }
        match node.kind() {
            // Its TeX follows as `MathText`, shown verbatim.
            kind @ (InlineKind::InlineMath | InlineKind::DisplayMath) => {
                if let Some(tex) = node.math() {
                    formulas.push((r, tex.to_owned(), kind == InlineKind::DisplayMath));
                }
            }
            InlineKind::Wikilink | InlineKind::Embed => {
                let label = link_label(&src[r.clone()]);
                push(r.clone(), Some(&label), &mut text);
                skip_to = r.end;
            }
            // Tags read as written, `#` included.
            InlineKind::Tag => {
                push(r.clone(), None, &mut text);
                skip_to = r.end;
            }
            InlineKind::Comment
            | InlineKind::BlockId
            | InlineKind::FootnoteRef
            | InlineKind::RawHtml => skip_to = r.end,
            InlineKind::Text
            | InlineKind::CodeText
            | InlineKind::MathText
            | InlineKind::AutolinkText
            | InlineKind::Entity => push(r, None, &mut text),
            // The escaped character without its backslash.
            InlineKind::Escape => push(r.start + 1..r.end, None, &mut text),
            InlineKind::SoftBreak | InlineKind::HardBreak => push(r, Some(" "), &mut text),
            _ => {}
        }
    }
    let to_out = |at: usize, start: bool| -> Option<usize> {
        if start {
            pieces.iter().find(|p| p.src.end > at).map(|p| {
                if p.verbatim && at >= p.src.start {
                    p.out.start + (at - p.src.start)
                } else {
                    p.out.start
                }
            })
        } else {
            pieces.iter().rev().find(|p| p.src.start < at).map(|p| {
                if p.verbatim && at <= p.src.end {
                    p.out.start + (at - p.src.start)
                } else {
                    p.out.end
                }
            })
        }
    };
    let marks = marks
        .iter()
        .filter_map(|m| Some(to_out(m.start, true)?..to_out(m.end, false)?))
        .filter(|m| {
            m.start < m.end && text.is_char_boundary(m.start) && text.is_char_boundary(m.end)
        })
        .collect();
    let math = formulas
        .into_iter()
        .filter_map(|(r, tex, display)| {
            let inside = pieces
                .iter()
                .filter(|p| p.src.start >= r.start && p.src.end <= r.end);
            let start = inside.clone().map(|p| p.out.start).min()?;
            let end = inside.map(|p| p.out.end).max()?;
            (!tex.trim().is_empty()).then_some(Formula {
                out: start..end,
                tex,
                display,
            })
        })
        .collect();
    Marked { text, marks, math }
}

/// A link construct as its text reads: `[[Note|shown]]` is "shown",
/// `[[Note#Part]]` "Note > Part", `[text](url)` "text", `<url>` "url".
pub(crate) fn link_label(construct: &str) -> String {
    let s = construct.trim();
    let s = s.strip_prefix('!').unwrap_or(s);
    if let Some(inner) = s.strip_prefix("[[").and_then(|r| r.strip_suffix("]]")) {
        if let Some((_, alias)) = inner.split_once('|') {
            return alias.trim().to_string();
        }
        return match inner.split_once('#') {
            Some(("", fragment)) => fragment.trim_start_matches('^').trim().to_string(),
            Some((name, fragment)) => format!(
                "{} > {}",
                name.trim(),
                fragment.trim_start_matches('^').trim()
            ),
            None => inner.trim().to_string(),
        };
    }
    if let Some(rest) = s.strip_prefix('[')
        && let Some(close) = rest.find(']')
    {
        let text = &rest[..close];
        if !text.is_empty() {
            return text.to_string();
        }
    }
    if let Some(inner) = s.strip_prefix('<').and_then(|r| r.strip_suffix('>')) {
        return inner.to_string();
    }
    s.to_string()
}

/// Cuts the start of `m` so at most about `lead` characters come before
/// its first mark (at a word when one is near). Returns whether it cut.
pub(crate) fn keep_lead(m: &mut Marked, lead: usize) -> bool {
    let Some(first) = m.marks.first().map(|r| r.start) else {
        return false;
    };
    let before = &m.text[..first];
    let count = before.chars().count();
    if count <= lead {
        return false;
    }
    let mut cut = before
        .char_indices()
        .nth(count - lead)
        .map_or(first, |(i, _)| i);
    // Start at a word: the next space within the lead, else mid-word.
    if let Some(space) = before[cut..].find(' ')
        && cut + space + 1 < first
    {
        cut += space + 1;
    }
    m.text.drain(..cut);
    for r in &mut m.marks {
        *r = r.start.saturating_sub(cut)..r.end.saturating_sub(cut);
    }
    m.marks.retain(|r| r.start < r.end);
    // A formula cut into keeps what is left of its TeX as text.
    m.math.retain(|f| f.out.start >= cut);
    for f in &mut m.math {
        f.out = f.out.start - cut..f.out.end - cut;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obsidian() -> InlineOptions {
        inline_options(ProfileId::Obsidian)
    }

    fn marked(m: &Marked) -> Vec<&str> {
        m.marks.iter().map(|r| &m.text[r.clone()]).collect()
    }

    #[test]
    fn markup_goes_and_marks_follow() {
        let src = "fits a **harmonic model** to [the record](a.md), see [[Data pipeline#Validation]] $\\pm 4$ cm %%note%% #tides";
        let at = |s: &str| {
            let i = src.find(s).expect("in src");
            i..i + s.len()
        };
        let m = strip(
            src,
            &[
                at("harmonic"),
                at("record"),
                at("Data"),
                at("4"),
                at("#tides"),
            ],
            obsidian(),
        );
        assert_eq!(
            m.text,
            "fits a harmonic model to the record, see Data pipeline > Validation \\pm 4 cm  #tides"
        );
        // The wikilink is one piece: its mark covers its text.
        assert_eq!(
            marked(&m),
            [
                "harmonic",
                "record",
                "Data pipeline > Validation",
                "4",
                "#tides"
            ]
        );
        // Aliases, code, escapes, block ids.
        let m = strip("[[Note|shown]] `x*y` \\*a\\* end ^b1", &[], obsidian());
        assert_eq!(m.text, "shown x*y *a* end");
        // A mark on markup alone is dropped.
        let m = strip("**bold**", std::slice::from_ref(&(0..2)), obsidian());
        assert_eq!(m.text, "bold");
        assert!(m.marks.is_empty());
    }

    #[test]
    fn formulas_are_found_as_the_profile_reads_math() {
        let tex = |m: &Marked| -> Vec<(String, String, bool)> {
            m.math
                .iter()
                .map(|f| (m.text[f.out.clone()].to_owned(), f.tex.clone(), f.display))
                .collect()
        };
        let src = r"area $\pi r^2$ and $$e^x$$ grows";
        let m = strip(src, &[], obsidian());
        assert_eq!(m.text, r"area \pi r^2 and e^x grows");
        assert_eq!(
            tex(&m),
            [
                (r"\pi r^2".to_owned(), r"\pi r^2".to_owned(), false),
                ("e^x".to_owned(), "e^x".to_owned(), true),
            ]
        );
        // Currency and escaped dollars are text.
        let m = strip("costs $5 and $6 today", &[], obsidian());
        assert_eq!(m.text, "costs $5 and $6 today");
        assert!(m.math.is_empty(), "{m:?}");
        let m = strip(r"not \$y$ here", &[], obsidian());
        assert_eq!(m.text, "not $y$ here");
        assert!(m.math.is_empty(), "{m:?}");
        // Without math in the profile, dollars are text.
        let m = strip(src, &[], inline_options(ProfileId::CommonMark));
        assert_eq!(m.text, src);
        assert!(m.math.is_empty(), "{m:?}");
        // A cut into a formula leaves its TeX as text; later ones move.
        let src = "long text before $a+b$ then $c$ and the word";
        let at = src.find("word").expect("word");
        let mut m = strip(src, std::slice::from_ref(&(at..at + 4)), obsidian());
        let cut = m.text.find("+b").expect("in text");
        let lead = m.text[cut..m.marks[0].start].chars().count();
        assert!(keep_lead(&mut m, lead));
        assert_eq!(tex(&m), [("c".to_owned(), "c".to_owned(), false)]);
    }

    #[test]
    fn the_start_is_cut_to_keep_the_mark_in_view() {
        let text = "The station logs the water level every six minutes and fits a harmonic model";
        let at = text.find("harmonic").expect("word");
        let mut m = Marked {
            text: text.to_owned(),
            marks: std::iter::once(at..at + 8).collect(),
            math: Vec::new(),
        };
        assert!(keep_lead(&mut m, 12));
        assert_eq!(m.text, "and fits a harmonic model");
        assert_eq!(marked(&m), ["harmonic"]);
        // Close enough already: unchanged.
        assert!(!keep_lead(&mut m, 12));
    }
}
