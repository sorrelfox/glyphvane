// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Round trip: HTML → Markdown (CommonMark profile) → gv-syntax → HTML must
//! be semantically equivalent to the input.
//!
//! "Semantically equivalent" is decided on a canonical form of both HTML
//! strings: element names unified (`b` = `strong`, `i` = `em`, `s` =
//! `del`), only meaningful attributes kept, entities decoded, white space
//! collapsed as a browser would, paragraphs directly inside list items
//! dropped (tight and loose lists look the same), trailing newlines in
//! code blocks ignored.

use gv_html::{PasteOptions, Profile, html_to_markdown};

const BLOCK: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "blockquote",
    "pre",
    "hr",
    "br",
    "table",
    "tr",
    "td",
    "th",
    "div",
];

fn decode(s: &str) -> String {
    s.replace("&copy;", "\u{a9}")
        .replace("&#169;", "\u{a9}")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[derive(Debug, PartialEq)]
enum Tok {
    Open(String),
    Close(String),
    Text(String),
}

/// A small tokenizer for the well-formed HTML used here.
fn tokens(html: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut rest = html;
    let mut pre = 0usize;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('<') {
            let end = stripped.find('>').expect("closed tag");
            let tag = &stripped[..end];
            rest = &stripped[end + 1..];
            let closing = tag.starts_with('/');
            let tag = tag.trim_start_matches('/').trim_end_matches('/').trim();
            let (name, attrs) = tag.split_once(' ').unwrap_or((tag, ""));
            let name = match name.to_ascii_lowercase().as_str() {
                "b" => "strong".to_owned(),
                "i" => "em".to_owned(),
                "s" | "strike" => "del".to_owned(),
                n => n.to_owned(),
            };
            if name == "pre" {
                if closing {
                    pre -= 1;
                } else {
                    pre += 1;
                }
            }
            if closing {
                out.push(Tok::Close(name));
                continue;
            }
            let mut kept = Vec::new();
            let mut a = attrs;
            while let Some(eq) = a.find('=') {
                let key = a[..eq].trim().to_ascii_lowercase();
                let v = &a[eq + 1..];
                let q = v.chars().next().unwrap_or('"');
                let close = v[1..].find(q).map_or(v.len(), |i| i + 1);
                let value = decode(&v[1..close]);
                a = &v[(close + 1).min(v.len())..];
                let keep = match key.as_str() {
                    "href" | "src" | "alt" | "title" => true,
                    "start" => value != "1",
                    "class" => value.starts_with("language-"),
                    _ => false,
                };
                if keep {
                    kept.push(format!("{key}={value:?}"));
                }
            }
            kept.sort();
            out.push(Tok::Open(if kept.is_empty() {
                name
            } else {
                format!("{name} {}", kept.join(" "))
            }));
            if matches!(out.last(), Some(Tok::Open(n)) if n.starts_with("br") || n.starts_with("hr") || n.starts_with("img"))
            {
                let n = match out.last() {
                    Some(Tok::Open(n)) => n.split(' ').next().unwrap_or("").to_owned(),
                    _ => String::new(),
                };
                out.push(Tok::Close(n));
            }
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let text = decode(&rest[..end]);
            rest = &rest[end..];
            let text = if pre > 0 {
                text
            } else {
                text.split_whitespace().collect::<Vec<_>>().join(" ")
                    + if text.ends_with(char::is_whitespace) && !text.trim().is_empty() {
                        " "
                    } else {
                        ""
                    }
            };
            out.push(Tok::Text(text));
        }
    }
    out
}

fn name_of(t: &Tok) -> Option<&str> {
    match t {
        Tok::Open(n) | Tok::Close(n) => n.split(' ').next(),
        Tok::Text(_) => None,
    }
}

/// The canonical form.
fn canonical(html: &str) -> String {
    let toks = tokens(html);
    // Drop `p` directly inside `li`.
    let mut stack: Vec<String> = Vec::new();
    let mut kept: Vec<Tok> = Vec::new();
    for t in toks {
        match &t {
            Tok::Open(_) => {
                let n = name_of(&t).unwrap_or("").to_owned();
                let in_li = stack.last().is_some_and(|s| s == "li");
                stack.push(n.clone());
                if n == "p" && in_li {
                    continue;
                }
            }
            Tok::Close(_) => {
                let n = name_of(&t).unwrap_or("").to_owned();
                stack.pop();
                if n == "p" && stack.last().is_some_and(|s| s == "li") {
                    continue;
                }
            }
            Tok::Text(_) => {}
        }
        kept.push(t);
    }
    // Text: trim next to block boundaries, collapse, drop empties.
    let mut out = String::new();
    let mut in_pre = 0;
    for (i, t) in kept.iter().enumerate() {
        match t {
            Tok::Open(n) => {
                if name_of(t) == Some("pre") {
                    in_pre += 1;
                }
                out.push_str(&format!("<{n}>"));
            }
            Tok::Close(n) => {
                if n == "pre" {
                    in_pre -= 1;
                }
                out.push_str(&format!("</{n}>"));
            }
            Tok::Text(s) => {
                if in_pre > 0 {
                    out.push_str(s.trim_end_matches('\n'));
                    continue;
                }
                let before_block = i == 0
                    || kept
                        .get(i - 1)
                        .and_then(name_of)
                        .is_some_and(|n| BLOCK.contains(&n));
                let after_block = kept
                    .get(i + 1)
                    .and_then(name_of)
                    .is_none_or(|n| BLOCK.contains(&n));
                let mut s = s.as_str();
                if before_block {
                    s = s.trim_start();
                }
                if after_block {
                    s = s.trim_end();
                }
                out.push_str(s);
            }
        }
    }
    out
}

fn roundtrip(html: &str) -> (String, String, String) {
    let md = html_to_markdown(html, PasteOptions::new(Profile::CommonMark));
    let tree = gv_syntax::parse(md.as_str());
    let back = gv_syntax::to_commonmark_html(&tree, md.as_str());
    (md, canonical(html), canonical(&back))
}

const SAMPLES: &[&str] = &[
    "<h1>Title</h1><p>Some <em>emphasis</em>, <strong>strong</strong> and <code>code</code>.</p>",
    "<h2>Section</h2><p>A <a href=\"https://example.com/a\" title=\"T\">link</a> and an <img src=\"https://example.com/i.png\" alt=\"image\">.</p>",
    "<p>Nested <strong>bold <em>and italic</em></strong> text.</p>",
    "<p>Intra<strong>word</strong>bold and <em>\"quoted\"</em>x.</p>",
    "<p>Literal *stars*, _underscores_, snake_case, [brackets], `ticks`, &lt;tags&gt;, &amp;amp; and \\ backslash \\*.</p>",
    "<p># not a heading</p><p>1. not a list</p><p>- not a bullet</p><p>&gt; not a quote</p><p>+ plus</p><p>---</p><p>===</p>",
    "<p>Line one<br>line two<br>- three</p>",
    "<ul><li>one</li><li>two<ul><li>nested <em>item</em></li></ul></li><li>three</li></ul>",
    "<ol start=\"4\"><li>four</li><li>five<ol><li>sub</li></ol></li></ol>",
    "<ul><li><p>loose one</p><p>second para</p></li><li><p>loose two</p></li></ul>",
    "<blockquote><p>Quoted <strong>text</strong></p><ul><li>in a quote</li></ul></blockquote>",
    "<pre><code class=\"language-rust\">fn main() {\n    println!(\"*not* markdown\");\n}\n</code></pre>",
    "<pre><code>has ``` fence\n```\ninside\n</code></pre>",
    "<p>before</p><hr><p>after</p>",
    "<p>A <del>deleted</del> word and <code>a`b</code> span.</p>",
    "<p><a href=\"https://example.com/x\"><img src=\"https://example.com/b.svg\" alt=\"badge\"></a> <a href=\"https://example.com/y\"><strong>bold link</strong></a></p>",
    "<p>Entities: &copy; &#169; stay as text; AT&amp;T; a &amp;copy; b</p>",
    "<p>Trailing hash #</p><h3>Heading #</h3>",
    "<ul><li>a</li></ul><ul><li>b</li></ul>",
    "<p><em>a</em><strong>b</strong><em>c</em></p>",
    "<p>(<strong>paren</strong>) \"<em>quote</em>\" <strong>end.</strong></p>",
];

#[test]
fn round_trip_is_equivalent() {
    let mut failures = Vec::new();
    for html in SAMPLES {
        let (md, want, got) = roundtrip(html);
        if want != got {
            failures.push(format!(
                "{html}\n--- markdown\n{md}\n--- want\n{want}\n--- got\n{got}\n"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn canonical_form_sanity() {
    assert_eq!(
        canonical("<p>a  <b>b</b>\n</p>"),
        canonical("<p>a <strong>b</strong></p>")
    );
    assert_ne!(
        canonical("<p>a<em>b</em></p>"),
        canonical("<p>a<strong>b</strong></p>")
    );
}
