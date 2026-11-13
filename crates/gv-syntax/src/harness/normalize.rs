// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Spec-style HTML normalization: a port of `test/normalize.py` from the
//! CommonMark spec repository (tag 0.31.2), which spec runners use to
//! compare HTML while ignoring insignificant differences.
//!
//! The Python version feeds the HTML through `html.parser.HTMLParser`; this
//! port re-implements the parts of that tokenizer the normalizer depends on.
//! The rules:
//!
//! - whitespace runs in text collapse to one space, except inside `<pre>`;
//! - whitespace next to block-level tags is removed;
//! - a newline right after `<br>` is removed;
//! - self-closing tags lose the slash (`<br />` becomes `<br>`);
//! - tag and attribute names are lowercased, attributes sorted by name, and
//!   attribute values re-escaped (`& < > " '`);
//! - character references in text are decoded, except that `<`, `>`, `&`
//!   and `"` are written as entities; unknown named references are kept;
//! - `<![CDATA[…]]>` on one line is copied verbatim; comments, declarations
//!   and processing instructions are kept.
//!
//! Known deviations from Python's tokenizer, none of which affect
//! spec-conformant output: attribute values decode numeric references and
//! the HTML 4 named references (with `;`), not every HTML5 name; an `&#`
//! that is not a valid character reference is plain text; lone surrogates in
//! numeric references are kept as written.
//!
//! The original code carries these notices, retained here as its licences
//! require:
//!
//! > Copyright (c) 2014, John MacFarlane. All rights reserved.
//! >
//! > Redistribution and use in source and binary forms, with or without
//! > modification, are permitted provided that the following conditions are
//! > met: (1) Redistributions of source code must retain the above copyright
//! > notice, this list of conditions and the following disclaimer.
//! > (2) Redistributions in binary form must reproduce the above copyright
//! > notice, this list of conditions and the following disclaimer in the
//! > documentation and/or other materials provided with the distribution.
//! >
//! > THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS
//! > IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED
//! > TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
//! > PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER
//! > OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
//! > EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
//! > PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
//! > PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
//! > LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
//! > NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
//! > SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//!
//! The normalization code was in turn derived from the markdowntest project:
//!
//! > Copyright (c) 2013 Karl Dubost. Released under the MIT License:
//! > Permission is hereby granted, free of charge, to any person obtaining a
//! > copy of this software and associated documentation files (the
//! > "Software"), to deal in the Software without restriction, including
//! > without limitation the rights to use, copy, modify, merge, publish,
//! > distribute, sublicense, and/or sell copies of the Software, and to
//! > permit persons to whom the Software is furnished to do so, subject to
//! > the following conditions: The above copyright notice and this
//! > permission notice shall be included in all copies or substantial
//! > portions of the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT
//! > WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
//! > THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! > NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
//! > LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
//! > OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
//! > WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use std::borrow::Cow;

use super::html4_entities::HTML4_ENTITIES;

/// Normalizes `html` with the CommonMark spec rules (see the module docs).
/// Two HTML strings are equivalent for conformance purposes iff their
/// normalized forms are equal.
pub fn normalize_html(html: &str) -> String {
    let input = drop_unchunkable_lt(html);
    let mut n = Normalizer {
        out: String::with_capacity(input.len()),
        last: Last::StartTag,
        last_tag: String::new(),
        in_pre: false,
        cdata_elem: None,
    };
    n.run(&input);
    n.out
}

/// The Python version first splits the input with the regex
/// `<!\[CDATA\[.*?\]\]>|<[^>]*>|[^<]+`, which silently skips every `<` that
/// has no `>` anywhere after it. Reproduce that.
fn drop_unchunkable_lt(html: &str) -> Cow<'_, str> {
    let tail_start = html.rfind('>').map_or(0, |i| i + 1);
    if html[tail_start..].contains('<') {
        let mut s = String::with_capacity(html.len());
        s.push_str(&html[..tail_start]);
        s.extend(html[tail_start..].chars().filter(|&c| c != '<'));
        Cow::Owned(s)
    } else {
        Cow::Borrowed(html)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    StartTag,
    EndTag,
    Data,
    Other,
}

struct Normalizer {
    out: String,
    last: Last,
    last_tag: String,
    in_pre: bool,
    /// Inside `<script>` or `<style>`: everything up to the end tag is text.
    cdata_elem: Option<String>,
}

/// Python's `str.isspace` (what `\s` and `strip()` match on `str`).
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// ASCII whitespace as the tokenizer regexes use it inside tags.
fn is_tag_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c')
}

fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "article"
            | "header"
            | "aside"
            | "hgroup"
            | "blockquote"
            | "hr"
            | "iframe"
            | "body"
            | "li"
            | "map"
            | "button"
            | "object"
            | "canvas"
            | "ol"
            | "caption"
            | "output"
            | "col"
            | "p"
            | "colgroup"
            | "pre"
            | "dd"
            | "progress"
            | "div"
            | "section"
            | "dl"
            | "table"
            | "td"
            | "dt"
            | "tbody"
            | "embed"
            | "textarea"
            | "fieldset"
            | "tfoot"
            | "figcaption"
            | "th"
            | "figure"
            | "thead"
            | "footer"
            | "tr"
            | "form"
            | "ul"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "video"
            | "script"
            | "style"
    )
}

fn html4_entity(name: &str) -> Option<char> {
    HTML4_ENTITIES
        .binary_search_by(|(n, _)| (*n).cmp(name))
        .ok()
        .and_then(|i| char::from_u32(HTML4_ENTITIES[i].1))
}

/// Finds `needle` in `hay[from..]`, ASCII case-insensitively.
fn find_ci(hay: &str, from: usize, needle: &str) -> Option<usize> {
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    (from..=h.len().checked_sub(n.len())?).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

fn find_from(hay: &str, from: usize, needle: &str) -> Option<usize> {
    hay.get(from..)?.find(needle).map(|i| i + from)
}

fn rstrip_py(s: &mut String) {
    let keep = s.trim_end_matches(is_py_space).len();
    s.truncate(keep);
}

impl Normalizer {
    fn run(&mut self, s: &str) {
        let b = s.as_bytes();
        let n = b.len();
        let mut i = 0;
        while i < n {
            if let Some(elem) = self.cdata_elem.clone() {
                i = self.raw_text(s, i, &elem);
                continue;
            }
            match b[i] {
                b'<' => {
                    if s[i..].starts_with("<![CDATA[")
                        && let Some(end) = find_from(s, i + 9, "]]>")
                        && !s[i..end].contains('\n')
                    {
                        // Copied verbatim, bypassing the tokenizer state.
                        self.out.push_str(&s[i..end + 3]);
                        i = end + 3;
                    } else {
                        i = self.markup(s, i);
                    }
                }
                b'&' => i = self.reference(s, i),
                _ => {
                    let j = s[i..].find(['<', '&']).map_or(n, |j| i + j);
                    self.data(&s[i..j]);
                    i = j;
                }
            }
        }
    }

    /// Text inside `<script>` or `<style>`, up to and including its end tag.
    fn raw_text(&mut self, s: &str, i: usize, elem: &str) -> usize {
        let close = format!("</{elem}");
        let Some(j) = find_ci(s, i, &close) else {
            self.data(&s[i..]);
            return s.len();
        };
        if j > i {
            self.data(&s[i..j]);
        }
        match find_from(s, j, ">") {
            Some(gt) => {
                self.cdata_elem = None;
                self.end_tag_event(elem);
                gt + 1
            }
            None => {
                self.data(&s[j..]);
                s.len()
            }
        }
    }

    /// Something starting with `<`; returns the index after it.
    fn markup(&mut self, s: &str, i: usize) -> usize {
        let b = s.as_bytes();
        let next = b.get(i + 1).copied();
        if next.is_some_and(|c| c.is_ascii_alphabetic()) {
            self.start_tag(s, i)
        } else if next == Some(b'/') {
            self.end_tag(s, i)
        } else if s[i..].starts_with("<!--") {
            match find_from(s, i + 4, "-->") {
                Some(j) => {
                    self.special("<!--", &s[i + 4..j], "-->");
                    j + 3
                }
                None => self.incomplete(s, i),
            }
        } else if s[i..].starts_with("<?") {
            match find_from(s, i + 2, ">") {
                Some(j) => {
                    self.special("<?", &s[i + 2..j], ">");
                    j + 1
                }
                None => self.incomplete(s, i),
            }
        } else if s[i..].starts_with("<!") {
            if b.len() >= i + 9 && b[i..i + 9].eq_ignore_ascii_case(b"<!doctype") {
                match find_from(s, i + 9, ">") {
                    Some(j) => {
                        self.special("<!", &s[i + 2..j], ">");
                        j + 1
                    }
                    None => self.incomplete(s, i),
                }
            } else if s[i..].starts_with("<![") {
                // Marked section (multi-line CDATA): an "unknown declaration".
                match find_from(s, i + 3, "]]>") {
                    Some(j) => {
                        self.special("<!", &s[i + 3..j], ">");
                        j + 3
                    }
                    None => self.incomplete(s, i),
                }
            } else {
                // Bogus comment.
                match find_from(s, i + 2, ">") {
                    Some(j) => {
                        self.special("<!--", &s[i + 2..j], "-->");
                        j + 1
                    }
                    None => self.incomplete(s, i),
                }
            }
        } else {
            self.data("<");
            i + 1
        }
    }

    /// A construct that never completes: emitted as text up to the next `>`
    /// (or `<`), as Python's parser does when it is closed.
    fn incomplete(&mut self, s: &str, i: usize) -> usize {
        let k = match find_from(s, i + 1, ">") {
            Some(gt) => gt + 1,
            None => find_from(s, i + 1, "<").unwrap_or(i + 1),
        };
        self.data(&s[i..k]);
        k
    }

    fn start_tag(&mut self, s: &str, i: usize) -> usize {
        let b = s.as_bytes();
        let n = b.len();
        let mut k = i + 1;
        while k < n
            && !matches!(
                b[k],
                b'\t' | b'\n' | b'\r' | b'\x0c' | b' ' | b'/' | b'>' | 0
            )
        {
            k += 1;
        }
        let tag = s[i + 1..k].to_lowercase();
        let mut attrs: Vec<(String, Option<String>)> = Vec::new();
        let self_closing = loop {
            while k < n && (is_tag_space(b[k]) || (b[k] == b'/' && b.get(k + 1) != Some(&b'>'))) {
                k += 1;
            }
            if k >= n {
                return self.incomplete(s, i);
            }
            if b[k] == b'>' {
                k += 1;
                break false;
            }
            if s[k..].starts_with("/>") {
                k += 2;
                break true;
            }
            // Attribute name: [^\s/>][^\s/=>]*
            let name_start = k;
            k += 1;
            while k < n && !is_tag_space(b[k]) && !matches!(b[k], b'/' | b'=' | b'>') {
                k += 1;
            }
            let name = s[name_start..k].to_lowercase();
            // Optional value: \s*=+\s*('…'|"…"|bare)
            let mut v = k;
            while v < n && is_tag_space(b[v]) {
                v += 1;
            }
            let mut value = None;
            if v < n && b[v] == b'=' {
                while v < n && b[v] == b'=' {
                    v += 1;
                }
                while v < n && is_tag_space(b[v]) {
                    v += 1;
                }
                if v < n && (b[v] == b'"' || b[v] == b'\'') {
                    let quote = if b[v] == b'"' { "\"" } else { "'" };
                    match find_from(s, v + 1, quote) {
                        Some(end) => {
                            value = Some(&s[v + 1..end]);
                            k = end + 1;
                        }
                        None => return self.incomplete(s, i),
                    }
                } else {
                    let start = v;
                    while v < n && !is_tag_space(b[v]) && b[v] != b'>' {
                        v += 1;
                    }
                    value = Some(&s[start..v]);
                    k = v;
                }
            }
            attrs.push((name, value.map(unescape_attr)));
        };
        self.start_tag_event(&tag, attrs);
        if self_closing {
            self.last = Last::EndTag;
        } else if tag == "script" || tag == "style" {
            self.cdata_elem = Some(tag);
        }
        k
    }

    fn end_tag(&mut self, s: &str, i: usize) -> usize {
        let b = s.as_bytes();
        let n = b.len();
        let Some(gt) = find_from(s, i + 2, ">") else {
            return self.incomplete(s, i);
        };
        // Strict form: </\s*([a-zA-Z][-.a-zA-Z0-9:_]*)\s*>
        let mut k = i + 2;
        while k < n && is_tag_space(b[k]) {
            k += 1;
        }
        if k < n && b[k].is_ascii_alphabetic() {
            let name_start = k;
            while k < n
                && (b[k].is_ascii_alphanumeric() || matches!(b[k], b'-' | b'.' | b':' | b'_'))
            {
                k += 1;
            }
            let name_end = k;
            while k < n && is_tag_space(b[k]) {
                k += 1;
            }
            if k < n && b[k] == b'>' {
                let tag = s[name_start..name_end].to_ascii_lowercase();
                self.end_tag_event(&tag);
                return k + 1;
            }
        }
        // Tolerant form: a name right after `</`, junk up to the next `>`.
        if b.get(i + 2).is_some_and(u8::is_ascii_alphabetic) {
            let mut k = i + 2;
            while k < n
                && !matches!(
                    b[k],
                    b'\t' | b'\n' | b'\r' | b'\x0c' | b' ' | b'/' | b'>' | 0
                )
            {
                k += 1;
            }
            let tag = s[i + 2..k].to_lowercase();
            let gt = find_from(s, k, ">").unwrap_or(gt);
            self.end_tag_event(&tag);
            return gt + 1;
        }
        if s[i..].starts_with("</>") {
            return i + 3;
        }
        self.special("<!--", &s[i + 2..gt], "-->");
        gt + 1
    }

    /// `&…`; returns the index after the reference (or after the `&`).
    fn reference(&mut self, s: &str, i: usize) -> usize {
        let b = s.as_bytes();
        let n = b.len();
        if s[i..].starts_with("&#") {
            let mut k = i + 2;
            let hex = k < n && (b[k] == b'x' || b[k] == b'X');
            if hex {
                k += 1;
            }
            let digits = k;
            while k < n
                && (if hex {
                    b[k].is_ascii_hexdigit()
                } else {
                    b[k].is_ascii_digit()
                })
            {
                k += 1;
            }
            if k > digits && k < n && !b[k].is_ascii_hexdigit() {
                let name = &s[i + 2..k];
                let c = match name.strip_prefix('x') {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => name.parse::<u32>().ok(),
                }
                .and_then(char::from_u32);
                self.output_char(c, &format!("&{name};"));
                return if b[k] == b';' { k + 1 } else { k };
            }
            self.data("&#");
            return i + 2;
        }
        if b.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
            let mut k = i + 1;
            while k < n && (b[k].is_ascii_alphanumeric() || b[k] == b'-' || b[k] == b'.') {
                k += 1;
            }
            // The name needs a terminating character; at the end of the
            // input the regex backtracks to the last `-` or `.`.
            let (end, consumed) = if k < n {
                (k, if b[k] == b';' { k + 1 } else { k })
            } else if let Some(p) = s[i + 2..k].rfind(['-', '.']) {
                (i + 2 + p, i + 2 + p)
            } else {
                self.data(&s[i..]);
                return n;
            };
            let name = &s[i + 1..end];
            self.output_char(html4_entity(name), &format!("&{name};"));
            return consumed;
        }
        self.data("&");
        i + 1
    }

    fn output_char(&mut self, c: Option<char>, fallback: &str) {
        match c {
            Some('<') => self.out.push_str("&lt;"),
            Some('>') => self.out.push_str("&gt;"),
            Some('&') => self.out.push_str("&amp;"),
            Some('"') => self.out.push_str("&quot;"),
            Some(c) => self.out.push(c),
            None => self.out.push_str(fallback),
        }
        self.last = Last::Other;
    }

    fn data(&mut self, data: &str) {
        if data.is_empty() {
            return;
        }
        let after_tag = matches!(self.last, Last::StartTag | Last::EndTag);
        let after_block_tag = after_tag && is_block_tag(&self.last_tag);
        let mut d: Cow<'_, str> = Cow::Borrowed(data);
        if after_tag && self.last_tag == "br" {
            d = Cow::Owned(d.trim_start_matches('\n').to_owned());
        }
        if !self.in_pre {
            d = Cow::Owned(collapse_whitespace(&d));
        }
        if after_block_tag && !self.in_pre {
            if self.last == Last::StartTag {
                d = Cow::Owned(d.trim_start_matches(is_py_space).to_owned());
            } else {
                d = Cow::Owned(d.trim_matches(is_py_space).to_owned());
            }
        }
        self.out.push_str(&d);
        self.last = Last::Data;
    }

    fn start_tag_event(&mut self, tag: &str, mut attrs: Vec<(String, Option<String>)>) {
        if tag == "pre" {
            self.in_pre = true;
        }
        if is_block_tag(tag) {
            rstrip_py(&mut self.out);
        }
        self.out.push('<');
        self.out.push_str(tag);
        attrs.sort();
        for (k, v) in attrs {
            self.out.push(' ');
            self.out.push_str(&k);
            if let Some(v) = v {
                self.out.push_str("=\"");
                escape_attr(&v, &mut self.out);
                self.out.push('"');
            }
        }
        self.out.push('>');
        tag.clone_into(&mut self.last_tag);
        self.last = Last::StartTag;
    }

    fn end_tag_event(&mut self, tag: &str) {
        if tag == "pre" {
            self.in_pre = false;
        } else if is_block_tag(tag) {
            rstrip_py(&mut self.out);
        }
        self.out.push_str("</");
        self.out.push_str(tag);
        self.out.push('>');
        tag.clone_into(&mut self.last_tag);
        self.last = Last::EndTag;
    }

    /// Comments, declarations and processing instructions.
    fn special(&mut self, open: &str, body: &str, close: &str) {
        self.out.push_str(open);
        self.out.push_str(body);
        self.out.push_str(close);
        self.last = Last::Other;
    }
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if is_py_space(c) {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// Python's `html.escape(v, quote=True)`.
fn escape_attr(v: &str, out: &mut String) {
    for c in v.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
}

/// Decodes character references in an attribute value: numeric references
/// (`;` optional) and HTML 4 named references (`;` required).
fn unescape_attr(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let b = rest.as_bytes();
        if b.get(1) == Some(&b'#') {
            let hex = matches!(b.get(2), Some(b'x' | b'X'));
            let start = if hex { 3 } else { 2 };
            let len = rest[start..]
                .bytes()
                .take_while(|c| {
                    if hex {
                        c.is_ascii_hexdigit()
                    } else {
                        c.is_ascii_digit()
                    }
                })
                .count();
            if len > 0 {
                let digits = &rest[start..start + len];
                let code = u32::from_str_radix(digits, if hex { 16 } else { 10 }).ok();
                let c = code
                    .filter(|&c| c != 0)
                    .and_then(char::from_u32)
                    .unwrap_or('\u{fffd}');
                out.push(c);
                let mut end = start + len;
                if rest.as_bytes().get(end) == Some(&b';') {
                    end += 1;
                }
                rest = &rest[end..];
                continue;
            }
        } else if let Some(semi) = rest.find(';')
            && let Some(c) = html4_entity(&rest[1..semi])
        {
            out.push(c);
            rest = &rest[semi + 1..];
            continue;
        }
        out.push('&');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::normalize_html as norm;

    /// The examples from the docstring of the original `normalize_html`.
    #[test]
    fn upstream_doctests() {
        assert_eq!(norm("<p>a  \t b</p>"), "<p>a b</p>");
        assert_eq!(norm("<p>a  \t\nb</p>"), "<p>a b</p>");
        assert_eq!(norm("<p>a  b</p>"), "<p>a b</p>");
        assert_eq!(norm(" <p>a  b</p>"), "<p>a b</p>");
        assert_eq!(norm("<p>a  b</p> "), "<p>a b</p>");
        assert_eq!(norm("\n\t<p>\n\t\ta  b\t\t</p>\n\t"), "<p>a b</p>");
        assert_eq!(norm("<i>a  b</i> "), "<i>a b</i> ");
        assert_eq!(norm("<br />"), "<br>");
        assert_eq!(
            norm(r#"<a title="bar" HREF="foo">x</a>"#),
            r#"<a href="foo" title="bar">x</a>"#
        );
        assert_eq!(
            norm("&forall;&amp;&gt;&lt;&quot;"),
            "\u{2200}&amp;&gt;&lt;&quot;"
        );
    }

    #[test]
    fn pre_keeps_whitespace() {
        assert_eq!(
            norm("<pre><code>a  \n  b\n</code></pre>\n"),
            "<pre><code>a  \n  b\n</code></pre>"
        );
    }

    #[test]
    fn newline_after_br_is_dropped() {
        assert_eq!(norm("<p>a<br />\nb</p>\n"), "<p>a<br>b</p>");
    }

    #[test]
    fn character_references() {
        assert_eq!(norm("&#65;&#x42;&#X43;"), "AB&X43;");
        assert_eq!(norm("&ngE; &copy;"), "&ngE; \u{a9}");
        // Like Python's parser: `&T` followed by a space is a reference.
        assert_eq!(norm("AT&T &"), "AT&T; &");
        assert_eq!(norm("&#34;"), "&quot;");
    }

    #[test]
    fn attributes() {
        assert_eq!(
            norm(r#"<input type="checkbox" disabled="" checked />"#),
            r#"<input checked disabled="" type="checkbox">"#
        );
        assert_eq!(
            norm(r#"<a href="/u?a=1&amp;b='2'">x</a>"#),
            r#"<a href="/u?a=1&amp;b=&#x27;2&#x27;">x</a>"#
        );
        assert_eq!(norm("<img src=x alt='a b'>"), r#"<img alt="a b" src="x">"#);
    }

    #[test]
    fn comments_declarations_cdata() {
        assert_eq!(norm("<!-- a > b -->"), "<!-- a > b -->");
        assert_eq!(norm("<!DOCTYPE html>"), "<!DOCTYPE html>");
        assert_eq!(norm("<![CDATA[ <x> ]]>"), "<![CDATA[ <x> ]]>");
        assert_eq!(norm("<?php echo 1; ?>"), "<?php echo 1; ?>");
        assert_eq!(norm("<!foo>"), "<!--foo-->");
    }

    #[test]
    fn script_content_is_text() {
        assert_eq!(
            norm("<script>if (a < b) {}</script>"),
            "<script>if (a < b) {}</script>"
        );
    }

    #[test]
    fn lone_lt_without_gt_is_dropped() {
        assert_eq!(norm("a < b"), "a b");
        assert_eq!(norm("<p>x</p> < y <"), "<p>x</p>y");
    }

    #[test]
    fn equivalent_cmark_outputs_compare_equal() {
        let a = "<ul>\n<li>a</li>\n<li>b</li>\n</ul>\n";
        let b = "<ul><li>a</li><li>b</li></ul>";
        assert_eq!(norm(a), norm(b));
        assert_ne!(norm("<p>a</p>"), norm("<p>b</p>"));
        assert_ne!(norm("<em>a</em>"), norm("<strong>a</strong>"));
    }

    #[test]
    fn never_panics_on_truncated_markup() {
        for s in [
            "<",
            "</",
            "<!",
            "<!-",
            "<?",
            "&",
            "&#",
            "&#x",
            "<a",
            "<a b='",
            "<a b=",
            "</a",
            "<![CDATA[",
            "&a-",
            "<script>",
            "é<é",
            "&é",
        ] {
            let _ = norm(s);
        }
    }
}
