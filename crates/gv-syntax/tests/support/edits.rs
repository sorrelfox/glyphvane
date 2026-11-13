// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Incremental-reparse checking: a chunked text source whose chunk
//! boundaries fall anywhere, and [`check_edit`], which compares an
//! incremental reparse with a full parse.

use std::collections::HashMap;
use std::ops::Range;
use std::time::Instant;

use gv_syntax::{Budget, Change, Changes, Reparse, SyntaxTree};
use gv_text::TextSource;

/// A toy rope: the text in chunks of a fixed size (cut at char
/// boundaries), so lines straddle chunks, including between `\r` and
/// `\n`.
#[derive(Clone, Debug)]
pub struct ChunkedText {
    chunks: Vec<String>,
    starts: Vec<usize>,
    len: usize,
}

impl ChunkedText {
    pub fn new(text: &str, max_chunk: usize) -> Self {
        let max_chunk = max_chunk.max(1);
        let mut chunks = Vec::new();
        let mut rest = text;
        while !rest.is_empty() {
            let mut cut = max_chunk.min(rest.len());
            while !rest.is_char_boundary(cut) {
                cut += 1;
            }
            chunks.push(rest[..cut].to_owned());
            rest = &rest[cut..];
        }
        let mut starts = Vec::with_capacity(chunks.len());
        let mut len = 0;
        for c in &chunks {
            starts.push(len);
            len += c.len();
        }
        ChunkedText {
            chunks,
            starts,
            len,
        }
    }

    fn chunk_of(&self, offset: usize) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    fn byte(&self, offset: usize) -> Option<u8> {
        if offset >= self.len {
            return None;
        }
        let c = self.chunk_of(offset);
        Some(self.chunks[c].as_bytes()[offset - self.starts[c]])
    }
}

impl TextSource for ChunkedText {
    fn len(&self) -> usize {
        self.len
    }

    fn line<'a>(&'a self, start: usize, scratch: &'a mut Vec<u8>) -> &'a [u8] {
        if start >= self.len {
            return &[];
        }
        scratch.clear();
        let mut pos = start;
        while let Some(b) = self.byte(pos) {
            scratch.push(b);
            pos += 1;
            if b == b'\n' {
                break;
            }
            if b == b'\r' {
                if self.byte(pos) == Some(b'\n') {
                    scratch.push(b'\n');
                }
                break;
            }
        }
        scratch
    }

    fn copy_range(&self, range: Range<usize>, out: &mut Vec<u8>) {
        for p in range.start..range.end.min(self.len) {
            if let Some(b) = self.byte(p) {
                out.push(b);
            }
        }
    }
}

/// Applies a replacement to a string.
pub fn apply(text: &str, start: usize, old_end: usize, insert: &str) -> (String, Change) {
    let mut s = String::with_capacity(text.len() + insert.len());
    s.push_str(&text[..start]);
    s.push_str(insert);
    s.push_str(&text[old_end..]);
    (
        s,
        Change {
            start,
            old_end,
            new_len: insert.len(),
        },
    )
}

/// How [`check_edit`] runs the reparse.
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    /// To completion, on a `str`.
    Direct,
    /// On a [`ChunkedText`] with this chunk size, paused after every slice
    /// (an expired deadline) and resumed; every interim tree is validated.
    Sliced(usize),
}

/// Reparses `tree` (of `old_text`) after `change` and checks it against a
/// full parse of `new_text`: same lines, checkpoints and events; lossless
/// and well formed; BlockIds unique, and every old block that lies wholly
/// in the unchanged prefix or suffix and still exists with the same kind,
/// depth and (shifted) extent keeps its id.
pub fn check_edit(
    tree: &SyntaxTree,
    old_text: &str,
    new_text: &str,
    change: Change,
    mode: Mode,
) -> Result<(SyntaxTree, Changes), String> {
    let ctx = |msg: String| {
        format!("{msg}\nold text: {old_text:?}\nnew text: {new_text:?}\nchange: {change:?}")
    };
    let (new, changes) = match mode {
        Mode::Direct => tree.reparse_full(new_text, change),
        Mode::Sliced(size) => {
            let t = ChunkedText::new(new_text, size);
            match tree.reparse_change(&t, change, Budget::Until(Instant::now())) {
                Reparse::Done { tree, changes } => (tree, changes),
                Reparse::Partial { interim, mut task } => {
                    interim
                        .validate(&t)
                        .map_err(|e| ctx(format!("invalid interim tree: {e}")))?;
                    while !task.run(&t, Budget::Until(Instant::now())) {}
                    task.finish()
                }
            }
        }
    };
    let full = SyntaxTree::parse(new_text);
    new.same_structure(&full)
        .map_err(|e| ctx(format!("incremental != full: {e}\nchanges: {changes:?}")))?;
    new.validate(new_text)
        .map_err(|e| ctx(format!("validation: {e}")))?;
    let delta = change.new_len as isize - (change.old_end - change.start) as isize;
    let new_blocks = new.blocks();
    let by_extent: HashMap<_, _> = new_blocks
        .iter()
        .map(|b| ((b.range.start, b.range.end, b.depth, b.kind), b.id))
        .collect();
    for b in tree.blocks() {
        let shifted = if b.range.end <= change.start {
            Some((b.range.start, b.range.end))
        } else if b.range.start >= change.old_end {
            Some((
                (b.range.start as isize + delta) as usize,
                (b.range.end as isize + delta) as usize,
            ))
        } else {
            None
        };
        if let Some((s, e)) = shifted
            && let Some(&id) = by_extent.get(&(s, e, b.depth, b.kind))
            && id != b.id
        {
            return Err(ctx(format!("block {b:?} lost its id (now {id:?})")));
        }
    }
    Ok((new, changes))
}

/// The char boundaries of `s` (including 0 and `len`).
pub fn boundaries(s: &str) -> Vec<usize> {
    (0..=s.len()).filter(|&i| s.is_char_boundary(i)).collect()
}
