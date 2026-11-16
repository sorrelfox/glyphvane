// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Instruction-count benchmarks (Callgrind through gungraun), the CI gate
//! for the parser. Run them with `cargo xtask bench-icount` (Linux, needs
//! Valgrind and the matching `gungraun-runner`).
//!
//! Each case runs once under Callgrind; only the benchmark function is
//! counted, not the setup that generates the document and parses it.
//!
//! - `full_parse::block`, `full`, `html`: the block pass alone, block pass
//!   plus every inline, and parse plus CommonMark HTML (as in the `parse`
//!   wall-clock bench).
//! - `reparse::keystroke` (`insert_*`, `delete_*`): one character typed at the
//!   end of a text line in the middle of the document, and deleted again.
//! - `reparse::fence_at_top`: a fence opened at the top (as in the `parse`
//!   bench), so the reparse runs to the next closing fence: the end of the
//!   outliner, the first spec example in the spec document.
//!
//! Documents: the GFM spec repeated to 1 MB and the generated 1 MB outliner,
//! the same inputs as the `parse` bench (see `common`).

#[cfg(target_os = "linux")]
mod common;

#[cfg(target_os = "linux")]
mod benches {
    use std::hint::black_box;

    use gungraun::{library_benchmark, library_benchmark_group};
    use gv_syntax::{Change, SyntaxTree};

    use crate::common::{full_inlines, outliner, repeat_to, spec_text, text_line_end};

    /// The benchmark documents.
    #[derive(Clone, Copy, Debug)]
    pub enum Doc {
        /// The GFM spec repeated to 1 MB: prose, lists, code.
        Spec1Mb,
        /// One huge nested tight list of 1 MB.
        Outliner1Mb,
    }

    pub fn text(doc: Doc) -> String {
        match doc {
            Doc::Spec1Mb => repeat_to(&spec_text(), 1 << 20),
            Doc::Outliner1Mb => outliner(1 << 20),
        }
    }

    /// One edit to replay against a parsed document.
    #[derive(Debug)]
    pub struct Edit {
        tree: SyntaxTree,
        /// The text after the edit.
        new_text: String,
        change: Change,
    }

    /// Types `x` at the end of a text line in the middle of the document.
    pub fn insert_edit(doc: Doc) -> Edit {
        let text = text(doc);
        let at = text_line_end(&text, text.len() / 2);
        Edit {
            tree: SyntaxTree::parse(text.as_str()),
            new_text: format!("{}x{}", &text[..at], &text[at..]),
            change: Change {
                start: at,
                old_end: at,
                new_len: 1,
            },
        }
    }

    /// Deletes the `x` that [`insert_edit`] typed (a backspace).
    pub fn delete_edit(doc: Doc) -> Edit {
        let text = text(doc);
        let at = text_line_end(&text, text.len() / 2);
        let with = format!("{}x{}", &text[..at], &text[at..]);
        Edit {
            tree: SyntaxTree::parse(with.as_str()),
            new_text: text,
            change: Change {
                start: at,
                old_end: at + 1,
                new_len: 0,
            },
        }
    }

    /// Opens a fence at the top: everything below becomes code.
    pub fn fence_edit(doc: Doc) -> Edit {
        let text = text(doc);
        Edit {
            tree: SyntaxTree::parse(text.as_str()),
            new_text: format!("```\n{text}"),
            change: Change {
                start: 0,
                old_end: 0,
                new_len: 4,
            },
        }
    }

    // The inputs are returned so that freeing them is not counted.

    #[library_benchmark]
    #[bench::spec_1mb(args = (Doc::Spec1Mb), setup = text)]
    #[bench::outliner_1mb(args = (Doc::Outliner1Mb), setup = text)]
    fn block(text: String) -> (String, SyntaxTree) {
        let tree = SyntaxTree::parse(black_box(text.as_str()));
        (text, black_box(tree))
    }

    #[library_benchmark]
    #[bench::spec_1mb(args = (Doc::Spec1Mb), setup = text)]
    #[bench::outliner_1mb(args = (Doc::Outliner1Mb), setup = text)]
    fn full(text: String) -> (String, SyntaxTree) {
        let tree = SyntaxTree::parse(black_box(text.as_str()));
        full_inlines(&tree, &text);
        (text, black_box(tree))
    }

    #[library_benchmark]
    #[bench::spec_1mb(args = (Doc::Spec1Mb), setup = text)]
    #[bench::outliner_1mb(args = (Doc::Outliner1Mb), setup = text)]
    fn html(text: String) -> (String, String) {
        let html = gv_syntax::harness::render_commonmark_html(black_box(text.as_str()));
        (text, black_box(html))
    }

    fn reparse(edit: Edit) -> (Edit, SyntaxTree) {
        let (tree, stats) = edit
            .tree
            .reparse_full(black_box(edit.new_text.as_str()), black_box(edit.change));
        black_box(stats);
        (edit, black_box(tree))
    }

    #[library_benchmark]
    #[bench::insert_spec_1mb(args = (Doc::Spec1Mb), setup = insert_edit)]
    #[bench::insert_outliner_1mb(args = (Doc::Outliner1Mb), setup = insert_edit)]
    #[bench::delete_spec_1mb(args = (Doc::Spec1Mb), setup = delete_edit)]
    #[bench::delete_outliner_1mb(args = (Doc::Outliner1Mb), setup = delete_edit)]
    fn keystroke(edit: Edit) -> (Edit, SyntaxTree) {
        reparse(edit)
    }

    #[library_benchmark]
    #[bench::spec_1mb(args = (Doc::Spec1Mb), setup = fence_edit)]
    #[bench::outliner_1mb(args = (Doc::Outliner1Mb), setup = fence_edit)]
    fn fence_at_top(edit: Edit) -> (Edit, SyntaxTree) {
        reparse(edit)
    }

    library_benchmark_group!(name = full_parse, benchmarks = [block, full, html]);
    library_benchmark_group!(name = reparse, benchmarks = [keystroke, fence_at_top]);
}

#[cfg(target_os = "linux")]
use benches::{full_parse, reparse};

#[cfg(target_os = "linux")]
gungraun::main!(library_benchmark_groups = full_parse, reparse);

/// Valgrind-based benchmarks run on Linux only.
#[cfg(not(target_os = "linux"))]
fn main() {}
