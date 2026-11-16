// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! String-in, string-out entry points for test harnesses, fuzz targets and
//! the `glyphvane render` command.
//!
//! These functions are the contract between the parser and the tooling that
//! checks it:
//!
//! | Function | Used by |
//! |---|---|
//! | [`render_commonmark_html`] | `cargo xtask spec --suite commonmark`, the comrak differential test, pathological suite |
//! | [`render_gfm_html`] | `cargo xtask spec --suite gfm` |
//! | [`parse_full`] | pathological suite, `parse_full` fuzz target |
//! | [`cst_text`] | `roundtrip_lossless` fuzz target |
//! | [`cst_dump`], [`cst_dump_after_edits`] (and their `_with` forms) | `incremental_equals_full` fuzz target, `render --format cst` |
//!
//! [`normalize_html`] is the spec-style HTML normalizer that conformance
//! and differential comparisons use.

use std::ops::Range;
use std::time::Instant;

use crate::html::{GfmHtmlOptions, to_commonmark_html, to_gfm_html};
use crate::incremental::{Budget, Change, Reparse};
use crate::inline::api::{InlineInput, parse_inlines};
use crate::options::ParseOptions;
use crate::tree::SyntaxTree;
use crate::tree::walk;

mod html4_entities;
mod normalize;

pub use normalize::normalize_html;

/// `true` while the functions in this module are placeholders rather than
/// calls into the real parser.
pub const IS_PLACEHOLDER: bool = false;

/// The cmark-gfm extension names [`render_gfm_html`] accepts, as listed on
/// the examples of the GFM spec.
pub const GFM_EXTENSIONS: &[&str] = &[
    "table",
    "strikethrough",
    "autolink",
    "tagfilter",
    "tasklist",
    "footnotes",
];

/// Parses `src` completely (block structure and every inline) and drops the
/// result. Used to time the parser and to check it does not panic.
pub fn parse_full(src: &str) {
    let tree = SyntaxTree::parse(src);
    let refs = tree.ref_map(src);
    walk::walk(
        &tree,
        &mut walk::LeafFn(|leaf: &walk::Leaf<'_>| {
            if matches!(
                leaf.kind,
                walk::LeafKind::Paragraph | walk::LeafKind::Heading { .. }
            ) {
                let segments = leaf.segments();
                std::hint::black_box(parse_inlines(
                    src,
                    InlineInput {
                        segments: &segments,
                    },
                    &*refs,
                ));
            }
        }),
    );
}

/// Renders `src` with the CommonMark profile through the CommonMark-exact
/// HTML emitter: the output must equal cmark's after [`normalize_html`].
pub fn render_commonmark_html(src: &str) -> String {
    to_commonmark_html(&SyntaxTree::parse(src), src)
}

/// Renders `src` like cmark-gfm through the GFM-exact HTML emitter.
///
/// `extensions` names the cmark-gfm extensions to enable (see
/// [`GFM_EXTENSIONS`]), as the GFM spec lists them per example and
/// cmark-gfm's spec runner passes them: `table`, `strikethrough`,
/// `autolink` and `tagfilter`. `tasklist` and `footnotes` are not
/// implemented yet, and unknown names are ignored. Front matter is off, as
/// in cmark-gfm.
pub fn render_gfm_html(src: &str, extensions: &[&str]) -> String {
    let has = |name: &str| extensions.contains(&name);
    let options = ParseOptions {
        tables: has("table"),
        strikethrough: has("strikethrough"),
        strikethrough_single: has("strikethrough"),
        autolinks: has("autolink"),
        ..ParseOptions::COMMONMARK
    };
    let html = GfmHtmlOptions {
        tagfilter: has("tagfilter"),
    };
    to_gfm_html(&SyntaxTree::parse_with(src, options), src, html)
}

/// The deterministic S-expression dump of the CST of `src` (the `cst` format
/// of `glyphvane render`, documented in [`crate::cst_dump`]). Two parses are
/// structurally equal iff their dumps are equal.
pub fn cst_dump(src: &str) -> String {
    cst_dump_with(src, ParseOptions::COMMONMARK)
}

/// [`cst_dump`] with the given parser options.
pub fn cst_dump_with(src: &str, options: ParseOptions) -> String {
    crate::cst_dump::cst_dump(&SyntaxTree::parse_with(src, options), src)
}

/// The concatenated text of every CST token of `src`, in order. The CST is
/// lossless iff this equals `src`.
pub fn cst_text(src: &str) -> String {
    let bytes = crate::cst_dump::cst_text(&SyntaxTree::parse(src), src);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// One replacement in a text: `range` (byte offsets into the text as it is
/// when the edit is applied) is replaced by `insert`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    /// Byte range to replace. Both ends must be char boundaries.
    pub range: Range<usize>,
    /// Replacement text.
    pub insert: String,
}

/// Applies `edits` to `src` one after the other; each edit's range refers to
/// the text produced by the edits before it.
///
/// # Panics
///
/// Panics if a range is out of bounds, reversed, or not on char boundaries.
pub fn apply_edits(src: &str, edits: &[TextEdit]) -> String {
    let mut text = src.to_owned();
    for edit in edits {
        text.replace_range(edit.range.clone(), &edit.insert);
    }
    text
}

/// Parses `src`, then applies `edits` one by one, reparsing incrementally
/// after each, and returns the [`cst_dump`] of the final tree. Incremental
/// parsing is correct iff this equals `cst_dump(&apply_edits(src, edits))`.
///
/// Each reparse runs with an already expired budget, so it pauses after
/// every slice and resumes (exercising the background continuation); every
/// interim tree is checked with [`SyntaxTree::validate`].
///
/// # Panics
///
/// Panics on invalid edits, like [`apply_edits`], and if an interim tree is
/// invalid.
pub fn cst_dump_after_edits(src: &str, edits: &[TextEdit]) -> String {
    cst_dump_after_edits_with(src, edits, ParseOptions::COMMONMARK)
}

/// [`cst_dump_after_edits`] with the given parser options; compare with
/// [`cst_dump_with`].
pub fn cst_dump_after_edits_with(src: &str, edits: &[TextEdit], options: ParseOptions) -> String {
    let mut text = src.to_owned();
    let mut tree = SyntaxTree::parse_with(text.as_str(), options);
    for edit in edits {
        let change = Change {
            start: edit.range.start,
            old_end: edit.range.end,
            new_len: edit.insert.len(),
        };
        text.replace_range(edit.range.clone(), &edit.insert);
        tree = match tree.reparse_change(text.as_str(), change, Budget::Until(Instant::now())) {
            Reparse::Done { tree, .. } => tree,
            Reparse::Partial { interim, mut task } => {
                if let Err(e) = interim.validate(text.as_str()) {
                    panic!("invalid interim tree: {e}");
                }
                while !task.run(text.as_str(), Budget::Until(Instant::now())) {}
                task.finish().0
            }
        };
    }
    crate::cst_dump::cst_dump(&tree, text.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_edits_is_sequential() {
        let edits = [
            TextEdit {
                range: 0..1,
                insert: "ab".to_owned(),
            },
            TextEdit {
                range: 2..2,
                insert: "c".to_owned(),
            },
        ];
        assert_eq!(apply_edits("xyz", &edits), "abcyz");
    }

    #[test]
    fn harness_contract_holds() {
        let src = "# Title\n\n- item\n";
        parse_full(src);
        assert_eq!(cst_text(src), src);
        let edits = [TextEdit {
            range: 2..2,
            insert: "x".to_owned(),
        }];
        assert_eq!(
            cst_dump_after_edits(src, &edits),
            cst_dump(&apply_edits(src, &edits))
        );
        let table = "| a |\n| - |\n| b |\n";
        let edits = [TextEdit {
            range: 8..9,
            insert: ":".to_owned(),
        }];
        assert_eq!(
            cst_dump_after_edits_with(table, &edits, ParseOptions::GFM),
            cst_dump_with(&apply_edits(table, &edits), ParseOptions::GFM)
        );
        assert!(cst_dump_with(table, ParseOptions::GFM).contains("(table-header)"));
    }
}
