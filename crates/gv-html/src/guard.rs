// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Cheap checks that run before html5ever sees the input.
//!
//! html5ever has two quadratic paths: its scope checks are linear in the
//! depth of the open-element stack (10,000 nested `div`s cost about
//! 170 ms), and its duplicate-attribute check is quadratic in the number of
//! attributes on one tag. The scanner below estimates the nesting depth
//! with a simplified open-element stack and counts attributes per tag, in
//! one linear pass. When a limit is exceeded the input is not parsed;
//! [`plain_text`] extracts its text with the same scanner instead.

use std::ops::Range;

/// Deepest estimated element nesting that is still parsed.
pub(crate) const MAX_DEPTH: usize = 512;
/// Most attributes on one tag.
pub(crate) const MAX_ATTRIBUTES: usize = 1000;
/// Longest single tag, in bytes.
pub(crate) const MAX_TAG_BYTES: usize = 64 * 1024;
/// Largest input that is parsed, in bytes.
pub(crate) const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// Why the input was not parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardTrip {
    /// Elements are nested deeper than the limit.
    TooDeep,
    /// A tag has too many attributes.
    TooManyAttributes,
    /// A single tag is too long.
    TagTooLong,
    /// The whole input is too large.
    TooLarge,
}

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr", "param", "keygen", "basefont", "bgsound", "frame",
];

/// Elements whose content is raw text up to the matching end tag.
const RAW_TEXT: &[&str] = &[
    "script", "style", "textarea", "title", "xmp", "iframe", "noembed", "noframes",
];

/// Elements that start a block (and so close an open `p`).
pub(crate) const BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "center",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "html",
    "li",
    "listing",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
    "caption",
    "legend",
    "plaintext",
    "xmp",
];

/// How far down the stack the implicit-close and end-tag searches look.
const SEARCH_WINDOW: usize = 64;

/// One scanner event.
enum Event {
    Text(Range<usize>),
    Start { name: String, self_closing: bool },
    End(String),
}

struct Scanner<'a> {
    src: &'a str,
    pos: usize,
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b':' | b'_' | b'.')
}

impl<'a> Scanner<'a> {
    fn new(src: &'a str) -> Self {
        Scanner { src, pos: 0 }
    }

    fn find_from(&self, from: usize, needle: &str) -> Option<usize> {
        self.src.get(from..)?.find(needle).map(|i| from + i)
    }

    /// Skips raw text up to `</name` (case-insensitive); returns the text.
    fn skip_raw_text(&mut self, name: &str) -> Range<usize> {
        let start = self.pos;
        let bytes = self.src.as_bytes();
        let mut i = start;
        while let Some(lt) = self.find_from(i, "</") {
            let after = lt + 2;
            let end = after + name.len();
            if end <= bytes.len() && bytes[after..end].eq_ignore_ascii_case(name.as_bytes()) {
                self.pos = lt;
                return start..lt;
            }
            i = after;
        }
        self.pos = bytes.len();
        start..bytes.len()
    }

    /// The next event, or a guard trip. `None` at the end.
    fn next(&mut self) -> Option<Result<Event, GuardTrip>> {
        let bytes = self.src.as_bytes();
        // Comments and declarations are skipped in a loop (no recursion).
        let lt = loop {
            if self.pos >= bytes.len() {
                return None;
            }
            let start = self.pos;
            let Some(lt) = self.find_from(start, "<") else {
                self.pos = bytes.len();
                return Some(Ok(Event::Text(start..bytes.len())));
            };
            if lt > start {
                self.pos = lt;
                return Some(Ok(Event::Text(start..lt)));
            }
            let rest = &bytes[lt..];
            if rest.starts_with(b"<!--") {
                self.pos = self.find_from(lt + 4, "-->").map_or(bytes.len(), |i| i + 3);
            } else if rest.starts_with(b"<!") || rest.starts_with(b"<?") {
                self.pos = self.find_from(lt + 2, ">").map_or(bytes.len(), |i| i + 1);
            } else {
                break lt;
            }
        };
        let (closing, name_start) = if bytes[lt..].starts_with(b"</") {
            (true, lt + 2)
        } else {
            (false, lt + 1)
        };
        if !bytes.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            // A lone `<`: text.
            self.pos = lt + 1;
            return Some(Ok(Event::Text(lt..lt + 1)));
        }
        let mut i = name_start;
        while i < bytes.len() && is_name_byte(bytes[i]) {
            i += 1;
        }
        let name = self.src[name_start..i].to_ascii_lowercase();
        // Attributes, up to the closing `>`.
        let mut attrs = 0usize;
        let mut self_closing = false;
        loop {
            if i - lt > MAX_TAG_BYTES {
                return Some(Err(GuardTrip::TagTooLong));
            }
            match bytes.get(i) {
                None => break,
                Some(b'>') => {
                    i += 1;
                    break;
                }
                Some(b'/') => {
                    self_closing = bytes.get(i + 1) == Some(&b'>');
                    i += 1;
                }
                Some(b) if b.is_ascii_whitespace() => i += 1,
                Some(_) => {
                    attrs += 1;
                    if attrs > MAX_ATTRIBUTES {
                        return Some(Err(GuardTrip::TooManyAttributes));
                    }
                    // The attribute name (an `=` can start one).
                    i += 1;
                    while i < bytes.len()
                        && !bytes[i].is_ascii_whitespace()
                        && !matches!(bytes[i], b'=' | b'>' | b'/')
                    {
                        i += 1;
                    }
                    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    if bytes.get(i) == Some(&b'=') {
                        i += 1;
                        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                            i += 1;
                        }
                        match bytes.get(i) {
                            Some(&q @ (b'"' | b'\'')) => {
                                let close = bytes[i + 1..]
                                    .iter()
                                    .position(|&c| c == q)
                                    .map_or(bytes.len(), |p| i + 1 + p + 1);
                                i = close;
                            }
                            _ => {
                                while i < bytes.len()
                                    && !bytes[i].is_ascii_whitespace()
                                    && bytes[i] != b'>'
                                {
                                    i += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        if i - lt > MAX_TAG_BYTES {
            return Some(Err(GuardTrip::TagTooLong));
        }
        self.pos = i.min(bytes.len());
        Some(Ok(if closing {
            Event::End(name)
        } else {
            Event::Start { name, self_closing }
        }))
    }
}

/// The simplified open-element stack.
#[derive(Default)]
struct Stack {
    names: Vec<String>,
}

impl Stack {
    /// Truncates to below the nearest entry matching `pred` within the
    /// search window, unless a `stop` element comes first.
    fn close_nearest(&mut self, pred: impl Fn(&str) -> bool, stop: &[&str]) {
        let len = self.names.len();
        for i in (len.saturating_sub(SEARCH_WINDOW)..len).rev() {
            let n = self.names[i].as_str();
            if pred(n) {
                self.names.truncate(i);
                return;
            }
            if stop.contains(&n) {
                return;
            }
        }
    }

    fn start(&mut self, name: &str) {
        match name {
            "li" => self.close_nearest(|n| n == "li", &["ul", "ol", "menu", "table"]),
            "dt" | "dd" => self.close_nearest(|n| n == "dt" || n == "dd", &["dl", "table"]),
            "td" | "th" => self.close_nearest(|n| n == "td" || n == "th", &["tr", "table"]),
            "tr" => self.close_nearest(|n| n == "tr", &["table"]),
            "tbody" | "thead" | "tfoot" => {
                self.close_nearest(|n| matches!(n, "tbody" | "thead" | "tfoot"), &["table"])
            }
            "option" | "optgroup" => self.close_nearest(|n| n == "option", &["select"]),
            _ => {}
        }
        if BLOCK.contains(&name) {
            self.close_nearest(
                |n| n == "p",
                &["div", "td", "th", "li", "blockquote", "table"],
            );
        }
        self.names.push(name.to_owned());
    }

    fn end(&mut self, name: &str) {
        self.close_nearest(|n| n == name, &[]);
    }
}

/// Checks `html` against the limits in one linear pass.
pub(crate) fn check(html: &str) -> Result<(), GuardTrip> {
    if html.len() > MAX_INPUT_BYTES {
        return Err(GuardTrip::TooLarge);
    }
    let mut sc = Scanner::new(html);
    let mut stack = Stack::default();
    while let Some(ev) = sc.next() {
        match ev? {
            Event::Text(_) => {}
            Event::Start { name, self_closing } => {
                if VOID.contains(&name.as_str()) || self_closing {
                    continue;
                }
                if RAW_TEXT.contains(&name.as_str()) {
                    sc.skip_raw_text(&name);
                    continue;
                }
                if name == "plaintext" {
                    break;
                }
                stack.start(&name);
                if stack.names.len() > MAX_DEPTH {
                    return Err(GuardTrip::TooDeep);
                }
            }
            Event::End(name) => stack.end(&name),
        }
    }
    Ok(())
}

/// Decodes the most common character references (the plain-text fallback
/// does not run the HTML tokenizer).
fn decode_entities(s: &str, out: &mut String) {
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let semi = rest[..rest.len().min(12)].find(';');
        let decoded = semi.and_then(|semi| {
            let body = &rest[1..semi];
            let c = match body {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => {
                    let num = body.strip_prefix('#')?;
                    let v = match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => num.parse().ok()?,
                    };
                    char::from_u32(v).filter(|c| *c != '\0')
                }
            }?;
            Some((c, semi + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
}

/// The text of `html` without parsing it: tags removed, the content of
/// `script`, `style` and the other raw-text elements dropped, block tags
/// turned into line breaks. Used when [`check`] refuses the input.
pub(crate) fn plain_text(html: &str) -> String {
    let mut out = String::new();
    let mut sc = Scanner::new(html);
    loop {
        let ev = match sc.next() {
            None => break,
            Some(Ok(ev)) => ev,
            // An oversized tag: skip past it.
            Some(Err(_)) => {
                let Some(gt) = sc.find_from(sc.pos + 1, ">") else {
                    break;
                };
                sc.pos = gt + 1;
                continue;
            }
        };
        match ev {
            Event::Text(r) => decode_entities(&html[r], &mut out),
            Event::Start { name, .. } => {
                if RAW_TEXT.contains(&name.as_str()) {
                    sc.skip_raw_text(&name);
                } else if name == "br" || BLOCK.contains(&name.as_str()) {
                    out.push('\n');
                }
            }
            Event::End(name) => {
                if BLOCK.contains(&name.as_str()) {
                    out.push('\n');
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_html_passes() {
        assert_eq!(check("<p>a<p>b<ul><li>x<li>y</ul>"), Ok(()));
        let rows = "<tr><td>a<td>b".repeat(2000);
        assert_eq!(check(&format!("<table>{rows}</table>")), Ok(()));
        let paras = "<p>x".repeat(5000);
        assert_eq!(check(&paras), Ok(()));
        let lis = "<li><p>x".repeat(5000);
        assert_eq!(check(&format!("<ul>{lis}</ul>")), Ok(()));
        // Raw text is skipped, comments too.
        let script = format!("<script>{}</script>", "<div>".repeat(5000));
        assert_eq!(check(&script), Ok(()));
        let comment = format!("<!--{}-->", "<div>".repeat(5000));
        assert_eq!(check(&comment), Ok(()));
    }

    #[test]
    fn limits_trip() {
        assert_eq!(check(&"<div>".repeat(600)), Err(GuardTrip::TooDeep));
        assert_eq!(check(&"<b><i>".repeat(300)), Err(GuardTrip::TooDeep));
        let attrs: String = (0..1500).map(|i| format!(" a{i}=x")).collect();
        assert_eq!(
            check(&format!("<p{attrs}>t</p>")),
            Err(GuardTrip::TooManyAttributes)
        );
        let long = format!("<p title=\"{}\">t</p>", "x".repeat(70_000));
        assert_eq!(check(&long), Err(GuardTrip::TagTooLong));
        // Balanced deep nesting within the limit is fine.
        let ok = format!("{}x{}", "<div>".repeat(500), "</div>".repeat(500));
        assert_eq!(check(&ok), Ok(()));
    }

    #[test]
    fn plain_text_fallback() {
        let t = plain_text("<p>a &amp; b</p><script>evil()</script><div>c&#x41;&bogus;</div>");
        assert_eq!(
            t.split_whitespace().collect::<Vec<_>>(),
            ["a", "&", "b", "cA&bogus;"]
        );
    }
}
