// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Obsidian heading links. Obsidian has no slugs: `[[Note#Heading]]` and
//! `[t](Note.md#Heading%20Text)` name the heading text, which is matched
//! after a normalization (Obsidian's internal `stripHeading`): characters
//! that cannot appear in a link are replaced, whitespace is collapsed, and
//! the comparison ignores case.
//!
//! ORACLE-PENDING: the exact `stripHeading` rules are unverified. This
//! normalization replaces `#`, `|`, `^`, `:`, `%`, `[`, `]`, `*` and `` ` ``
//! with spaces, so a link written from the rendered text (without emphasis
//! or code markers) and one written from the source both match. It
//! compares against the heading's source text, as Obsidian's metadata cache
//! holds it.

/// The characters replaced by spaces before comparing.
const REPLACED: &[char] = &['#', '|', '^', ':', '%', '[', ']', '*', '`'];

/// The comparison key of a heading text or a link subpath: replaced
/// characters become spaces, whitespace runs collapse to one space and
/// are trimmed, and the result is lowercased.
pub fn heading_key(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() || REPLACED.contains(&c) {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.extend(c.to_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::heading_key;

    #[test]
    fn keys() {
        assert_eq!(heading_key("  Part   1 "), "part 1");
        assert_eq!(heading_key("A: b | c"), "a b c");
        assert_eq!(heading_key("Some **bold** `code`"), "some bold code");
        assert_eq!(heading_key("Heading [[link]]"), "heading link");
        assert_eq!(heading_key("ÉTÉ"), "été");
        assert_eq!(heading_key("#"), "");
    }
}
