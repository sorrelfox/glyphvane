// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `cargo xtask fuzz-seed`: writes the seed corpora of the fuzz targets in
//! `fuzz/` from the spec examples.

use std::path::Path;

use anyhow::{Result, bail};

use crate::fixtures;

/// Separates the document from the edit script in `incremental_equals_full`
/// inputs (0xFF never occurs in UTF-8). Must match `fuzz/src/lib.rs`.
const EDIT_SEPARATOR: u8 = 0xFF;

/// Edits applied to every seed: (position as a fraction of the length in
/// 1/8ths, bytes deleted, text inserted). Positions are snapped to char
/// boundaries by the target.
const SEED_EDITS: &[(u16, u8, &str)] = &[
    (4, 0, "x"),
    (2, 0, "\n"),
    (0, 0, "> "),
    (6, 1, ""),
    (8, 0, "```"),
    (3, 0, "- "),
];

/// Encodes `markdown` plus the seed edits for `incremental_equals_full`:
/// `doc 0xFF (pos_lo pos_hi delete insert_len insert…)*`.
fn incremental_seed(markdown: &str) -> Vec<u8> {
    let mut out = markdown.as_bytes().to_vec();
    out.push(EDIT_SEPARATOR);
    let len = markdown.len();
    for &(eighths, delete, insert) in SEED_EDITS {
        let pos = u16::try_from(len * usize::from(eighths) / 8).unwrap_or(u16::MAX);
        out.extend_from_slice(&pos.to_le_bytes());
        out.push(delete);
        out.push(u8::try_from(insert.len()).unwrap_or(0));
        out.extend_from_slice(insert.as_bytes());
    }
    out
}

pub(crate) fn run(root: &Path, args: &[String]) -> Result<bool> {
    if !args.is_empty() {
        bail!("`fuzz-seed` takes no arguments");
    }
    let corpus = root.join("fuzz").join("corpus");
    let mut count = 0;
    for (prefix, file) in [
        ("cm", fixtures::COMMONMARK_JSON),
        ("gfm", fixtures::GFM_JSON),
    ] {
        for ex in fixtures::load(root, file)? {
            let name = format!("{prefix}-{:03}", ex.example);
            for target in ["parse_full", "roundtrip_lossless"] {
                let dir = corpus.join(target);
                std::fs::create_dir_all(&dir)?;
                std::fs::write(dir.join(format!("{name}.md")), &ex.markdown)?;
            }
            let dir = corpus.join("incremental_equals_full");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join(format!("{name}.bin")),
                incremental_seed(&ex.markdown),
            )?;
            count += 1;
        }
    }
    eprintln!(
        "fuzz-seed: wrote {count} seeds per target under {}",
        corpus.display()
    );
    Ok(true)
}
