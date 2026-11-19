// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The math macros of the notes that context rows come from (search
//! results, backlinks), so a formula in a row is typeset with the
//! `\newcommand`s and `\def`s its note makes before it, as the Reading
//! view renders it (MathJax's document-wide scope, see
//! [`NoteContext::macros_at`]).
//!
//! Cost: nothing is worked out for a row without a formula, and a note
//! without a macro command is not parsed. A note's macros are computed
//! once per content-index generation ([`note_macros`]) and shared by the
//! panels; they run on background threads.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use gv_math::MacroSet;
use gv_syntax::{ParseOptions, SyntaxTree};
use gv_text::TextSnapshot;
use gv_vault::content::{ContentSnapshot, FileSource};
use gv_vault::{ProfileId, VaultPath};

use crate::editor::live_context::{NoteContext, has_macro_command};

/// Notes kept in the cache; past it the entries of older generations go,
/// then all.
const CACHE_NOTES: usize = 256;

/// The macro formulas of one note.
#[derive(Debug)]
pub(crate) struct NoteMacros(NoteContext);

impl NoteMacros {
    /// The macros of `text` read with `profile`; `None` when no formula
    /// defines one.
    pub fn compute(text: &str, profile: ProfileId) -> Option<Self> {
        if !has_macro_command(text) {
            return None;
        }
        let snapshot = TextSnapshot::new(text);
        let tree = SyntaxTree::parse_with(&snapshot, ParseOptions::builtin(profile));
        let context = NoteContext::compute(&tree, &snapshot, false);
        context.has_macros().then_some(Self(context))
    }

    /// The macros a formula at `range` of the note is typeset with.
    pub fn at(&self, range: Range<usize>) -> MacroSet {
        self.0.macros_at(&range).cloned().unwrap_or_default()
    }
}

/// The note's root and path.
type Key = (Option<PathBuf>, VaultPath);

/// Each note's macros with the generation they were computed at.
type Cache = HashMap<Key, (u64, Option<Arc<NoteMacros>>)>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The macros of note `path` in `snapshot`, whose text `text` gives (only
/// called when they are not cached for the snapshot's generation).
pub(crate) fn note_macros(
    snapshot: &ContentSnapshot,
    path: &VaultPath,
    text: impl FnOnce() -> Option<String>,
) -> Option<Arc<NoteMacros>> {
    let generation = snapshot.generation();
    let key: Key = (snapshot.root().map(PathBuf::from), path.clone());
    if let Ok(cache) = cache().lock()
        && let Some((at, macros)) = cache.get(&key)
        && *at == generation
    {
        return macros.clone();
    }
    let macros = text()
        .and_then(|text| NoteMacros::compute(&text, snapshot.options().profile))
        .map(Arc::new);
    if let Ok(mut cache) = cache().lock() {
        if cache.len() >= CACHE_NOTES {
            cache.retain(|_, (at, _)| *at == generation);
            if cache.len() >= CACHE_NOTES {
                cache.clear();
            }
        }
        cache.insert(key, (generation, macros.clone()));
    }
    macros
}

/// The macros of the notes of `files` (search results) with a formula in
/// a context line, read through `source`.
pub(crate) fn for_results(
    snapshot: &ContentSnapshot,
    source: &dyn FileSource,
    files: &[gv_search::FileResult],
) -> Vec<(VaultPath, Arc<NoteMacros>)> {
    files
        .iter()
        .filter(|f| f.snippets.iter().any(|s| s.text.contains('$')))
        .filter_map(|f| {
            let macros = note_macros(snapshot, &f.path, || snapshot.note_text(&f.path, source))?;
            Some((f.path.clone(), macros))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_get_the_macros_defined_before_them() {
        let text = "$a$ then $\\newcommand{\\R}{\\mathbb{R}}$ and $\\R^2$\n";
        let m = NoteMacros::compute(text, ProfileId::Obsidian).expect("macros");
        let at = |f: &str| {
            let start = text.find(f).expect("formula");
            m.at(start..start + f.len())
        };
        assert!(at("$a$").is_empty());
        assert_eq!(at("$\\R^2$").get("\\R"), Some("\\mathbb{R}"));
        // No macro command: no parse, nothing.
        assert!(NoteMacros::compute("$x$ and $y$\n", ProfileId::Obsidian).is_none());
        // A command in code defines nothing.
        assert!(NoteMacros::compute("`\\def\\x{1}` $\\x$\n", ProfileId::Obsidian).is_none());
    }
}
