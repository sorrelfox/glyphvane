// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the palette lists: the commands it offers, ranked against the query
//! with `nucleo-matcher`, and the letters each one matched.

use std::ops::Range;

use gv_settings::keymap::Category;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use unicode_segmentation::UnicodeSegmentation as _;

/// How many commands the palette remembers as recently run.
pub const RECENT_LIMIT: usize = 32;

/// A command the palette offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub title: String,
    pub category: Category,
    /// The label of the command's primary keys on this platform.
    pub shortcut: Option<String>,
}

/// Where a row's matched letters are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Highlight {
    /// The query is empty.
    None,
    /// Byte ranges of the title.
    Title(Vec<Range<usize>>),
    /// The title does not match but the id does: byte ranges of the id,
    /// which the row then shows after the title.
    Id(Vec<Range<usize>>),
}

/// One row of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The index of the candidate.
    pub index: usize,
    pub highlight: Highlight,
}

/// The rows for `query`, best first.
///
/// An empty query lists every candidate: the recently run commands first,
/// most recent first, then the rest by title, alphabetically (so that the
/// first row, which Enter runs, is not Quit). Otherwise the query
/// is matched fuzzily, word by word (any order), against each title and id;
/// a candidate ranks by its better score, then by how recently it ran, then
/// in registry order.
pub fn rows(candidates: &[Candidate], query: &str, recent: &[String]) -> Vec<Row> {
    let recency = |c: &Candidate| {
        recent
            .iter()
            .position(|id| *id == c.id)
            .unwrap_or(usize::MAX)
    };
    if query.trim().is_empty() {
        let mut order: Vec<(usize, String, usize)> = candidates
            .iter()
            .enumerate()
            .map(|(index, c)| (recency(c), c.title.to_lowercase(), index))
            .collect();
        order.sort_unstable();
        return order
            .into_iter()
            .map(|(_, _, index)| Row {
                index,
                highlight: Highlight::None,
            })
            .collect();
    }

    let pattern = Pattern::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let mut scored: Vec<(u32, usize, Row)> = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let mut find = |text: &str| {
            indices.clear();
            let score =
                pattern.indices(Utf32Str::new(text, &mut buf), &mut matcher, &mut indices)?;
            Some((score, ranges(text, &mut indices)))
        };
        let title = find(&candidate.title);
        let id = find(&candidate.id);
        let (score, highlight) = match (title, id) {
            (Some((t, ranges)), id) => (t.max(id.map_or(0, |(i, _)| i)), Highlight::Title(ranges)),
            (None, Some((i, ranges))) => (i, Highlight::Id(ranges)),
            (None, None) => continue,
        };
        scored.push((score, recency(candidate), Row { index, highlight }));
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.index.cmp(&b.2.index))
    });
    scored.into_iter().map(|(_, _, row)| row).collect()
}

/// The byte ranges of `text` at the matcher's character `indices`, merged
/// where they touch. The matcher counts bytes in ASCII text and grapheme
/// clusters otherwise.
fn ranges(text: &str, indices: &mut Vec<u32>) -> Vec<Range<usize>> {
    indices.sort_unstable();
    indices.dedup();
    let spans: Vec<Range<usize>> = if text.is_ascii() {
        indices
            .iter()
            .map(|&i| i as usize..i as usize + 1)
            .collect()
    } else {
        text.grapheme_indices(true)
            .enumerate()
            .filter(|(i, _)| indices.binary_search(&(*i as u32)).is_ok())
            .map(|(_, (start, g))| start..start + g.len())
            .collect()
    };
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if last.end == span.start => last.end = span.end,
            _ => merged.push(span),
        }
    }
    merged
}

/// Puts `id` first in the recently run commands.
pub fn remember(recent: &mut Vec<String>, id: &str) {
    recent.retain(|r| r != id);
    recent.insert(0, id.to_owned());
    recent.truncate(RECENT_LIMIT);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, title: &str) -> Candidate {
        Candidate {
            id: id.to_owned(),
            title: title.to_owned(),
            category: Category::View,
            shortcut: None,
        }
    }

    fn one(range: Range<usize>) -> Vec<Range<usize>> {
        vec![range]
    }

    fn sample() -> Vec<Candidate> {
        vec![
            candidate("view.toggle_mode", "Toggle Preview and Edit"),
            candidate("view.cycle_theme", "Cycle theme"),
            candidate("view.zoom_in", "Zoom in"),
            candidate("view.zoom_out", "Zoom out"),
            candidate("tabs.close", "Close tab"),
        ]
    }

    fn ids(candidates: &[Candidate], rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| candidates[r.index].id.clone())
            .collect()
    }

    #[test]
    fn an_empty_query_lists_recent_commands_first() {
        let c = sample();
        let none: Vec<String> = Vec::new();
        assert_eq!(
            ids(&c, &rows(&c, "", &none)),
            [
                "tabs.close",
                "view.cycle_theme",
                "view.toggle_mode",
                "view.zoom_in",
                "view.zoom_out"
            ]
        );
        let recent = vec!["view.zoom_out".to_owned(), "view.toggle_mode".to_owned()];
        let listed = rows(&c, "  ", &recent);
        assert_eq!(
            ids(&c, &listed),
            [
                "view.zoom_out",
                "view.toggle_mode",
                "tabs.close",
                "view.cycle_theme",
                "view.zoom_in"
            ]
        );
        assert!(listed.iter().all(|r| r.highlight == Highlight::None));
    }

    #[test]
    fn the_query_matches_titles_fuzzily_with_the_letters_marked() {
        let c = sample();
        let listed = rows(&c, "cyth", &[]);
        assert_eq!(ids(&c, &listed), ["view.cycle_theme"]);
        // "Cy" of Cycle, "th" of theme.
        assert_eq!(listed[0].highlight, Highlight::Title(vec![0..2, 6..8]));

        // Words match in any order; case does not matter in lower case.
        let listed = rows(&c, "tab clo", &[]);
        assert_eq!(ids(&c, &listed), ["tabs.close"]);
        assert_eq!(listed[0].highlight, Highlight::Title(vec![0..3, 6..9]));

        assert!(rows(&c, "xyz", &[]).is_empty());
    }

    #[test]
    fn ids_match_when_titles_do_not() {
        let c = sample();
        let listed = rows(&c, "view.cycle", &[]);
        assert_eq!(ids(&c, &listed), ["view.cycle_theme"]);
        assert_eq!(listed[0].highlight, Highlight::Id(one(0..10)));
        // `toggle_mode` is only in the id.
        let listed = rows(&c, "toggle_mode", &[]);
        assert_eq!(ids(&c, &listed)[0], "view.toggle_mode");
        assert!(matches!(listed[0].highlight, Highlight::Id(_)));
    }

    #[test]
    fn better_matches_rank_first_then_recent_ones() {
        let c = sample();
        let listed = rows(&c, "zoom", &[]);
        assert_eq!(ids(&c, &listed), ["view.zoom_in", "view.zoom_out"]);
        let recent = vec!["view.zoom_out".to_owned()];
        let listed = rows(&c, "zoom", &recent);
        assert_eq!(ids(&c, &listed), ["view.zoom_out", "view.zoom_in"]);
        // A word start beats letters scattered inside words.
        let listed = rows(&c, "clo", &[]);
        assert_eq!(ids(&c, &listed)[0], "tabs.close");
    }

    #[test]
    fn non_ascii_titles_are_marked_by_grapheme() {
        let c = vec![candidate("app.cafe", "Café menu")];
        let listed = rows(&c, "fé", &[]);
        assert_eq!(listed.len(), 1);
        // "f" at 2, "é" (two bytes) at 3.
        assert_eq!(listed[0].highlight, Highlight::Title(one(2..5)));
    }

    #[test]
    fn remembering_moves_a_command_to_the_front() {
        let mut recent = Vec::new();
        remember(&mut recent, "a.one");
        remember(&mut recent, "a.two");
        remember(&mut recent, "a.one");
        assert_eq!(recent, ["a.one", "a.two"]);
        for i in 0..RECENT_LIMIT + 5 {
            remember(&mut recent, &format!("a.n{i}"));
        }
        assert_eq!(recent.len(), RECENT_LIMIT);
    }
}
