// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Reading view's folds as the session keeps them (FR-VIEW-08,
//! FR-VAULT-14): each callout or `<details>` the reader opened or closed,
//! named by what it is, the first line of its rendered source (the
//! callout's title text, the `<details>` block's first line) and how many
//! with the same line come before it. Block ids change when the text is read again; these names survive
//! edits elsewhere in the note, and a fold whose block is gone or renamed
//! is dropped.

use std::collections::HashMap;
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
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

type Key = (FoldKind, String, usize);

/// The foldable blocks of `tree`, by name.
fn names(tree: &RenderTree, text: &str) -> Vec<(BlockId, Key)> {
    let mut seen: HashMap<(FoldKind, String), usize> = HashMap::new();
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
            Some((node.id, (kind, line, nth)))
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
        let by_name: HashMap<Key, BlockId> = names(&doc.tree, &doc.text)
            .into_iter()
            .map(|(id, key)| (key, id))
            .collect();
        for fold in folds {
            if let Some(&block) = by_name.get(&(fold.kind, fold.line, fold.nth)) {
                self.cache.folds.insert(block, fold.open);
            }
        }
    }
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
        names(&doc.tree, &doc.text)
            .into_iter()
            .filter_map(|(id, (kind, line, nth))| {
                let open = *st.cache.folds.get(&id)?;
                Some(ReadingFold {
                    kind,
                    line,
                    nth,
                    open,
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
        };
        let json = serde_json::to_string(&fold).expect("json");
        assert_eq!(
            json,
            r#"{"kind":"callout","line":"> [!note]- Later","open":true}"#
        );
        let back: ReadingFold = serde_json::from_str(&json).expect("back");
        assert_eq!(back, fold);
    }
}
