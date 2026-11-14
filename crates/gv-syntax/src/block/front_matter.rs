// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Front matter: a metadata block at byte 0 (after an optional BOM).
//!
//! - The first line is the opening delimiter: `---` (YAML) or `+++`
//!   (TOML), followed only by spaces or tabs, and a line ending.
//! - The block ends at the first later line that is a closing delimiter:
//!   `---` or `...` for YAML, `+++` for TOML, at column 0, followed only by
//!   spaces or tabs. The lines between are the content, possibly none.
//! - Without a closing delimiter there is no front matter: the first line
//!   is ordinary Markdown (a thematic break, for `---`). This follows
//!   Jekyll, whose rule GitHub uses; leading blank lines are not accepted.
//!
//! Whether line 0 opens front matter therefore depends on later lines. The
//! block pass decides it with a lookahead ([`detect`]) when it processes
//! line 0, and the front matter's mode is part of the checkpoints of the
//! lines inside it. The incremental reparse keeps that decision exact (see
//! [`crate::incremental`]): line 0 is never a convergence point, an edit
//! that removes the closing delimiter restarts at line 0 when no other
//! closing line follows, and an edit that creates a closing line under a
//! rejected opening line restarts at line 0 too.

use gv_text::TextSource;

use crate::options::FrontMatterOptions;
use crate::tree::event::FrontMatterKind;

/// Length of every delimiter.
pub(crate) const DELIMITER_LEN: usize = 3;

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// Whether `line` (terminator included) is `delim` followed only by spaces
/// and tabs and, when `need_ending`, a line ending.
fn delimiter_line(line: &[u8], delim: &[u8], need_ending: bool) -> bool {
    let Some(rest) = line.strip_prefix(delim) else {
        return false;
    };
    let ws = rest
        .iter()
        .take_while(|&&c| c == b' ' || c == b'\t')
        .count();
    match &rest[ws..] {
        b"\n" | b"\r\n" | b"\r" => true,
        b"" => !need_ending,
        _ => false,
    }
}

/// The kind of front matter `line` (line 0 without its BOM) would open.
pub(crate) fn opener(line: &[u8], opts: FrontMatterOptions) -> Option<FrontMatterKind> {
    if opts.yaml && delimiter_line(line, b"---", true) {
        Some(FrontMatterKind::Yaml)
    } else if opts.toml && delimiter_line(line, b"+++", true) {
        Some(FrontMatterKind::Toml)
    } else {
        None
    }
}

/// Whether `line` closes front matter of `kind`.
pub(crate) fn is_closer(line: &[u8], kind: FrontMatterKind) -> bool {
    match kind {
        FrontMatterKind::Yaml => {
            delimiter_line(line, b"---", false) || delimiter_line(line, b"...", false)
        }
        FrontMatterKind::Toml => delimiter_line(line, b"+++", false),
    }
}

/// The kind of front matter line 0 opens, if any (`None` when `opts`
/// enables none).
pub(crate) fn opener_at_start<S: TextSource + ?Sized>(
    text: &S,
    opts: FrontMatterOptions,
    scratch: &mut Vec<u8>,
) -> Option<FrontMatterKind> {
    if !opts.any() {
        return None;
    }
    let line = text.line(0, scratch);
    opener(line.strip_prefix(BOM).unwrap_or(line), opts)
}

/// Whether a line at or after `start` (a line start) closes front matter
/// of `kind`. O(bytes scanned).
pub(crate) fn closer_from<S: TextSource + ?Sized>(
    text: &S,
    mut start: usize,
    kind: FrontMatterKind,
    scratch: &mut Vec<u8>,
) -> bool {
    let n = text.len();
    while start < n {
        let line = text.line(start, scratch);
        if line.is_empty() {
            break;
        }
        if is_closer(line, kind) {
            return true;
        }
        start += line.len();
    }
    false
}

/// Whether the text starts with front matter: line 0 opens it and a later
/// line closes it. Returns its kind. O(front matter) when it exists,
/// O(document) when line 0 opens it but nothing closes it.
pub(crate) fn detect<S: TextSource + ?Sized>(
    text: &S,
    opts: FrontMatterOptions,
) -> Option<FrontMatterKind> {
    let mut scratch = Vec::new();
    let kind = opener_at_start(text, opts, &mut scratch)?;
    let first = text.line(0, &mut scratch).len();
    closer_from(text, first, kind, &mut scratch).then_some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: FrontMatterOptions = FrontMatterOptions {
        yaml: true,
        toml: true,
    };

    #[test]
    fn delimiters() {
        assert_eq!(opener(b"---\n", ALL), Some(FrontMatterKind::Yaml));
        assert_eq!(opener(b"--- \t\r\n", ALL), Some(FrontMatterKind::Yaml));
        assert_eq!(opener(b"+++\r", ALL), Some(FrontMatterKind::Toml));
        assert_eq!(opener(b"---", ALL), None);
        assert_eq!(opener(b"----\n", ALL), None);
        assert_eq!(opener(b" ---\n", ALL), None);
        assert_eq!(opener(b"...\n", ALL), None);
        assert_eq!(opener(b"--- x\n", ALL), None);
        let yaml_only = FrontMatterOptions {
            yaml: true,
            toml: false,
        };
        assert_eq!(opener(b"+++\n", yaml_only), None);
        assert!(is_closer(b"---", FrontMatterKind::Yaml));
        assert!(is_closer(b"...  \n", FrontMatterKind::Yaml));
        assert!(!is_closer(b"+++\n", FrontMatterKind::Yaml));
        assert!(is_closer(b"+++\n", FrontMatterKind::Toml));
        assert!(!is_closer(b"---\n", FrontMatterKind::Toml));
        assert!(!is_closer(b"  ---\n", FrontMatterKind::Yaml));
    }

    #[test]
    fn detection() {
        assert_eq!(
            detect("---\na: 1\n---\nx", ALL),
            Some(FrontMatterKind::Yaml)
        );
        assert_eq!(detect("---\n---", ALL), Some(FrontMatterKind::Yaml));
        assert_eq!(
            detect("\u{feff}---\na\n...\n", ALL),
            Some(FrontMatterKind::Yaml)
        );
        assert_eq!(
            detect("+++\na = 1\n+++\n", ALL),
            Some(FrontMatterKind::Toml)
        );
        assert_eq!(detect("---\na: 1\n", ALL), None);
        assert_eq!(detect("+++\na\n---\n", ALL), None);
        assert_eq!(detect("\n---\na\n---\n", ALL), None);
        assert_eq!(detect("---\na\n---\n", FrontMatterOptions::default()), None);
    }
}
