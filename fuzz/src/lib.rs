// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Fuzz target bodies, shared by the libFuzzer entry points in
//! `fuzz_targets/` and the stable standalone runner ([`standalone`]).
//!
//! The targets call the string-level entry points of `gv_syntax::harness`.
//! While those are placeholders the invariants hold trivially; the targets
//! need no change when the parser lands.

pub mod standalone;

use gv_syntax::harness::{self, TextEdit};

/// Separates the document from the edit script in
/// [`incremental_equals_full`] inputs; 0xFF never occurs in UTF-8.
/// `cargo xtask fuzz-seed` writes seeds in this format.
pub const EDIT_SEPARATOR: u8 = 0xFF;

/// At most this many edits are decoded from one input.
const MAX_EDITS: usize = 64;

/// Full parse and every renderer: must not panic.
pub fn parse_full(data: &[u8]) {
    let src = String::from_utf8_lossy(data);
    harness::parse_full(&src);
    let _ = harness::render_commonmark_html(&src);
    let _ = harness::render_gfm_html(&src, harness::GFM_EXTENSIONS);
    let _ = harness::cst_dump(&src);
}

/// The CST is lossless: the concatenated token texts equal the input.
pub fn roundtrip_lossless(data: &[u8]) {
    let src = String::from_utf8_lossy(data);
    let text = harness::cst_text(&src);
    assert!(text == src, "CST text differs from the input");
}

/// Incremental reparsing after an edit script gives the same tree as a full
/// parse of the edited text.
pub fn incremental_equals_full(data: &[u8]) {
    let (doc, edits) = decode_edits(data);
    let incremental = harness::cst_dump_after_edits(&doc, &edits);
    let full = harness::cst_dump(&harness::apply_edits(&doc, &edits));
    assert!(
        incremental == full,
        "incremental != full\ndoc: {doc:?}\nedits: {edits:?}\nincremental:\n{incremental}\nfull:\n{full}"
    );
}

/// Decodes `doc 0xFF (pos_lo pos_hi delete insert_len insert…)*`.
///
/// The document and inserted texts are decoded lossily as UTF-8. Each
/// position is taken modulo the current text length plus one and moved back
/// to a char boundary; the deleted range is clamped to the text and extended
/// to a char boundary, so every decoded edit is valid. Without a separator
/// the whole input is the document and there are no edits.
pub fn decode_edits(data: &[u8]) -> (String, Vec<TextEdit>) {
    let Some(sep) = data.iter().position(|&b| b == EDIT_SEPARATOR) else {
        return (String::from_utf8_lossy(data).into_owned(), Vec::new());
    };
    let doc = String::from_utf8_lossy(&data[..sep]).into_owned();
    let mut script = &data[sep + 1..];
    let mut text = doc.clone();
    let mut edits = Vec::new();
    while script.len() >= 4 && edits.len() < MAX_EDITS {
        let pos = usize::from(u16::from_le_bytes([script[0], script[1]]));
        let delete = usize::from(script[2]);
        let insert_len = usize::from(script[3]).min(script.len() - 4);
        let insert = String::from_utf8_lossy(&script[4..4 + insert_len]).into_owned();
        script = &script[4 + insert_len..];

        let mut start = pos % (text.len() + 1);
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (start + delete).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        let edit = TextEdit {
            range: start..end,
            insert,
        };
        text.replace_range(edit.range.clone(), &edit.insert);
        edits.push(edit);
    }
    (doc, edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_valid_edits() {
        let mut data = "é\n".as_bytes().to_vec();
        data.push(EDIT_SEPARATOR);
        data.extend_from_slice(&[1, 0, 1, 2, b'>', b' ']); // inside `é`: snapped to 0
        data.extend_from_slice(&[0xff, 0xff, 9, 0]); // delete past the end
        let (doc, edits) = decode_edits(&data);
        assert_eq!(doc, "é\n");
        assert_eq!(edits[0].range, 0..2);
        assert_eq!(harness::apply_edits(&doc, &edits[..1]), "> \n");
        assert_eq!(edits.len(), 2);
        let _ = harness::apply_edits(&doc, &edits);
    }

    #[test]
    fn targets_accept_odd_input() {
        for data in [
            &b""[..],
            b"\xff",
            b"\xff\xff\xff\xff\xff",
            b"# a\n\xff\x00",
            b"\xc3",
        ] {
            parse_full(data);
            roundtrip_lossless(data);
            incremental_equals_full(data);
        }
    }
}
