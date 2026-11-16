// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Checking a document, block by block, with a cache.

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::ops::Range;
use std::sync::Arc;

use gv_syntax::{Leaf, LeafKind, SyntaxTree, Visitor, walk, walk_range};
use gv_text::TextSource;

use crate::check::Checker;
use crate::prose::leaf_prose;
use crate::tokenize::words;

/// A misspelled word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Misspelling {
    /// Its bytes in the document.
    pub range: Range<usize>,
    pub word: Arc<str>,
}

/// One block's misspellings, relative to the block's first content byte.
type BlockResult = Arc<[(Range<usize>, Arc<str>)]>;

/// Misspellings of blocks checked before, by the block's text, kind, parse
/// options and checker. A block that did not change (or moved) is not
/// checked again; entries not used by a whole-document check are dropped
/// at the next one.
#[derive(Debug, Default)]
pub struct BlockCache {
    current: HashMap<u64, BlockResult>,
    previous: HashMap<u64, BlockResult>,
}

impl BlockCache {
    fn get(&mut self, key: u64) -> Option<BlockResult> {
        if let Some(hit) = self.current.get(&key) {
            return Some(Arc::clone(hit));
        }
        let hit = self.previous.remove(&key)?;
        self.current.insert(key, Arc::clone(&hit));
        Some(hit)
    }

    fn insert(&mut self, key: u64, result: BlockResult) {
        self.current.insert(key, result);
    }

    /// After a whole-document check: what it did not use goes at the next.
    fn finish(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }

    /// Blocks remembered.
    pub fn len(&self) -> usize {
        self.current.len() + self.previous.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.current.clear();
        self.previous.clear();
    }
}

/// What a check did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckStats {
    /// Blocks that can hold prose.
    pub blocks: usize,
    /// Blocks checked word by word.
    pub checked: usize,
    /// Blocks whose result came from the cache.
    pub reused: usize,
    /// Words looked up (in the blocks checked).
    pub words: usize,
}

/// The misspellings of a document (or of the blocks touching a range), in
/// order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentCheck {
    pub misspellings: Vec<Misspelling>,
    pub stats: CheckStats,
}

fn kind_code(kind: LeafKind) -> Option<u8> {
    match kind {
        LeafKind::Paragraph => Some(1),
        LeafKind::Heading { .. } => Some(2),
        LeafKind::Table => Some(3),
        LeafKind::FrontMatter { .. } => Some(4),
        LeafKind::ThematicBreak
        | LeafKind::FencedCode
        | LeafKind::IndentedCode
        | LeafKind::Html { .. }
        | LeafKind::LinkRefDef
        | LeafKind::MathBlock => None,
    }
}

struct Pass<'a, S: TextSource + ?Sized> {
    tree: &'a SyntaxTree,
    text: &'a S,
    checker: &'a Checker,
    cache: &'a mut BlockCache,
    /// Hash of the checker and the parse options.
    salt: u64,
    out: DocumentCheck,
    buf: Vec<u8>,
}

impl<S: TextSource + ?Sized> Pass<'_, S> {
    fn block(&mut self, leaf: &Leaf<'_>) -> BlockResult {
        let mut found: Vec<(Range<usize>, Arc<str>)> = Vec::new();
        let base = block_base(leaf);
        for run in leaf_prose(self.tree, self.text, leaf) {
            self.buf.clear();
            self.text.copy_range(run.clone(), &mut self.buf);
            let s = String::from_utf8_lossy(&self.buf);
            for word in words(&s) {
                self.out.stats.words += 1;
                let w = &s[word.clone()];
                if self.checker.is_misspelled(w) {
                    let range = run.start + word.start - base..run.start + word.end - base;
                    found.push((range, w.into()));
                }
            }
        }
        found.into()
    }
}

/// The first content byte of a leaf, which its cached results are
/// relative to.
fn block_base(leaf: &Leaf<'_>) -> usize {
    leaf.lines
        .first()
        .map_or(leaf.range.start, |l| l.content.start)
}

impl<S: TextSource + ?Sized> Visitor for Pass<'_, S> {
    fn leaf(&mut self, leaf: &Leaf<'_>) {
        let Some(code) = kind_code(leaf.kind) else {
            return;
        };
        self.out.stats.blocks += 1;
        let base = block_base(leaf);
        let end = leaf.lines.last().map_or(leaf.range.end, |l| l.content.end);
        self.buf.clear();
        self.text.copy_range(base..end.max(base), &mut self.buf);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.salt.hash(&mut hasher);
        code.hash(&mut hasher);
        self.buf.hash(&mut hasher);
        let key = hasher.finish();
        let result = match self.cache.get(key) {
            Some(hit) => {
                self.out.stats.reused += 1;
                hit
            }
            None => {
                self.out.stats.checked += 1;
                let result = self.block(leaf);
                self.cache.insert(key, Arc::clone(&result));
                result
            }
        };
        self.out
            .misspellings
            .extend(result.iter().map(|(range, word)| Misspelling {
                range: base + range.start..base + range.end,
                word: Arc::clone(word),
            }));
    }
}

/// The misspellings of the document `tree` was parsed from (`text`), or
/// of the blocks touching `range`. Blocks found in `cache` are not checked
/// again; a whole-document check also drops the entries it did not use.
pub fn check_document<S: TextSource + ?Sized>(
    tree: &SyntaxTree,
    text: &S,
    checker: &Checker,
    cache: &mut BlockCache,
    range: Option<Range<usize>>,
) -> DocumentCheck {
    if !checker.is_active() {
        return DocumentCheck::default();
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    checker.fingerprint().hash(&mut hasher);
    tree.options().hash(&mut hasher);
    let mut pass = Pass {
        tree,
        text,
        checker,
        cache,
        salt: hasher.finish(),
        out: DocumentCheck::default(),
        buf: Vec::new(),
    };
    let whole = range.is_none();
    match range {
        Some(range) => walk_range(tree, range, &mut pass),
        None => walk(tree, &mut pass),
    }
    let Pass { out, cache, .. } = pass;
    if whole {
        cache.finish();
    }
    out
}
