// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The `href` of `<a>` tags in raw HTML (HTML blocks and inline HTML), for
//! the content index's [`NoteData::html_links`](super::NoteData::html_links).
//!
//! The tags are read with CommonMark's grammar of an open tag (a name,
//! then attributes: a name, optionally `=` and an unquoted, `'`- or
//! `"`-quoted value), leniently: a tag that does not parse ends the scan
//! of that tag only. HTML comments and the contents of `<script>`,
//! `<style>` and `<textarea>` are skipped. Only local, relative
//! destinations are kept (no scheme such as `https:` or `mailto:`, no
//! network path `//host`).

use std::ops::Range;

use crate::url::has_scheme;

/// One `href` value: its byte range in the scanned text (without quotes)
/// and the value with character references decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Href {
    pub range: Range<usize>,
    pub value: String,
}

/// The local `href`s of the `<a>` tags in `html`, in order. Ranges are
/// offset by `base`.
pub(crate) fn a_hrefs(html: &str, base: usize) -> Vec<Href> {
    let b = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = html[i..].find('<') {
        let at = i + rel;
        let rest = &b[at..];
        if rest.starts_with(b"<!--") {
            i = html[at + 4..]
                .find("-->")
                .map_or(b.len(), |e| at + 4 + e + 3);
            continue;
        }
        let Some((name, mut j)) = tag_name(b, at + 1) else {
            i = at + 1;
            continue;
        };
        if ["script", "style", "textarea"]
            .iter()
            .any(|raw| name.eq_ignore_ascii_case(raw.as_bytes()))
        {
            let close = format!("</{}", String::from_utf8_lossy(name));
            i = find_ignore_case(html, at + 1, &close).unwrap_or(b.len());
            continue;
        }
        let is_a = name.eq_ignore_ascii_case(b"a");
        let mut href: Option<Range<usize>> = None;
        // Attributes, up to `>` or `/>`.
        loop {
            let ws = skip_ws(b, j);
            if ws >= b.len() || b[ws] == b'>' || b[ws..].starts_with(b"/>") {
                j = ws;
                break;
            }
            if ws == j {
                // An attribute needs white space before it.
                break;
            }
            let Some(name_end) = attr_name(b, ws) else {
                break;
            };
            let attr = &b[ws..name_end];
            j = name_end;
            let eq = skip_ws(b, j);
            if b.get(eq) != Some(&b'=') {
                continue;
            }
            let v = skip_ws(b, eq + 1);
            let value = match b.get(v) {
                Some(&q @ (b'"' | b'\'')) => {
                    let Some(end) = b[v + 1..].iter().position(|&c| c == q) else {
                        break;
                    };
                    j = v + 1 + end + 1;
                    v + 1..v + 1 + end
                }
                Some(_) => {
                    let end = b[v..]
                        .iter()
                        .position(|c| {
                            c.is_ascii_whitespace()
                                || matches!(c, b'"' | b'\'' | b'=' | b'<' | b'>' | b'`')
                        })
                        .map_or(b.len(), |e| v + e);
                    if end == v {
                        break;
                    }
                    j = end;
                    v..end
                }
                None => break,
            };
            // The first `href` counts, as in HTML.
            if is_a && href.is_none() && attr.eq_ignore_ascii_case(b"href") {
                href = Some(value);
            }
        }
        if let Some(range) = href
            && let Some(value) = local_value(&html[range.clone()])
        {
            out.push(Href {
                range: base + range.start..base + range.end,
                value,
            });
        }
        i = j.max(at + 1);
    }
    out
}

/// The value of a local, relative `href` with its character references
/// decoded; `None` for anything else (a scheme, a network path, an empty
/// value, white space around it).
fn local_value(written: &str) -> Option<String> {
    if written.is_empty() || written.trim() != written || written.starts_with("//") {
        return None;
    }
    let value = if written.contains('&') && !written.contains('\\') {
        gv_syntax::inline::unescape_string(written)
    } else {
        written.to_owned()
    };
    (!has_scheme(&value) && !value.starts_with("//")).then_some(value)
}

/// A tag name at `at` (`[A-Za-z][A-Za-z0-9-]*`) and the offset after it.
fn tag_name(b: &[u8], at: usize) -> Option<(&[u8], usize)> {
    if !b.get(at)?.is_ascii_alphabetic() {
        return None;
    }
    let end = b[at..]
        .iter()
        .position(|c| !(c.is_ascii_alphanumeric() || *c == b'-'))
        .map_or(b.len(), |e| at + e);
    // A name ends at white space, `>` or `/`.
    match b.get(end) {
        None => Some((&b[at..end], end)),
        Some(c) if c.is_ascii_whitespace() || *c == b'>' || *c == b'/' => Some((&b[at..end], end)),
        Some(_) => None,
    }
}

/// The end of an attribute name at `at` (`[A-Za-z_:][A-Za-z0-9_.:-]*`).
fn attr_name(b: &[u8], at: usize) -> Option<usize> {
    let first = *b.get(at)?;
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b':') {
        return None;
    }
    Some(
        b[at + 1..]
            .iter()
            .position(|c| !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-')))
            .map_or(b.len(), |e| at + 1 + e),
    )
}

fn skip_ws(b: &[u8], mut at: usize) -> usize {
    while b.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// The offset of `needle` (ASCII) in `text` from `from`, ignoring ASCII
/// case.
fn find_ignore_case(text: &str, from: usize, needle: &str) -> Option<usize> {
    let n = needle.as_bytes();
    text.as_bytes()
        .get(from..)?
        .windows(n.len())
        .position(|w| w.eq_ignore_ascii_case(n))
        .map(|p| from + p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(html: &str) -> Vec<(String, &str)> {
        a_hrefs(html, 0)
            .into_iter()
            .map(|h| {
                let written = &html[h.range];
                (h.value, written)
            })
            .collect()
    }

    #[test]
    fn local_hrefs_of_a_tags() {
        let html = r##"<a href="Note.md#Head">x</a> <A class=x HREF='sub/My%20Note.md'>y</A>
<a href=Other.md>z</a> <a title="t" href = "#Local">w</a>"##;
        assert_eq!(
            values(html),
            [
                ("Note.md#Head".to_owned(), "Note.md#Head"),
                ("sub/My%20Note.md".to_owned(), "sub/My%20Note.md"),
                ("Other.md".to_owned(), "Other.md"),
                ("#Local".to_owned(), "#Local"),
            ]
        );
    }

    #[test]
    fn schemes_network_paths_and_other_tags_are_left_out() {
        let html = r#"<a href="https://e.com/x.md">a</a> <a href="mailto:a@b.c">b</a>
<a href="//host/x.md">c</a> <img href="img.md" src="pic.png"> <abbr href="no.md">
<a name="n">d</a> <a href="">e</a>"#;
        assert!(values(html).is_empty(), "{:?}", values(html));
    }

    #[test]
    fn comments_and_scripts_are_skipped() {
        let html = r#"<!-- <a href="c.md"> --><script>"<a href='s.md'>"</script>
<a href="yes.md">ok</a>"#;
        assert_eq!(values(html), [("yes.md".to_owned(), "yes.md")]);
    }

    #[test]
    fn an_unclosed_quote_ends_only_its_own_tag() {
        let html = r#"<a href="ok.md">a</a> <b title="open <a href='y.md'>y</a>"#;
        assert_eq!(
            values(html),
            [("ok.md".to_owned(), "ok.md"), ("y.md".to_owned(), "y.md")]
        );
        // Searched to the end once per quote character at most: no later
        // tag has that quote to search for.
        let html = format!("<a href=\"{}", "<a href=y.md> ".repeat(100_000));
        assert_eq!(values(&html).len(), 100_000);
    }

    #[test]
    fn character_references_are_decoded_and_ranges_offset() {
        let html = r#"<a href="Q&amp;A.md">q</a>"#;
        let found = a_hrefs(html, 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].value, "Q&A.md");
        assert_eq!(found[0].range, 19..29);
    }
}
