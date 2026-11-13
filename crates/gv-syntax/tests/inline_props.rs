// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests of the inline parser (losslessness, well-formed trees, no
//! panics, rope and string sources agree, HTML agrees with comrak outside
//! its known divergences) and linear-time checks on pathological inputs.
//!
//! The linear-time tests compare sizes n and 8n: quadratic behavior shows
//! as a normalized time ratio of 8. The default test uses small sizes and a
//! bound of 3 so it is meaningful in debug builds on a busy machine; the
//! ignored variant uses larger inputs and a bound of 2.5 and is meant for
//! release builds (per-byte cost rises in steps as the working set leaves
//! the CPU caches, so ratios between 1 and 2 are expected there):
//!
//! ```sh
//! cargo test --release -p gv-syntax --test inline_props -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::ops::Range;
use std::time::Instant;

use gv_syntax::harness::normalize_html;
use gv_syntax::inline::{
    InlineInput, InlineTree, RefDef, cst_dump_inlines, parse_inlines, parse_ref_def,
    render_inlines_html,
};
use gv_text::TextSnapshot;
use proptest::prelude::*;

/// Fragments that exercise every construct and its edge cases.
const FRAGMENTS: &[&str] = &[
    "*",
    "_",
    "**",
    "__",
    "***",
    "`",
    "``",
    "[",
    "]",
    "![",
    "](",
    ")",
    "(",
    "<",
    ">",
    "\\",
    "&amp;",
    "&#35;",
    "&#x41;",
    "&nope;",
    "&",
    " ",
    "  ",
    "\t",
    "a",
    "b",
    "foo",
    "é",
    "!",
    "\"",
    "'",
    ".",
    "-",
    "http://x.y",
    "a@b.c",
    "<a href=\"x\">",
    "</a>",
    "<!-- c -->",
    "<?p ?>",
    "<!X y>",
    "<![CDATA[x]]>",
    "\n",
    "  \n",
    "\\\n",
    "\r\n",
    "\r",
    "[foo]",
    "[bar][]",
    "[x][foo]",
    "(/u \"t\")",
    "<http://a.b>",
    "<a@b.cd>",
    "\u{a0}",
    "»",
    "€",
    "%",
    "\"t\"",
    "'t'",
    "(t)",
    "*a*",
    "_a_",
    "[a](b)",
    "![a](b)",
    "\\*",
    "\\[",
    "\\`",
    "x_y",
    "<x:y>",
    "[]",
    "[ ]",
    "`a\nb`",
    "<a\nb='c'>",
    "[a\nb](c)",
    "](\n/u\n'x\ny'\n)",
    "\0",
    "\u{3000}",
    "<!--",
    "-->",
    "<?",
    "?>",
    "<a b=\"",
    "&#0;",
    "ẞ",
    "\u{1F600}",
];

/// [`FRAGMENTS`] without the constructs where comrak 0.59 (like cmark)
/// knowingly differs from the 0.31.2 text: U+0000, CR, `<!` and `<?`
/// constructs.
const COMRAK_FRAGMENTS: &[&str] = &[
    "*",
    "_",
    "**",
    "__",
    "***",
    "`",
    "``",
    "[",
    "]",
    "![",
    "](",
    ")",
    "(",
    "<",
    ">",
    "\\",
    "&amp;",
    "&#35;",
    "&#x41;",
    "&nope;",
    "&",
    " ",
    "  ",
    "\t",
    "a",
    "b",
    "foo",
    "é",
    "!",
    "\"",
    "'",
    ".",
    "-",
    "http://x.y",
    "a@b.c",
    "<a href=\"x\">",
    "</a>",
    "\n",
    "  \n",
    "\\\n",
    "[foo]",
    "[bar][]",
    "[x][foo]",
    "(/u \"t\")",
    "<http://a.b>",
    "<a@b.cd>",
    "\u{a0}",
    "»",
    "€",
    "%",
    "\"t\"",
    "'t'",
    "(t)",
    "*a*",
    "_a_",
    "[a](b)",
    "![a](b)",
    "\\*",
    "\\[",
    "\\`",
    "x_y",
    "<x:y>",
    "[]",
    "[ ]",
    "`a\nb`",
    "<a\nb='c'>",
    "[a\nb](c)",
    "](\n/u\n'x\ny'\n)",
    "\u{3000}",
    "<a b=\"",
    "&#0;",
    "ẞ",
    "\u{1F600}",
];

const DEFS: &[&str] = &["[foo]: /url \"title\"", "[bar]: <b ar>", "[x]: /x 'X'"];

fn refs() -> HashMap<String, RefDef> {
    DEFS.iter()
        .filter_map(|d| parse_ref_def(d))
        .map(|(_, label, def)| (label, def))
        .collect()
}

fn fragments() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(FRAGMENTS), 0..40).prop_map(|v| v.concat())
}

/// How a test lays the content out in the document.
#[derive(Clone, Copy, Debug)]
struct Layout {
    /// Each line carries this container prefix (not part of the segments).
    prefix: &'static str,
    /// Segments include their line endings.
    with_eol: bool,
    /// Leading spaces and tabs of each line are left out of the segments.
    strip_indent: bool,
}

fn layouts() -> impl Strategy<Value = Layout> {
    (
        prop::sample::select(&["", "> ", "  - ", ">\t"][..]),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(prefix, with_eol, strip_indent)| Layout {
            prefix,
            with_eol,
            strip_indent,
        })
}

/// Builds a document from `content` (each line gets the prefix) and the
/// segments of its lines.
fn lay_out(content: &str, layout: Layout) -> (String, Vec<Range<usize>>) {
    let mut doc = String::new();
    let mut segs = Vec::new();
    let mut rest = content;
    loop {
        let (line, eol, more) = match rest.find(['\n', '\r']) {
            Some(i) => {
                let eol = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
                (&rest[..i], &rest[i..i + eol], Some(&rest[i + eol..]))
            }
            None => (rest, "", None),
        };
        doc.push_str(layout.prefix);
        let indent = if layout.strip_indent {
            line.len() - line.trim_start_matches([' ', '\t']).len()
        } else {
            0
        };
        let start = doc.len() + indent;
        doc.push_str(line);
        doc.push_str(eol);
        let end = if layout.with_eol {
            doc.len()
        } else {
            doc.len() - eol.len()
        };
        segs.push(start..end);
        match more {
            Some(m) => rest = m,
            None => break,
        }
    }
    (doc, segs)
}

/// Checks the invariants of a tree parsed from `segs` of `doc`.
fn check_tree(doc: &str, segs: &[Range<usize>], tree: &InlineTree) {
    // Tokens tile the segments, in order.
    let mut tiled = String::new();
    let mut last = 0;
    for (_, r) in tree.tokens() {
        assert!(r.start >= last, "tokens out of order");
        assert!(r.start <= r.end);
        last = r.end;
        tiled.push_str(&doc[r]);
    }
    let expected: String = segs.iter().map(|r| &doc[r.clone()]).collect();
    assert_eq!(tiled, expected, "tokens do not tile the segments");
    // Nodes nest: every child lies inside its parent, and children follow
    // each other.
    for n in tree.nodes() {
        let r = n.range();
        let mut prev_end = r.start;
        for c in n.children() {
            let cr = c.range();
            assert!(
                r.start <= cr.start && cr.end <= r.end,
                "child outside its parent"
            );
            assert!(cr.start >= prev_end, "children overlap");
            prev_end = cr.end;
        }
        assert!(
            n.kind().is_token() || n.children().next().is_some(),
            "empty node"
        );
    }
    let mut html = String::new();
    render_inlines_html(tree, &mut html);
    let mut plain = String::new();
    tree.plain_text(&mut plain);
    let _ = tree.links().count() + tree.code_spans().count() + tree.emphasis().count();
}

fn dump(tree: &InlineTree) -> String {
    let mut out = String::new();
    cst_dump_inlines(tree, &mut out);
    out
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        max_global_rejects: 1 << 16,
        ..ProptestConfig::default()
    })]

    #[test]
    fn lossless_and_well_formed(content in fragments(), layout in layouts()) {
        let (doc, segs) = lay_out(&content, layout);
        let refs = refs();
        let tree = parse_inlines(doc.as_str(), InlineInput { segments: &segs }, &refs);
        check_tree(&doc, &segs, &tree);
        // Deterministic, and a rope source gives the same tree.
        let again = parse_inlines(&TextSnapshot::new(&doc), InlineInput { segments: &segs }, &refs);
        prop_assert_eq!(dump(&tree), dump(&again));
    }

    #[test]
    fn segment_layout_does_not_change_html(content in fragments()) {
        // A leaf never holds a blank line (with one, a CR ending a segment
        // and a next segment of just LF would read as one CRLF).
        let content = without_blank_lines(&content);
        let refs = refs();
        let render = |layout: Layout| {
            let (doc, segs) = lay_out(&content, layout);
            let tree = parse_inlines(doc.as_str(), InlineInput { segments: &segs }, &refs);
            let mut html = String::new();
            render_inlines_html(&tree, &mut html);
            html
        };
        // Raw HTML keeps the line endings of the source when segments
        // include them (as cmark does) and has `\n` when they are implied.
        let unify = |s: String| s.replace("\r\n", "\n").replace('\r', "\n");
        let plain = unify(render(Layout { prefix: "", with_eol: false, strip_indent: true }));
        for prefix in ["", "> "] {
            for with_eol in [false, true] {
                let html = unify(render(Layout { prefix, with_eol, strip_indent: true }));
                prop_assert_eq!(&html, &plain);
            }
        }
    }

    #[test]
    fn arbitrary_text_and_segments_never_panic(
        text in "\\PC{0,200}",
        cuts in prop::collection::vec((0usize..260, 0usize..260), 0..8),
    ) {
        let segs: Vec<Range<usize>> = cuts.iter().map(|&(a, b)| a..b).collect();
        let tree = parse_inlines(text.as_str(), InlineInput { segments: &segs }, &());
        let mut out = String::new();
        render_inlines_html(&tree, &mut out);
        cst_dump_inlines(&tree, &mut out);
        let snapshot = TextSnapshot::new(&text);
        let tree = parse_inlines(&snapshot, InlineInput { segments: &segs }, &());
        render_inlines_html(&tree, &mut out);
        // Valid, ordered, char-aligned segments are tiled exactly.
        let valid: Vec<Range<usize>> = {
            let mut v: Vec<usize> = cuts.iter().flat_map(|&(a, b)| [a, b]).filter(|&i| text.is_char_boundary(i)).collect();
            v.sort_unstable();
            v.dedup();
            v.chunks(2).filter(|c| c.len() == 2).map(|c| c[0]..c[1]).collect()
        };
        let tree = parse_inlines(text.as_str(), InlineInput { segments: &valid }, &());
        check_tree(&text, &valid, &tree);
    }

    #[test]
    fn html_matches_comrak(
        content in prop::collection::vec(prop::sample::select(COMRAK_FRAGMENTS), 0..30).prop_map(|v| v.concat()),
    ) {
        // Skip the other inputs where comrak 0.59 (like cmark) knowingly
        // differs from the 0.31.2 text: backslash + line ending inside
        // `<…>` destinations, several backtick runs (its closer cache),
        // nested links in its output, `<!`/`<?` formed by adjacent
        // fragments.
        let backtick_runs = content.split(|c| c != '`').filter(|r| !r.is_empty()).count();
        prop_assume!(!content.contains("<!") && !content.contains("<?"));
        prop_assume!(!(content.contains("\\\n") && content.contains("](<")));
        prop_assume!(backtick_runs < 3);
        // One paragraph: every line starts with a letter.
        let para: String = content.split('\n').map(|l| format!("a{l}")).collect::<Vec<_>>().join("\n");
        let defs = DEFS.join("\n");
        let doc = format!("{defs}\n\n{para}");
        let mut opts = comrak::Options::default();
        opts.render.r#unsafe = true;
        let expected = comrak::markdown_to_html(&doc, &opts);
        prop_assume!(expected.starts_with("<p>") && expected.matches("<p>").count() == 1);
        prop_assume!(!nested_links(&expected));
        let start = defs.len() + 2;
        let mut segs = Vec::new();
        let mut pos = start;
        for line in para.split('\n') {
            let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
            segs.push(pos + lead..pos + line.len());
            pos += line.len() + 1;
        }
        let tree = parse_inlines(doc.as_str(), InlineInput { segments: &segs }, &refs());
        let mut html = String::from("<p>");
        render_inlines_html(&tree, &mut html);
        html.push_str("</p>\n");
        prop_assert_eq!(normalize_html(&html), normalize_html(&expected));
    }
}

/// `s` with an `x` at the start of every line after the first that holds
/// only spaces and tabs.
fn without_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        let line_end = c == '\n' || (c == '\r' && chars.peek() != Some(&'\n'));
        if line_end {
            let rest: String = chars
                .clone()
                .take_while(|&c| c != '\n' && c != '\r')
                .collect();
            if rest.trim_matches([' ', '\t']).is_empty() {
                out.push('x');
            }
        }
    }
    out
}

/// Whether comrak's output has a link inside a link (comrak resets its
/// "no link openers" flag when a later bracket opens).
fn nested_links(html: &str) -> bool {
    let b = html.as_bytes();
    let mut depth = 0i32;
    for i in 0..b.len() {
        if b[i..].starts_with(b"<a href") {
            depth += 1;
            if depth > 1 {
                return true;
            }
        } else if b[i..].starts_with(b"</a>") {
            depth = (depth - 1).max(0);
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Linear time

type Make = fn(usize) -> String;

/// Pathological inline shapes: cmark's `pathological_tests.py` (the inline
/// ones) plus raw HTML, entity, autolink and label shapes.
const SHAPES: &[(&str, Make)] = &[
    ("nested strong emph", |n| {
        format!("{}b{}", "*a **a ".repeat(n), " a** a*".repeat(n))
    }),
    ("emph closers, no openers", |n| "a_ ".repeat(n)),
    ("emph openers, no closers", |n| "_a ".repeat(n)),
    ("link closers, no openers", |n| "a]".repeat(n)),
    ("link openers, no closers", |n| "[a".repeat(n)),
    ("mismatched openers and closers", |n| "*a_ ".repeat(n)),
    ("openers and closers multiple of 3", |n| {
        format!("a**b{}", "c* ".repeat(n))
    }),
    ("link openers and emph closers", |n| "[ a_".repeat(n)),
    ("[ (](", |n| "[ (](".repeat(n)),
    ("![[]()", |n| "![[]()".repeat(n)),
    ("nested brackets", |n| {
        format!("{}a{}", "[".repeat(n), "]".repeat(n))
    }),
    ("nested images", |n| {
        format!("{}a{}", "![".repeat(n), "](u)".repeat(n))
    }),
    ("backticks", |n| {
        (1..n)
            .map(|i| format!("e{}", "`".repeat(i % 300 + 1)))
            .collect()
    }),
    ("unclosed links A", |n| "[a](<b".repeat(n)),
    ("unclosed links B", |n| "[a](b".repeat(n)),
    ("unclosed comments", |n| format!("</{}", "<!--".repeat(n))),
    ("unclosed processing instructions", |n| "a <?".repeat(n)),
    ("unclosed declarations", |n| "a <!A ".repeat(n)),
    ("unclosed CDATA", |n| "a <![CDATA[".repeat(n)),
    ("unclosed attribute quotes", |n| {
        "<a b=\"x' <c d='".repeat(n)
    }),
    ("many attributes, no >", |n| {
        format!("<a{}", " b=c".repeat(n))
    }),
    ("entity-like", |n| {
        "&a&#&#x&#12345678;&abcdefghijklmnopqrstuvwxyzabcdefgh;".repeat(n)
    }),
    ("autolink-like", |n| "<a:b <a@b <ab: <a.b@c-.d ".repeat(n)),
    ("long labels", |n| format!("[{}]", "x ".repeat(n))),
    ("many shortcut labels", |n| {
        "[x x x x x x x x x x x] ".repeat(n)
    }),
    ("full reference attempts", |n| "[a][b c [".repeat(n)),
    ("rule of 3 runs", |n| "a***b**c*".repeat(n)),
    ("alternating delimiters", |n| "*_".repeat(n)),
    ("line breaks", |n| "a  \nb\\\nc\n".repeat(n)),
    ("code spans", |n| "`a` ``b`` ".repeat(n)),
    ("inline links", |n| "[a](b 'c') ".repeat(n)),
    ("nested parentheses in destinations", |n| {
        "[a](((((((((((((((((((((((((((((((((b".repeat(n)
    }),
];

/// Best-of-`runs` time to parse `doc` as one paragraph.
fn time_parse(doc: &str, runs: usize) -> f64 {
    let segs = lines(doc);
    let mut best = f64::MAX;
    for _ in 0..runs {
        let t = Instant::now();
        let tree = parse_inlines(doc, InlineInput { segments: &segs }, &());
        std::hint::black_box(&tree);
        best = best.min(t.elapsed().as_secs_f64());
    }
    best
}

fn lines(doc: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in doc.split_inclusive('\n') {
        out.push(start..start + line.trim_end_matches('\n').len());
        start += line.len();
    }
    if out.is_empty() {
        out.push(0..0);
    }
    out
}

/// Runs every shape at sizes n and `factor`·n; returns the failures.
fn check_linear(base_bytes: usize, factor: usize, max_ratio: f64) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, make) in SHAPES {
        let unit = make(1).len().max(1);
        let n = (base_bytes / unit).max(1);
        let (small, big) = (make(n), make(n * factor));
        let t_small = time_parse(&small, 3).max(1e-6);
        let t_big = time_parse(&big, 3);
        let ratio = (t_big / t_small) / (big.len() as f64 / small.len() as f64);
        println!(
            "{name:36} {:>9} B {:8.3} ms   {:>9} B {:8.3} ms   ratio {ratio:5.2}",
            small.len(),
            t_small * 1e3,
            big.len(),
            t_big * 1e3
        );
        if ratio > max_ratio {
            failures.push(format!("{name}: normalized ratio {ratio:.2}"));
        }
    }
    failures
}

#[test]
fn pathological_shapes_are_linear() {
    // Quadratic behavior shows as a normalized ratio of 8; allow 3 for noise.
    let failures = check_linear(20_000, 8, 3.0);
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn pathological_shapes_are_linear_release() {
    let failures = check_linear(50_000, 8, 2.5);
    assert!(failures.is_empty(), "{failures:#?}");
}
