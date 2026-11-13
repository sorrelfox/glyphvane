// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The CommonMark 0.31.2 spec examples of the inline sections, run through
//! the inline parser alone.
//!
//! A minimal block splitter (tests only) turns each example into
//! paragraphs, ATX and setext headings, thematic breaks and code blocks,
//! extracts link reference definitions with `parse_ref_def`, and hands each
//! paragraph or heading to `parse_inlines` as line segments. Examples that
//! need block quotes, lists or HTML blocks are skipped (the full pipeline,
//! `cargo xtask spec`, covers them).
//!
//! Every example runs twice: with segments that exclude their line endings
//! (the parser implies them) and with segments that include them.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::path::PathBuf;

use gv_syntax::harness::normalize_html;
use gv_syntax::inline::{
    InlineInput, InlineKind, RefDef, normalize_label, parse_inlines, parse_ref_def,
    render_inlines_html,
};

/// The sections whose examples are asserted.
const INLINE_SECTIONS: &[&str] = &[
    "Inlines",
    "Backslash escapes",
    "Entity and numeric character references",
    "Code spans",
    "Emphasis and strong emphasis",
    "Links",
    "Images",
    "Autolinks",
    "Raw HTML",
    "Hard line breaks",
    "Soft line breaks",
    "Textual content",
];

struct Example {
    number: u64,
    section: String,
    markdown: String,
    html: String,
}

fn spec_path() -> PathBuf {
    match std::env::var_os("GV_SPEC_JSON") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/spec/commonmark-0.31.2.json"),
    }
}

fn load_examples() -> Vec<Example> {
    let text = std::fs::read_to_string(spec_path()).expect("spec fixture");
    let json: serde_json::Value = serde_json::from_str(&text).expect("spec JSON");
    json.as_array()
        .expect("array")
        .iter()
        .map(|e| Example {
            number: e["example"].as_u64().expect("number"),
            section: e["section"].as_str().unwrap_or_default().to_owned(),
            markdown: e["markdown"].as_str().unwrap_or_default().to_owned(),
            html: e["html"].as_str().unwrap_or_default().to_owned(),
        })
        .collect()
}

/// A block of the minimal splitter.
enum Block {
    /// Paragraph or heading content: line ranges without line endings, and
    /// the same lines with them.
    Inline {
        tag: &'static str,
        lines: Vec<Range<usize>>,
        with_eol: Vec<Range<usize>>,
    },
    Html(String),
}

/// Lines of `doc` as (content range, range including the line ending).
fn lines(doc: &str) -> Vec<(Range<usize>, Range<usize>)> {
    let b = doc.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
            i += 1;
        }
        let content = start..i;
        if i < b.len() {
            i += if b[i] == b'\r' && b.get(i + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
        }
        out.push((content, start..i));
    }
    out
}

fn indent(s: &str) -> usize {
    s.bytes().take_while(|&c| c == b' ').count()
}

fn is_blank(s: &str) -> bool {
    s.bytes().all(|c| c == b' ' || c == b'\t')
}

fn thematic_break(s: &str) -> bool {
    if indent(s) > 3 {
        return false;
    }
    let t: Vec<u8> = s.bytes().filter(|&c| c != b' ' && c != b'\t').collect();
    t.len() >= 3 && matches!(t[0], b'-' | b'*' | b'_') && t.iter().all(|&c| c == t[0])
}

fn setext(s: &str) -> Option<&'static str> {
    if indent(s) > 3 {
        return None;
    }
    let t = s.trim();
    if !t.is_empty() && t.bytes().all(|c| c == b'=') {
        Some("h1")
    } else if !t.is_empty() && t.bytes().all(|c| c == b'-') {
        Some("h2")
    } else {
        None
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Splits `doc` into blocks; `None` if it needs constructs the splitter
/// does not know.
fn split(doc: &str, refs: &mut HashMap<String, RefDef>) -> Option<Vec<Block>> {
    let ls = lines(doc);
    let mut blocks = Vec::new();
    let mut para: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    let mut i = 0;
    let close = |para: &mut Vec<(Range<usize>, Range<usize>)>,
                 blocks: &mut Vec<Block>,
                 refs: &mut HashMap<String, RefDef>,
                 tag: &'static str| {
        if para.is_empty() {
            return true;
        }
        // Reference definitions at the start of a paragraph.
        let mut first = 0;
        {
            loop {
                let joined: Vec<&str> =
                    para[first..].iter().map(|(c, _)| &doc[c.clone()]).collect();
                let joined = joined.join("\n");
                let Some((n, label, def)) = parse_ref_def(&joined) else {
                    break;
                };
                refs.entry(label).or_insert(def);
                // `n` ends at a line boundary of the joined text.
                let mut consumed = 0;
                while first < para.len() && consumed < n {
                    consumed += para[first].0.len() + 1;
                    first += 1;
                }
                if first >= para.len() {
                    break;
                }
            }
        }
        if first < para.len() {
            blocks.push(Block::Inline {
                tag,
                lines: para[first..].iter().map(|(c, _)| c.clone()).collect(),
                with_eol: para[first..].iter().map(|(_, e)| e.clone()).collect(),
            });
        } else if tag != "p" {
            return false;
        }
        para.clear();
        true
    };
    while i < ls.len() {
        let (c, e) = ls[i].clone();
        let line = &doc[c.clone()];
        if is_blank(line) {
            if !close(&mut para, &mut blocks, refs, "p") {
                return None;
            }
            i += 1;
            continue;
        }
        if line.contains('\t')
            && indent(line) < line.len()
            && line[indent(line)..].starts_with('\t')
        {
            return None;
        }
        let ind = indent(line);
        let rest = &line[ind.min(line.len())..];
        if !para.is_empty()
            && let Some(tag) = setext(line)
        {
            if !close(&mut para, &mut blocks, refs, tag) {
                return None;
            }
            i += 1;
            continue;
        }
        if ind >= 4 {
            if !para.is_empty() {
                para.push((c.start + ind..c.end, c.start + ind..e.end));
                i += 1;
                continue;
            }
            // Indented code.
            let mut code = String::new();
            let mut j = i;
            let mut pending_blank = String::new();
            while j < ls.len() {
                let l = &doc[ls[j].0.clone()];
                if is_blank(l) {
                    pending_blank.push_str(l.get(4..).unwrap_or(""));
                    pending_blank.push('\n');
                } else if indent(l) >= 4 {
                    code.push_str(&pending_blank);
                    pending_blank.clear();
                    code.push_str(&l[4..]);
                    code.push('\n');
                } else {
                    break;
                }
                j += 1;
            }
            blocks.push(Block::Html(format!(
                "<pre><code>{}</code></pre>",
                escape(&code)
            )));
            i = j;
            continue;
        }
        if thematic_break(line) {
            if !close(&mut para, &mut blocks, refs, "p") {
                return None;
            }
            blocks.push(Block::Html("<hr />".into()));
            i += 1;
            continue;
        }
        let hashes = rest.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes)
            && rest[hashes..]
                .chars()
                .next()
                .is_none_or(|ch| ch == ' ' || ch == '\t')
        {
            if !close(&mut para, &mut blocks, refs, "p") {
                return None;
            }
            let mut content = rest[hashes..].trim_matches([' ', '\t']);
            let stripped = content.trim_end_matches('#');
            if stripped.is_empty() || stripped.ends_with([' ', '\t']) {
                content = stripped.trim_end_matches([' ', '\t']);
            }
            let start = c.start + ind + hashes + rest[hashes..].find(content).unwrap_or(0);
            let r = start..start + content.len();
            let tag = ["h1", "h2", "h3", "h4", "h5", "h6"][hashes - 1];
            blocks.push(Block::Inline {
                tag,
                lines: vec![r.clone()],
                with_eol: vec![r],
            });
            i += 1;
            continue;
        }
        if rest.starts_with("```") || rest.starts_with("~~~") {
            if !close(&mut para, &mut blocks, refs, "p") {
                return None;
            }
            let fc = rest.as_bytes()[0];
            let flen = rest.bytes().take_while(|&b| b == fc).count();
            let info = rest[flen..].trim();
            if fc == b'`' && info.contains('`') {
                return None;
            }
            let mut code = String::new();
            let mut j = i + 1;
            while j < ls.len() {
                let l = &doc[ls[j].0.clone()];
                let li = indent(l);
                let lr = &l[li.min(l.len())..];
                if li <= 3
                    && lr.bytes().take_while(|&b| b == fc).count() >= flen
                    && lr.trim_start_matches(fc as char).trim().is_empty()
                {
                    j += 1;
                    break;
                }
                let strip = li.min(ind);
                code.push_str(&l[strip..]);
                code.push('\n');
                j += 1;
            }
            let lang = info.split_whitespace().next().unwrap_or("");
            let mut html = String::from("<pre><code");
            if !lang.is_empty() {
                // Entities and escapes in the info string are not decoded here.
                if lang.contains(['&', '\\']) {
                    return None;
                }
                let _ = write!(html, " class=\"language-{}\"", escape(lang));
            }
            let _ = write!(html, ">{}</code></pre>", escape(&code));
            blocks.push(Block::Html(html));
            i = j;
            continue;
        }
        let starts_container = rest.starts_with('>')
            || ((rest.starts_with(['-', '*', '+']))
                && rest[1..]
                    .chars()
                    .next()
                    .is_none_or(|ch| ch == ' ' || ch == '\t'))
            || {
                let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
                (1..=9).contains(&digits)
                    && rest[digits..].starts_with(['.', ')'])
                    && rest[digits + 1..]
                        .chars()
                        .next()
                        .is_none_or(|ch| ch == ' ' || ch == '\t')
            };
        if starts_container {
            return None;
        }
        if let Some(kind) = html_block_start(doc, c.start + ind..c.end, para.is_empty()) {
            if !close(&mut para, &mut blocks, refs, "p") {
                return None;
            }
            let mut html = String::new();
            let mut j = i;
            while j < ls.len() {
                let l = &doc[ls[j].0.clone()];
                if kind >= 6 && is_blank(l) {
                    break;
                }
                html.push_str(l);
                html.push('\n');
                j += 1;
                if kind < 6 && html_block_ends(kind, l) {
                    break;
                }
            }
            html.pop();
            blocks.push(Block::Html(html));
            i = j;
            continue;
        }
        para.push((c.start + ind..c.end, c.start + ind..e.end));
        i += 1;
    }
    if !close(&mut para, &mut blocks, refs, "p") {
        return None;
    }
    Some(blocks)
}

const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
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
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];

/// The HTML block type (1 to 7) a line starts, if any. Type 7 cannot
/// interrupt a paragraph.
fn html_block_start(doc: &str, line: Range<usize>, para_empty: bool) -> Option<u8> {
    let rest = &doc[line.clone()];
    let lower = rest.to_ascii_lowercase();
    let ends_name =
        |s: &str| s.is_empty() || s.starts_with([' ', '\t', '>']) || s.starts_with("/>");
    for name in ["pre", "script", "style", "textarea"] {
        if let Some(after) = lower.strip_prefix('<').and_then(|s| s.strip_prefix(name))
            && (after.is_empty() || after.starts_with([' ', '\t', '>']))
        {
            return Some(1);
        }
    }
    if lower.starts_with("<!--") {
        return Some(2);
    }
    if lower.starts_with("<?") {
        return Some(3);
    }
    if lower.starts_with("<![cdata[") {
        return Some(5);
    }
    if lower.starts_with("<!") && lower.as_bytes().get(2).is_some_and(u8::is_ascii_alphabetic) {
        return Some(4);
    }
    let tag = lower.strip_prefix("</").or_else(|| lower.strip_prefix('<'));
    if let Some(tag) = tag {
        let n = tag
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric())
            .count();
        if BLOCK_TAGS.contains(&&tag[..n]) && ends_name(&tag[n..]) {
            return Some(6);
        }
    }
    if para_empty && rest.starts_with('<') {
        // A complete open or closing tag followed only by whitespace.
        let tree = parse_inlines(doc, InlineInput { segments: &[line] }, &());
        let mut nodes = tree.top_level();
        let first = nodes.next()?;
        let text = first.text();
        let is_tag =
            text.starts_with("</") || text.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic);
        if first.kind() == InlineKind::RawHtml
            && is_tag
            && nodes.all(|n| n.kind() == InlineKind::Whitespace)
        {
            return Some(7);
        }
    }
    None
}

/// Whether `line` ends an HTML block of type 1 to 5.
fn html_block_ends(kind: u8, line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    match kind {
        1 => ["</pre>", "</script>", "</style>", "</textarea>"]
            .iter()
            .any(|t| lower.contains(t)),
        2 => line.contains("-->"),
        3 => line.contains("?>"),
        4 => line.contains('>'),
        _ => line.contains("]]>"),
    }
}

/// Renders `doc` with the minimal splitter; `with_eol` selects segments
/// that include their line endings.
fn render(doc: &str, with_eol: bool) -> Option<String> {
    let mut refs = HashMap::new();
    let blocks = split(doc, &mut refs)?;
    let mut out = String::new();
    for block in &blocks {
        match block {
            Block::Html(h) => {
                out.push_str(h);
                out.push('\n');
            }
            Block::Inline {
                tag,
                lines,
                with_eol: eol_lines,
            } => {
                let segments = if with_eol { eol_lines } else { lines };
                let tree = parse_inlines(doc, InlineInput { segments }, &refs);
                let _ = write!(out, "<{tag}>");
                render_inlines_html(&tree, &mut out);
                let _ = writeln!(out, "</{tag}>");
                // Losslessness: the tokens tile the segments.
                let mut covered = Vec::new();
                for (_, r) in tree.tokens() {
                    covered.push(r);
                }
                assert_tiles(doc, segments, &covered);
            }
        }
    }
    Some(out)
}

/// Asserts that `tokens` cover `segments` exactly, in order.
fn assert_tiles(doc: &str, segments: &[Range<usize>], tokens: &[Range<usize>]) {
    let mut expected = String::new();
    for s in segments {
        expected.push_str(&doc[s.clone()]);
    }
    let mut got = String::new();
    let mut last_end = 0;
    for t in tokens {
        assert!(
            t.start >= last_end,
            "tokens overlap or go backwards: {tokens:?}"
        );
        last_end = t.end;
        got.push_str(&doc[t.clone()]);
    }
    assert_eq!(got, expected, "tokens do not tile the segments");
}

#[test]
fn spec_inline_sections() {
    let examples = load_examples();
    let mut report = String::new();
    let mut failures = Vec::new();
    let mut skipped_examples = Vec::new();
    let mut by_section: Vec<(String, usize, usize, usize)> = Vec::new();
    for ex in examples
        .iter()
        .filter(|e| INLINE_SECTIONS.contains(&e.section.as_str()))
    {
        if by_section.last().is_none_or(|s| s.0 != ex.section) {
            by_section.push((ex.section.clone(), 0, 0, 0));
        }
        let entry = by_section.last_mut().expect("section");
        let expected = normalize_html(&ex.html);
        let mut skipped = false;
        let mut ok = true;
        for with_eol in [false, true] {
            match render(&ex.markdown, with_eol) {
                None => skipped = true,
                Some(html) => {
                    if normalize_html(&html) != expected {
                        ok = false;
                        failures.push(format!(
                            "example {} ({}; segments {}):\n  markdown: {:?}\n  expected: {:?}\n  actual:   {:?}",
                            ex.number,
                            ex.section,
                            if with_eol { "with line endings" } else { "without line endings" },
                            ex.markdown,
                            ex.html,
                            html
                        ));
                    }
                }
            }
        }
        if skipped {
            entry.3 += 1;
            skipped_examples.push(ex.number);
        } else if ok {
            entry.1 += 1;
        } else {
            entry.2 += 1;
        }
    }
    let (mut pass, mut fail, mut skip) = (0, 0, 0);
    for (section, p, f, s) in &by_section {
        let _ = writeln!(report, "{section:42} pass {p:3}  fail {f:3}  skipped {s:3}");
        pass += p;
        fail += f;
        skip += s;
    }
    let _ = writeln!(
        report,
        "{:42} pass {pass:3}  fail {fail:3}  skipped {skip:3}",
        "total"
    );
    let _ = writeln!(
        report,
        "skipped (block structure the splitter lacks): {skipped_examples:?}"
    );
    println!("{report}");
    assert!(failures.is_empty(), "{report}\n{}", failures.join("\n"));
    assert!(pass >= 350, "too few examples ran: {report}");
}

#[test]
fn label_normalization_matches_spec_examples() {
    assert_eq!(normalize_label("ẞ"), normalize_label("SS"));
    assert_eq!(normalize_label("  Foo\n\t bar "), "foo bar");
    assert_eq!(normalize_label("ΑΓΩ"), normalize_label("αγω"));
}
