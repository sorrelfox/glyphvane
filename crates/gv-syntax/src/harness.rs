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
//! | [`cst_dump`], [`cst_dump_after_edits`] | `incremental_equals_full` fuzz target, `render --format cst` |
//!
//! # Placeholders
//!
//! **The parser does not exist yet.** Every function below has its final
//! signature and a trivial body: the renderers return [`PLACEHOLDER_HTML`]
//! (an HTML comment that matches no spec example, so the stub scores 0), the
//! CST dump is empty, and [`cst_text`] echoes its input. [`IS_PLACEHOLDER`] is
//! `true` while that is the case, so harnesses can report "not implemented"
//! instead of failing. Replace the bodies (and set the constant to `false`)
//! as the parser lands; the harnesses need no changes.
//!
//! [`normalize_html`] is not a placeholder: it is the spec-style HTML
//! normalizer that conformance and differential comparisons use.

use std::ops::Range;

mod html4_entities;
mod normalize;

pub use normalize::normalize_html;

/// `true` while the functions in this module are placeholders rather than
/// calls into the real parser.
pub const IS_PLACEHOLDER: bool = true;

/// What the placeholder renderers return. No spec example expects it, so
/// examples whose expected output is empty do not pass by accident.
pub const PLACEHOLDER_HTML: &str = "<!-- gv-syntax: HTML emitter not implemented -->\n";

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
///
/// Placeholder: does nothing.
pub fn parse_full(src: &str) {
    let _ = src;
}

/// Renders `src` with the CommonMark profile through the CommonMark-exact
/// HTML emitter: the output must equal cmark's after [`normalize_html`].
///
/// Placeholder: returns [`PLACEHOLDER_HTML`].
pub fn render_commonmark_html(src: &str) -> String {
    let _ = src;
    PLACEHOLDER_HTML.to_owned()
}

/// Renders `src` with the GFM profile through the GFM-exact HTML emitter.
///
/// `extensions` names the cmark-gfm extensions to enable (see
/// [`GFM_EXTENSIONS`]); the GFM spec lists them per example. Unknown names are
/// ignored.
///
/// Placeholder: returns [`PLACEHOLDER_HTML`].
pub fn render_gfm_html(src: &str, extensions: &[&str]) -> String {
    let _ = (src, extensions);
    PLACEHOLDER_HTML.to_owned()
}

/// The deterministic S-expression dump of the CST of `src` (the `cst` format
/// of `glyphvane render`). Two parses are structurally equal iff their dumps
/// are equal.
///
/// Placeholder: returns an empty string.
pub fn cst_dump(src: &str) -> String {
    let _ = src;
    String::new()
}

/// The concatenated text of every CST token of `src`, in order. The CST is
/// lossless iff this equals `src`.
///
/// Placeholder: returns `src` unchanged.
pub fn cst_text(src: &str) -> String {
    src.to_owned()
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
/// Placeholder: returns the full-parse dump of the edited text.
///
/// # Panics
///
/// Panics on invalid edits, like [`apply_edits`].
pub fn cst_dump_after_edits(src: &str, edits: &[TextEdit]) -> String {
    cst_dump(&apply_edits(src, edits))
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
    fn placeholder_contract_holds() {
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
    }
}
