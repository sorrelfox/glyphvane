// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Unit tests of the inline parser, construct by construct: HTML output,
//! token structure, source ranges, accessors and the CST dump.

// Segment lists of a single range are what many tests need.
#![allow(clippy::single_range_in_vec_init)]

use std::collections::HashMap;
use std::ops::Range;

use gv_syntax::inline::{
    InlineInput, InlineKind as K, InlineTree, LinkKind, RefDef, cst_dump_inlines, normalize_label,
    parse_inlines, parse_ref_def, render_inlines_html,
};
use gv_text::TextSnapshot;

/// Segments of `doc` as a paragraph would give them: one per line, line
/// ending excluded, leading spaces and tabs stripped.
fn lines(doc: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in doc.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let lead = content.len() - content.trim_start_matches([' ', '\t']).len();
        out.push(start + lead..start + content.len());
        start += line.len();
    }
    out
}

fn refs() -> HashMap<String, RefDef> {
    let mut refs = HashMap::new();
    for def in ["[foo]: /url \"title\"", "[bar]: <b ar>", "[Ünï]: /u"] {
        let (_, label, def) = parse_ref_def(def).expect("definition");
        refs.insert(label, def);
    }
    refs
}

fn tree(doc: &str) -> InlineTree {
    parse_inlines(
        doc,
        InlineInput {
            segments: &lines(doc),
        },
        &refs(),
    )
}

fn html(doc: &str) -> String {
    let mut out = String::new();
    render_inlines_html(&tree(doc), &mut out);
    out
}

/// The top-level kinds.
fn top(doc: &str) -> Vec<K> {
    tree(doc).top_level().map(|n| n.kind()).collect()
}

/// Every token as (kind, text).
fn tokens(doc: &str) -> Vec<(K, String)> {
    let t = tree(doc);
    t.tokens().map(|(k, r)| (k, doc[r].to_owned())).collect()
}

fn tok(k: K, s: &str) -> (K, String) {
    (k, s.to_owned())
}

#[test]
fn text_escapes_and_entities() {
    assert_eq!(html(r"a \* b \q"), r"a * b \q");
    assert_eq!(
        html("&amp; &copy; &#35; &#x22; &#0; &bogus; &"),
        "&amp; © # &quot; \u{FFFD} &amp;bogus; &amp;"
    );
    assert_eq!(html("a < b > c \" d"), "a &lt; b &gt; c &quot; d");
    assert_eq!(
        tokens(r"x\*&lt;y"),
        vec![
            tok(K::Text, "x"),
            tok(K::Escape, r"\*"),
            tok(K::Entity, "&lt;"),
            tok(K::Text, "y")
        ]
    );
    // U+0000 is replaced when rendering only.
    assert_eq!(html("a\0b"), "a\u{FFFD}b");
    assert_eq!(tokens("a\0b"), vec![tok(K::Text, "a\0b")]);
}

#[test]
fn code_spans() {
    assert_eq!(html("`a`"), "<code>a</code>");
    assert_eq!(html("`` a`b ``"), "<code>a`b</code>");
    assert_eq!(html("` `"), "<code> </code>");
    assert_eq!(html("`a\nb`"), "<code>a b</code>");
    assert_eq!(html("`a``"), "`a``");
    assert_eq!(html("`<a>&amp;\\`"), "<code>&lt;a&gt;&amp;amp;\\</code>");
    assert_eq!(
        tokens("x `a` y"),
        vec![
            tok(K::Text, "x "),
            tok(K::Marker, "`"),
            tok(K::CodeText, "a"),
            tok(K::Marker, "`"),
            tok(K::Text, " y"),
        ]
    );
    let t = tree("x ``a\nb`` y");
    let spans: Vec<_> = t.code_spans().collect();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].range, 2..9);
    assert_eq!(spans[0].content_range, 4..7);
    assert_eq!(spans[0].raw_content, "a\nb");
}

#[test]
fn emphasis() {
    assert_eq!(
        html("*a* _b_ **c** __d__"),
        "<em>a</em> <em>b</em> <strong>c</strong> <strong>d</strong>"
    );
    assert_eq!(html("***a***"), "<em><strong>a</strong></em>");
    assert_eq!(html("*a **b** c*"), "<em>a <strong>b</strong> c</em>");
    assert_eq!(html("foo_bar_baz"), "foo_bar_baz");
    assert_eq!(html("foo*bar*baz"), "foo<em>bar</em>baz");
    // Rule of 3.
    assert_eq!(
        html("*foo**bar**baz*"),
        "<em>foo<strong>bar</strong>baz</em>"
    );
    assert_eq!(html("**foo*bar**"), "<strong>foo*bar</strong>");
    // Unicode punctuation and whitespace (0.31: symbols count too).
    assert_eq!(html("a*\u{A0}b*"), "a*\u{A0}b*");
    assert_eq!(html("*€* a"), "<em>€</em> a");
    assert_eq!(html("*€*a"), "*€*a");
    assert_eq!(html("_«a»_"), "<em>«a»</em>");
    let t = tree("x **a** y");
    let e: Vec<_> = t.emphasis().collect();
    assert_eq!(e.len(), 1);
    assert!(e[0].strong);
    assert_eq!(e[0].range, 2..7);
    assert_eq!(e[0].content_range, 4..5);
    assert_eq!(
        tokens("***a***"),
        vec![
            tok(K::Marker, "*"),
            tok(K::Marker, "**"),
            tok(K::Text, "a"),
            tok(K::Marker, "**"),
            tok(K::Marker, "*"),
        ]
    );
    // Unused delimiters stay text and merge with their neighbours.
    assert_eq!(tokens("a ** b"), vec![tok(K::Text, "a ** b")]);
}

#[test]
fn inline_links_and_images() {
    assert_eq!(html("[a](/u \"t\")"), "<a href=\"/u\" title=\"t\">a</a>");
    assert_eq!(
        html("[a](<b c> 't')"),
        "<a href=\"b%20c\" title=\"t\">a</a>"
    );
    assert_eq!(html("[a]()"), "<a href=\"\">a</a>");
    assert_eq!(html("[a](b(c)d)"), "<a href=\"b(c)d\">a</a>");
    assert_eq!(
        html(r"[a](\(b\) 'x\'y')"),
        "<a href=\"(b)\" title=\"x'y\">a</a>"
    );
    assert_eq!(
        html("[a](/ü?x=1&y=2)"),
        "<a href=\"/%C3%BC?x=1&amp;y=2\">a</a>"
    );
    assert_eq!(html("[a [b] c](d)"), "<a href=\"d\">a [b] c</a>");
    assert_eq!(html("[a [b](c)](d)"), "[a <a href=\"c\">b</a>](d)");
    assert_eq!(
        html("![a *b* `c`](d \"t\")"),
        "<img src=\"d\" alt=\"a b c\" title=\"t\" />"
    );
    assert_eq!(html("![a ![b](c)](d)"), "<img src=\"d\" alt=\"a b\" />");
    assert_eq!(html("[a](\n/u\n)"), "<a href=\"/u\">a</a>");
    assert_eq!(html("[a] (b)"), "[a] (b)");
    let t = tree("x [a *b*](</u v> \"t\") y");
    let links: Vec<_> = t.links().collect();
    assert_eq!(links.len(), 1);
    let l = &links[0];
    assert_eq!(l.kind, LinkKind::Inline);
    assert!(!l.is_image);
    assert_eq!(l.range, 2..21);
    assert_eq!(l.text_range, 3..8);
    assert_eq!(l.destination, "/u v");
    assert_eq!(l.raw_destination, Some(10..16));
    assert_eq!(l.title, Some("t"));
    assert_eq!(l.label, None);
    assert_eq!(
        tokens("[a](b 'c')"),
        vec![
            tok(K::Marker, "["),
            tok(K::Text, "a"),
            tok(K::Marker, "]"),
            tok(K::Marker, "("),
            tok(K::LinkDestination, "b"),
            tok(K::Whitespace, " "),
            tok(K::LinkTitle, "'c'"),
            tok(K::Marker, ")"),
        ]
    );
}

#[test]
fn reference_links() {
    assert_eq!(html("[foo]"), "<a href=\"/url\" title=\"title\">foo</a>");
    assert_eq!(html("[FOO][]"), "<a href=\"/url\" title=\"title\">FOO</a>");
    assert_eq!(html("[x][Foo]"), "<a href=\"/url\" title=\"title\">x</a>");
    assert_eq!(html("[x][nope]"), "[x][nope]");
    assert_eq!(html("[bar]"), "<a href=\"b%20ar\">bar</a>");
    assert_eq!(html("[ÜNÏ]"), "<a href=\"/u\">ÜNÏ</a>");
    assert_eq!(
        html("![foo]"),
        "<img src=\"/url\" alt=\"foo\" title=\"title\" />"
    );
    assert_eq!(
        html("[foo] [bar]"),
        "<a href=\"/url\" title=\"title\">foo</a> <a href=\"b%20ar\">bar</a>"
    );
    let t = tree("[x][Foo] [nope] [FOO]");
    let links: Vec<_> = t.links().collect();
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].kind, LinkKind::Full);
    assert_eq!(links[0].label, Some("foo"));
    assert_eq!(links[0].label_range, Some(4..7));
    assert_eq!(links[0].raw_destination, None);
    assert_eq!(links[1].kind, LinkKind::Shortcut);
    assert_eq!(links[1].label_range, Some(17..20));
    // Failed lookups are recorded too (cache invalidation).
    let lookups: Vec<_> = t.label_lookups().collect();
    assert_eq!(lookups, vec!["foo", "nope", "foo"]);
    assert_eq!(
        tokens("[x][foo]"),
        vec![
            tok(K::Marker, "["),
            tok(K::Text, "x"),
            tok(K::Marker, "]"),
            tok(K::Marker, "["),
            tok(K::LinkLabel, "foo"),
            tok(K::Marker, "]"),
        ]
    );
}

#[test]
fn autolinks() {
    assert_eq!(
        html("<http://a.b/c?d>"),
        "<a href=\"http://a.b/c?d\">http://a.b/c?d</a>"
    );
    assert_eq!(html("<a@b.cd>"), "<a href=\"mailto:a@b.cd\">a@b.cd</a>");
    assert_eq!(
        html("<http://a/&amp;b>"),
        "<a href=\"http://a/&amp;b\">http://a/&amp;b</a>"
    );
    assert_eq!(
        html("<http://a/\\*>"),
        "<a href=\"http://a/%5C*\">http://a/\\*</a>"
    );
    assert_eq!(html("<m:a b>"), "&lt;m:a b&gt;");
    let t = tree("x <a@b.cd>");
    let l: Vec<_> = t.links().collect();
    assert_eq!(l[0].kind, LinkKind::EmailAutolink);
    assert_eq!(l[0].destination, "a@b.cd");
    assert_eq!(l[0].range, 2..10);
    assert_eq!(l[0].text_range, 3..9);
}

#[test]
fn raw_html() {
    assert_eq!(html("a <b c=\"d\">e</b>"), "a <b c=\"d\">e</b>");
    assert_eq!(
        html("<!-- x -- y --> <?p ?> <!X y> <![CDATA[<]]>"),
        "<!-- x -- y --> <?p ?> <!X y> <![CDATA[<]]>"
    );
    assert_eq!(html("<a b='c\nd'>"), "<a b='c\nd'>");
    assert_eq!(html("<33> <a h*=x>"), "&lt;33&gt; &lt;a h*=x&gt;");
    // A tag across two segments is one node with a token per segment.
    let doc = "x <a\n  b>";
    let t = tree(doc);
    let n: Vec<_> = t.top_level().collect();
    assert_eq!(n[1].kind(), K::RawHtml);
    assert_eq!(n[1].range(), 2..doc.len());
    let kids: Vec<_> = n[1].children().map(|c| (c.kind(), c.range())).collect();
    // The implied line ending between the segments belongs to the first
    // token (and maps to no document bytes).
    assert_eq!(kids, vec![(K::HtmlText, 2..4), (K::HtmlText, 7..9)]);
}

#[test]
fn line_breaks() {
    assert_eq!(html("a\nb"), "a\nb");
    assert_eq!(html("a  \nb"), "a<br />\nb");
    assert_eq!(html("a\\\nb"), "a<br />\nb");
    assert_eq!(html("a \t\nb"), "a\nb");
    assert_eq!(html("a  "), "a");
    assert_eq!(html("a\\"), "a\\");
    assert_eq!(top("a  \nb"), vec![K::Text, K::HardBreak, K::Text]);
    // Segments that include their line endings parse the same.
    let doc = "a  \r\n  b\rc";
    let segs = [0..5, 7..9, 9..10];
    let t = parse_inlines(doc, InlineInput { segments: &segs }, &());
    let mut out = String::new();
    render_inlines_html(&t, &mut out);
    assert_eq!(out, "a<br />\nb\nc");
    let toks: Vec<_> = t.tokens().map(|(k, r)| (k, &doc[r])).collect();
    assert_eq!(
        toks,
        vec![
            (K::Text, "a"),
            (K::HardBreak, "  \r\n"),
            (K::Text, "b"),
            (K::SoftBreak, "\r"),
            (K::Text, "c"),
        ]
    );
}

#[test]
fn leaf_whitespace_is_kept_as_tokens() {
    let doc = "  a  \n";
    let segs = [0..doc.len()];
    let t = parse_inlines(doc, InlineInput { segments: &segs }, &());
    let toks: Vec<_> = t.tokens().map(|(k, r)| (k, &doc[r])).collect();
    assert_eq!(
        toks,
        vec![
            (K::Whitespace, "  "),
            (K::Text, "a"),
            (K::Whitespace, "  \n")
        ]
    );
    let mut out = String::new();
    render_inlines_html(&t, &mut out);
    assert_eq!(out, "a");
}

#[test]
fn container_prefixes_between_segments() {
    // The segments of a paragraph inside a block quote: "> " prefixes are
    // the block side's.
    let doc = "> a [b\n> c](d) `e\n> f`";
    let segs = [2..6, 9..17, 20..22];
    let t = parse_inlines(doc, InlineInput { segments: &segs }, &());
    let mut out = String::new();
    render_inlines_html(&t, &mut out);
    assert_eq!(out, "a <a href=\"d\">b\nc</a> <code>e f</code>");
    let link = t.links().next().expect("link");
    assert_eq!(link.range, 4..14);
    assert_eq!(link.text_range, 5..10);
    assert_eq!(&doc[link.raw_destination.expect("raw")], "d");
    let code = t.code_spans().next().expect("code");
    assert_eq!(code.range, 15..22);
    // Tokens tile the segments exactly.
    let mut tiled = String::new();
    for (_, r) in t.tokens() {
        tiled.push_str(&doc[r]);
    }
    let expected: String = segs.iter().map(|r| &doc[r.clone()]).collect();
    assert_eq!(tiled, expected);
}

#[test]
fn rope_source_matches_str_source() {
    let mut doc = String::new();
    for i in 0..200 {
        doc.push_str(&format!("w{i} *é{i}* [l](/u{i}) `c` &amp; <x{i}> é é\n"));
    }
    let segs = lines(&doc);
    let snapshot = TextSnapshot::new(&doc);
    let a = parse_inlines(doc.as_str(), InlineInput { segments: &segs }, &());
    let b = parse_inlines(&snapshot, InlineInput { segments: &segs }, &());
    let (mut da, mut db) = (String::new(), String::new());
    cst_dump_inlines(&a, &mut da);
    cst_dump_inlines(&b, &mut db);
    assert_eq!(da, db);
}

#[test]
fn bad_segments_do_not_panic() {
    let doc = "aé*b*";
    let reversed = Range { start: 3, end: 1 };
    for segs in [
        vec![0..2],
        vec![2..3],
        vec![reversed],
        vec![0..99],
        vec![50..60],
        vec![4..5, 0..3],
        vec![],
    ] {
        let t = parse_inlines(doc, InlineInput { segments: &segs }, &());
        let mut out = String::new();
        render_inlines_html(&t, &mut out);
        cst_dump_inlines(&t, &mut out);
        let _ = t.links().count() + t.tokens().count();
    }
    // A snapshot hands out the bytes of a range that splits a character.
    let snapshot = TextSnapshot::new(doc);
    let t = parse_inlines(&snapshot, InlineInput { segments: &[2..5] }, &());
    let mut out = String::new();
    render_inlines_html(&t, &mut out);
    assert_eq!(out, "?*b");
}

#[test]
fn plain_text_projection() {
    let mut out = String::new();
    tree("a *b* [c `d`](e) &amp; ![f](g)\nh").plain_text(&mut out);
    assert_eq!(out, "a b c d & f h");
}

#[test]
fn cst_dump_format() {
    let mut out = String::new();
    cst_dump_inlines(&tree("a *b* [c](d)"), &mut out);
    let expected = "\
(Text 0..2 \"a \")
(Emphasis 2..5
  (Marker 2..3 \"*\")
  (Text 3..4 \"b\")
  (Marker 4..5 \"*\"))
(Text 5..6 \" \")
(Link 6..12 inline \"d\"
  (Marker 6..7 \"[\")
  (Text 7..8 \"c\")
  (Marker 8..9 \"]\")
  (Marker 9..10 \"(\")
  (LinkDestination 10..11 \"d\")
  (Marker 11..12 \")\"))
";
    assert_eq!(out, expected);
}

#[test]
fn reference_definitions() {
    let (n, label, def) =
        parse_ref_def("[Foo  Bar]:\n  <a b>\n  'T\\'t &amp;'\nrest").expect("def");
    assert_eq!(n, 35);
    assert_eq!(label, "foo bar");
    assert_eq!(def.destination, "a b");
    assert_eq!(def.title.as_deref(), Some("T't &"));
    assert!(parse_ref_def("[a]: b c").is_none());
    assert!(parse_ref_def("[]: b").is_none());
    assert!(parse_ref_def("[a\\]: b").is_none());
    assert_eq!(parse_ref_def("[a]: /x \"t\"   ").map(|d| d.0), Some(14));
    assert_eq!(normalize_label("  ẞ  Straße\t"), "ss strasse");
}

#[test]
fn deep_nesting_is_iterative() {
    let n = 50_000;
    let doc = format!("{}b{}", "*a **a ".repeat(n), " a** a*".repeat(n));
    let t = tree(&doc);
    assert_eq!(t.emphasis().count(), 2 * n);
    let mut out = String::new();
    render_inlines_html(&t, &mut out);
    assert!(out.starts_with("<em>a <strong>a <em>a <strong>"));
    assert!(out.ends_with("</strong> a</em>"));
    let doc = format!("{}a{}", "![".repeat(n), "](u)".repeat(n));
    let t = tree(&doc);
    assert_eq!(t.links().count(), n);
}
