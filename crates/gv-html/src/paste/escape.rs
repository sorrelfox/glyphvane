// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Escaping of literal text for Markdown.
//!
//! The goal is minimal escaping that still parses back to the same text:
//! a character is escaped only where it could start or end a construct of
//! the target profile. Context outside the string is unknown, so the first
//! and last characters are treated as if a construct could touch them.

use crate::profile::Profile;

/// Where inline content is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// A paragraph: hard breaks are `\` + newline.
    Para,
    /// An ATX heading: one line.
    Heading,
    /// A GFM table cell: one line, `|` escaped.
    Cell,
}

fn is_ws(c: Option<char>) -> bool {
    c.is_some_and(char::is_whitespace)
}

fn is_alnum(c: Option<char>) -> bool {
    c.is_some_and(char::is_alphanumeric)
}

/// `true` if `rest` (the text after an `&`) would complete a character
/// reference: `#123;`, `#x1F;` or `name;`. Text pieces of an inline run
/// are always separated by a delimiter, so the end of the string ends any
/// reference.
pub(crate) fn entity_like(rest: &str) -> bool {
    if rest.is_empty() {
        return false;
    }
    let b = rest.as_bytes();
    let (digits, max): (&[u8], usize) = if b[0] == b'#' {
        if b.get(1).is_some_and(|c| matches!(c, b'x' | b'X')) {
            (&b[2..], 6)
        } else {
            (&b[1..], 7)
        }
    } else {
        (b, 32)
    };
    let hexish = b[0] == b'#';
    let n = digits
        .iter()
        .take_while(|c| {
            if hexish {
                c.is_ascii_hexdigit()
            } else {
                c.is_ascii_alphanumeric()
            }
        })
        .count();
    n > 0 && n <= max && digits.get(n) == Some(&b';')
}

/// Appends `s` with Markdown-significant characters escaped.
pub(crate) fn escape_text(s: &str, profile: Profile, mode: Mode, out: &mut String) {
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        let doubled =
            |d: char| prev == Some(d) || next == Some(d) || prev.is_none() || next.is_none();
        let escape = match c {
            '\\' => next.is_none_or(|n| n.is_ascii_punctuation()),
            '`' | '[' | ']' => true,
            '*' => !(is_ws(prev) && is_ws(next)),
            '_' => !((is_ws(prev) && is_ws(next)) || (is_alnum(prev) && is_alnum(next))),
            '<' => next.is_none_or(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!' | '?')),
            '&' => {
                let rest: String = chars[i + 1..].iter().take(40).collect();
                entity_like(&rest)
            }
            '~' => profile.tilde_delimiter() && !(is_ws(prev) && is_ws(next)),
            '=' => profile.highlight() && doubled('='),
            '+' => profile.plus_delimiter() && doubled('+'),
            '^' => {
                profile.sub_sup() || (profile.obsidian_syntax() && (prev.is_none() || is_ws(prev)))
            }
            '%' => profile.obsidian_syntax() && doubled('%'),
            '$' => profile.dollar_math(),
            '|' => mode == Mode::Cell,
            _ => false,
        };
        if escape {
            out.push('\\');
        }
        out.push(c);
    }
}

/// Escapes what would make a rendered line start a block: ATX headings,
/// quotes, list markers, thematic breaks and setext underlines, fences,
/// table delimiter rows, fenced divs, front matter.
pub(crate) fn escape_line_start(line: &str, profile: Profile) -> String {
    let b = line.as_bytes();
    let Some(&first) = b.first() else {
        return String::new();
    };
    let after = |n: usize| b.get(n).is_none_or(|c| *c == b' ' || *c == b'\t');
    let only = |set: &[u8]| b.iter().all(|c| set.contains(c));
    let prefix = match first {
        b'#' => {
            let n = b.iter().take_while(|&&c| c == b'#').count();
            n <= 6 && after(n)
        }
        b'>' => true,
        b'-' | b'+' => after(1) || only(b"- \t") || line.starts_with("+++"),
        b'=' => only(b"= \t"),
        b'~' => line.starts_with("~~~"),
        b':' => profile.fenced_divs() && line.starts_with(":::"),
        _ => false,
    };
    let delimiter_row = profile.tables() && only(b"|:- \t") && b.contains(&b'-');
    if prefix || (delimiter_row && matches!(first, b'|' | b':' | b'-')) {
        return format!("\\{line}");
    }
    // An ordered list marker: up to nine digits, then `.` or `)`.
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits) && matches!(b.get(digits), Some(b'.' | b')')) && after(digits + 1)
    {
        return format!("{}\\{}", &line[..digits], &line[digits..]);
    }
    line.to_owned()
}

/// A code span holding `s` (white space already collapsed).
pub(crate) fn code_span(s: &str, mode: Mode, out: &mut String) {
    let content = if mode == Mode::Cell {
        s.replace('|', "\\|")
    } else {
        s.to_owned()
    };
    let mut longest = 0;
    let mut run = 0;
    for c in content.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    let pad = content.starts_with('`')
        || content.ends_with('`')
        || (content.starts_with(' ') && content.ends_with(' ') && content.trim() != "");
    out.push_str(&fence);
    if pad {
        out.push(' ');
    }
    out.push_str(&content);
    if pad {
        out.push(' ');
    }
    out.push_str(&fence);
}

/// A link or image destination, in `<…>` form when the plain form cannot
/// hold it.
pub(crate) fn destination(url: &str, mode: Mode) -> String {
    let url = if mode == Mode::Cell {
        url.replace('|', "%7C")
    } else {
        url.to_owned()
    };
    let mut depth = 0i32;
    let mut balanced = true;
    for c in url.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    balanced = false;
                }
            }
            _ => {}
        }
    }
    let angle = url.is_empty()
        || !balanced
        || depth != 0
        || url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '<' || c == '>');
    let mut out = String::with_capacity(url.len() + 2);
    if angle {
        out.push('<');
    }
    for (i, c) in url.char_indices() {
        match c {
            '\\' => out.push_str("\\\\"),
            '<' | '>' if angle => {
                out.push('\\');
                out.push(c);
            }
            '&' if entity_like(&url[i + 1..]) => out.push_str("\\&"),
            c => out.push(c),
        }
    }
    if angle {
        out.push('>');
    }
    out
}

/// A link title in double quotes.
pub(crate) fn title(t: &str, mode: Mode) -> String {
    let mut out = String::with_capacity(t.len() + 2);
    out.push('"');
    for (i, c) in t.char_indices() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '&' if entity_like(&t[i + 1..]) => out.push_str("\\&"),
            '|' if mode == Mode::Cell => out.push_str("\\|"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Escapes text for HTML content and attribute values.
pub(crate) fn html_escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(s: &str, p: Profile) -> String {
        let mut out = String::new();
        escape_text(s, p, Mode::Para, &mut out);
        out
    }

    #[test]
    fn minimal_text_escaping() {
        let cm = Profile::CommonMark;
        assert_eq!(esc("snake_case_name", cm), "snake_case_name");
        assert_eq!(esc("_x_", cm), "\\_x\\_");
        assert_eq!(esc("a * b", cm), "a * b");
        assert_eq!(esc("2*3", cm), "2\\*3");
        assert_eq!(esc("[x](y)", cm), "\\[x\\](y)");
        assert_eq!(esc("a < b", cm), "a < b");
        assert_eq!(esc("<div>", cm), "\\<div>");
        assert_eq!(esc("AT&T", cm), "AT&T");
        assert_eq!(esc("&amp; &#38; &#x26;", cm), "\\&amp; \\&#38; \\&#x26;");
        assert_eq!(esc("a\\b \\* \\", cm), "a\\b \\\\\\* \\\\");
        assert_eq!(esc("`x`", cm), "\\`x\\`");
        assert_eq!(esc("~~x~~ $5 ==y==", cm), "~~x~~ $5 ==y==");
        let gfm = Profile::GfmFile;
        assert_eq!(esc("~~x~~ $5", gfm), "\\~\\~x\\~\\~ \\$5");
        assert_eq!(esc("a ~ b", gfm), "a ~ b");
        let obs = Profile::Obsidian;
        assert_eq!(esc("a==b a=b %%c", obs), "a\\=\\=b a=b \\%\\%c");
        assert_eq!(esc("x ^id", obs), "x \\^id");
        let ext = Profile::Extended;
        assert_eq!(esc("2^10 a++ b+c", ext), "2\\^10 a\\+\\+ b+c");
    }

    #[test]
    fn line_starts() {
        let cm = Profile::CommonMark;
        for (line, want) in [
            ("# x", "\\# x"),
            ("#hashtag", "#hashtag"),
            ("####### x", "####### x"),
            ("> q", "\\> q"),
            ("- x", "\\- x"),
            ("-x", "-x"),
            ("---", "\\---"),
            ("+ x", "\\+ x"),
            ("===", "\\==="),
            ("1. x", "1\\. x"),
            ("2) x", "2\\) x"),
            ("1.5 x", "1.5 x"),
            ("1234567890. x", "1234567890. x"),
            ("~~~", "\\~~~"),
            ("plain", "plain"),
        ] {
            assert_eq!(escape_line_start(line, cm), want, "{line:?}");
        }
        assert_eq!(escape_line_start("|---|", Profile::GfmFile), "\\|---|");
        assert_eq!(escape_line_start(":::", Profile::Extended), "\\:::");
        assert_eq!(escape_line_start(":::", Profile::CommonMark), ":::");
    }

    #[test]
    fn code_spans() {
        let span = |s: &str| {
            let mut o = String::new();
            code_span(s, Mode::Para, &mut o);
            o
        };
        assert_eq!(span("x"), "`x`");
        assert_eq!(span("a`b"), "``a`b``");
        assert_eq!(span("`a"), "`` `a ``");
        assert_eq!(span(" a "), "`  a  `");
        let mut o = String::new();
        code_span("a|b", Mode::Cell, &mut o);
        assert_eq!(o, "`a\\|b`");
    }

    #[test]
    fn destinations_and_titles() {
        assert_eq!(
            destination("https://x.com/a_(b)", Mode::Para),
            "https://x.com/a_(b)"
        );
        assert_eq!(destination("a b.png", Mode::Para), "<a b.png>");
        assert_eq!(destination("x)", Mode::Para), "<x)>");
        assert_eq!(destination("?a=1&b=2", Mode::Para), "?a=1&b=2");
        assert_eq!(destination("?a=1&copy;", Mode::Para), "?a=1\\&copy;");
        assert_eq!(destination("a|b", Mode::Cell), "a%7Cb");
        assert_eq!(title("say \"hi\"", Mode::Para), "\"say \\\"hi\\\"\"");
    }
}
