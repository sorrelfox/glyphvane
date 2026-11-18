// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Reading view's folds as the session keeps them (FR-VIEW-08,
//! FR-VAULT-14): each callout or `<details>` the reader opened or closed,
//! named by what it is, the first line of its rendered source (the
//! callout's title text, the `<details>` block's first line) and how many
//! with the same line come before it. Block ids change when the text is
//! read again; these names survive edits elsewhere in the note.
//!
//! A fold also keeps its place among the blocks of its kind and how many
//! there were. When its name is gone (the callout's title was renamed),
//! it goes to the block at the same place, but only when that is
//! unambiguous: as many blocks of the kind as when it was saved, and the
//! block there neither matched by name nor named by another saved fold.
//! Otherwise (blocks added and removed, or no place saved by an older
//! version) the fold is dropped. One case still guesses wrong: a block
//! deleted and another of the same kind added between the two sessions,
//! with the renamed one in between; then a callout opens or closes
//! differently, nothing else.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::Context;
use gv_render_tree::{NodeKind, RenderTree};
use gv_syntax::BlockId;
use serde::{Deserialize, Serialize};

use super::Reader;
use super::state::State;

/// Longest source line kept in a fold's name.
const LINE_MAX: usize = 200;

/// What kind of block a fold is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FoldKind {
    Callout,
    Details,
}

/// A fold of the Reading view, as the session keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingFold {
    pub kind: FoldKind,
    /// The first line of the block's rendered source, trimmed.
    pub line: String,
    /// How many blocks of the same kind and line come before it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub nth: usize,
    pub open: bool,
    /// Its place among the blocks of its kind and how many there were:
    /// `[place, count]`. Absent in sessions of older versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<[usize; 2]>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

type Key = (FoldKind, String, usize);

/// A foldable block: its id, name, and place among the blocks of its kind.
struct Named {
    id: BlockId,
    key: Key,
    place: usize,
}

/// How many blocks of each kind `named` has.
fn counts(named: &[Named]) -> HashMap<FoldKind, usize> {
    let mut counts = HashMap::new();
    for n in named {
        *counts.entry(n.key.0).or_default() += 1;
    }
    counts
}

/// The foldable blocks of `tree`, by name.
fn names(tree: &RenderTree, text: &str) -> Vec<Named> {
    let mut seen: HashMap<(FoldKind, String), usize> = HashMap::new();
    let mut places: HashMap<FoldKind, usize> = HashMap::new();
    tree.nodes()
        .iter()
        .filter_map(|node| {
            let kind = match node.kind {
                NodeKind::Details { .. } => FoldKind::Details,
                NodeKind::CalloutTitle { .. } => FoldKind::Callout,
                _ => return None,
            };
            let source = text.get(node.source.clone()).unwrap_or_default();
            let line: String = source
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .chars()
                .take(LINE_MAX)
                .collect();
            let count = seen.entry((kind, line.clone())).or_default();
            let nth = *count;
            *count += 1;
            let place = places.entry(kind).or_default();
            let at = *place;
            *place += 1;
            Some(Named {
                id: node.id,
                key: (kind, line, nth),
                place: at,
            })
        })
        .collect()
}

impl State {
    /// Applies the folds waiting for the document just installed, when it
    /// is their file.
    pub(super) fn apply_pending_folds(&mut self) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        if self
            .pending_folds
            .as_ref()
            .is_none_or(|(path, _)| *path != doc.path)
        {
            return;
        }
        let Some((_, folds)) = self.pending_folds.take() else {
            return;
        };
        let named = names(&doc.tree, &doc.text);
        for (block, open) in match_folds(&named, folds) {
            self.cache.folds.insert(block, open);
        }
    }
}

/// The blocks of `named` that `folds` apply to, and whether each is open:
/// by name, else (a renamed block) by an unambiguous place.
fn match_folds(named: &[Named], folds: Vec<ReadingFold>) -> Vec<(BlockId, bool)> {
    let by_name: HashMap<&Key, BlockId> = named.iter().map(|n| (&n.key, n.id)).collect();
    let saved: HashSet<Key> = folds
        .iter()
        .map(|f| (f.kind, f.line.clone(), f.nth))
        .collect();
    let mut out = Vec::new();
    let mut used = HashSet::new();
    let mut unnamed = Vec::new();
    for fold in folds {
        match by_name.get(&(fold.kind, fold.line.clone(), fold.nth)) {
            Some(&block) => {
                used.insert(block);
                out.push((block, fold.open));
            }
            None => unnamed.push(fold),
        }
    }
    if unnamed.is_empty() {
        return out;
    }
    let counts = counts(named);
    for fold in unnamed {
        let Some([place, count]) = fold.at else {
            continue;
        };
        if counts.get(&fold.kind) != Some(&count) {
            continue;
        }
        let Some(block) = named
            .iter()
            .find(|n| n.key.0 == fold.kind && n.place == place)
        else {
            continue;
        };
        if used.contains(&block.id) || saved.contains(&block.key) {
            continue;
        }
        used.insert(block.id);
        out.push((block.id, fold.open));
    }
    out
}

impl Reader {
    /// The callouts and `<details>` opened or closed in the document shown,
    /// for the session.
    pub fn reading_folds(&self) -> Vec<ReadingFold> {
        let st = self.state.borrow();
        let Some(doc) = st.doc.as_ref() else {
            return Vec::new();
        };
        if st.cache.folds.is_empty() {
            return Vec::new();
        }
        let named = names(&doc.tree, &doc.text);
        let counts = counts(&named);
        named
            .into_iter()
            .filter_map(|n| {
                let open = *st.cache.folds.get(&n.id)?;
                let (kind, line, nth) = n.key;
                Some(ReadingFold {
                    kind,
                    line,
                    nth,
                    open,
                    at: Some([n.place, counts.get(&kind).copied().unwrap_or_default()]),
                })
            })
            .collect()
    }

    /// Folds `folds` of the file at `path` once it is shown: now, when it
    /// is shown and no load runs, else once the load finishes.
    pub fn restore_reading_folds(
        &mut self,
        path: &Path,
        folds: Vec<ReadingFold>,
        cx: &mut Context<'_, Self>,
    ) {
        if folds.is_empty() {
            return;
        }
        let ready = self.status == super::LoadStatus::Ready;
        let mut st = self.state.borrow_mut();
        st.pending_folds = Some((PathBuf::from(path), folds));
        let shown = ready && st.doc.as_ref().is_some_and(|d| d.path == path);
        if shown {
            // Laid out again with the folds, at the same place.
            let anchor = st.engine.anchor();
            st.apply_pending_folds();
            st.engine = super::engine::Engine::new(st.layout.lead(), super::state::OVERDRAW);
            if let Some(anchor) = anchor
                && let Some(ix) = st.doc.as_ref().and_then(|d| d.tree.index_of(anchor.id))
            {
                st.engine.jump_to(ix, anchor.offset);
            }
            drop(st);
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_serialize_compactly() {
        let fold = ReadingFold {
            kind: FoldKind::Callout,
            line: "> [!note]- Later".into(),
            nth: 0,
            open: true,
            at: Some([1, 3]),
        };
        let json = serde_json::to_string(&fold).expect("json");
        assert_eq!(
            json,
            r#"{"kind":"callout","line":"> [!note]- Later","open":true,"at":[1,3]}"#
        );
        let back: ReadingFold = serde_json::from_str(&json).expect("back");
        assert_eq!(back, fold);
        // Sessions of older versions have no place.
        let old: ReadingFold =
            serde_json::from_str(r#"{"kind":"callout","line":"X","open":true}"#).expect("old");
        assert_eq!(old.at, None);
    }

    fn named(lines: &[&str]) -> Vec<Named> {
        let mut seen: HashMap<String, usize> = HashMap::new();
        lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let nth = seen.entry((*line).to_owned()).or_default();
                let key = (FoldKind::Callout, (*line).to_owned(), *nth);
                *nth += 1;
                Named {
                    id: BlockId(i as u64 + 1),
                    key,
                    place: i,
                }
            })
            .collect()
    }

    fn fold(line: &str, at: Option<[usize; 2]>) -> ReadingFold {
        ReadingFold {
            kind: FoldKind::Callout,
            line: line.into(),
            nth: 0,
            open: true,
            at,
        }
    }

    #[test]
    fn a_renamed_callout_keeps_its_fold_by_place() {
        let now = named(&["A", "Renamed", "C"]);
        let got = match_folds(&now, vec![fold("B", Some([1, 3]))]);
        assert_eq!(got, vec![(BlockId(2), true)]);
    }

    #[test]
    fn a_place_is_used_only_when_unambiguous() {
        // Blocks were added or removed: the count differs.
        let now = named(&["A", "Renamed", "C", "D"]);
        assert!(match_folds(&now, vec![fold("B", Some([1, 3]))]).is_empty());
        // The block there is another saved fold's.
        let now = named(&["A", "C", "X"]);
        let got = match_folds(&now, vec![fold("B", Some([1, 3])), fold("C", Some([2, 3]))]);
        assert_eq!(got, vec![(BlockId(2), true)]);
        // The block there matched by name.
        let now = named(&["A", "B2", "C"]);
        let got = match_folds(
            &now,
            vec![fold("B2", Some([0, 3])), fold("Old", Some([1, 3]))],
        );
        assert_eq!(got, vec![(BlockId(2), true)]);
        // An older session without places: no guess.
        let now = named(&["A", "Renamed", "C"]);
        assert!(match_folds(&now, vec![fold("B", None)]).is_empty());
    }
}
