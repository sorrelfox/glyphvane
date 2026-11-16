// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! GFM extensions beyond the spec examples: tables, strikethrough, extended
//! autolinks and the tagfilter, with the expected output of cmark-gfm
//! 0.29.0.gfm.13 (checked against comrak where the two agree; where they
//! differ, the case says so). Also: the CommonMark profile leaves all of it
//! alone, and the block view and CST dump of tables.

#![allow(clippy::single_range_in_vec_init)]

use gv_syntax::cst_dump::cst_dump;
use gv_syntax::harness::{normalize_html, render_commonmark_html, render_gfm_html};
use gv_syntax::inline::{InlineInput, InlineKind, parse_inlines_with};
use gv_syntax::{Leaf, LeafKind, ParseOptions, SyntaxTree, walk};

const ALL: &[&str] = &["table", "strikethrough", "autolink", "tagfilter"];

#[track_caller]
fn gfm(md: &str, expected: &str) {
    let ours = render_gfm_html(md, ALL);
    assert_eq!(
        normalize_html(&ours),
        normalize_html(expected),
        "\ninput: {md:?}\nours:\n{ours}"
    );
}

fn table(head: &[&str], rows: &[&[&str]]) -> String {
    let mut s = String::from("<table><thead><tr>");
    for h in head {
        s.push_str(&format!("<th>{h}</th>"));
    }
    s.push_str("</tr></thead>");
    if !rows.is_empty() {
        s.push_str("<tbody>");
        for r in rows {
            s.push_str("<tr>");
            for c in *r {
                s.push_str(&format!("<td>{c}</td>"));
            }
            s.push_str("</tr>");
        }
        s.push_str("</tbody>");
    }
    s.push_str("</table>");
    s
}

#[test]
fn tables_interrupt_paragraphs() {
    gfm(
        "a\n| b |\n| - |\n| c |\n",
        &format!("<p>a</p>{}", table(&["b"], &[&["c"]])),
    );
    gfm("abc\n|---\n", &table(&["abc"], &[]));
    // A failed delimiter row: no table later in the same paragraph.
    gfm(
        "a | b\n--- |\nc | d\n--- | ---\n",
        "<p>a | b\n--- |\nc | d\n--- | ---</p>",
    );
    // List items and setext underlines come first.
    gfm("a\n- | -\n", "<p>a</p><ul><li>| -</li></ul>");
    gfm("a\n---\n", "<h2>a</h2>");
}

#[test]
fn table_ends() {
    let t = table(&["a"], &[]);
    gfm(
        "| a |\n| - |\n    code\n",
        &format!("{t}<pre><code>code\n</code></pre>"),
    );
    gfm(
        "| a |\n| - |\n2. x\n",
        &format!("{t}<ol start=\"2\"><li>x</li></ol>"),
    );
    gfm("| a |\n| - |\n<foo>\n", &format!("{t}<foo>\n"));
    gfm("| a |\n| - |\n|\nx\n", &format!("{t}<p>|\nx</p>"));
    gfm(
        "> | a | b |\n> | - | - |\n> | c | d |\nlazy\n",
        &format!(
            "<blockquote>{}</blockquote><p>lazy</p>",
            table(&["a", "b"], &[&["c", "d"]])
        ),
    );
}

#[test]
fn table_cells() {
    gfm(
        "| a | b |\n|:-|-:|\n| 1 | 2 | 3 |\n| 4 |\n",
        "<table><thead><tr><th align=\"left\">a</th><th align=\"right\">b</th></tr></thead>\
         <tbody><tr><td align=\"left\">1</td><td align=\"right\">2</td></tr>\
         <tr><td align=\"left\">4</td><td align=\"right\"></td></tr></tbody></table>",
    );
    // A pipe after a backslash never splits a cell; the backslash before a
    // pipe is removed before the cell's inlines are parsed (cmark-gfm;
    // comrak keeps `\\|` as an escaped backslash).
    gfm("| a\\\\|b |\n| - |\n", &table(&["a|b"], &[]));
    gfm("| `a\\|b` |\n| - |\n", &table(&["<code>a|b</code>"], &[]));
    gfm(
        "| a |\n| - |\n| www.x.com ~~s~~ |\n",
        &table(
            &["a"],
            &[&["<a href=\"http://www.x.com\">www.x.com</a> <del>s</del>"]],
        ),
    );
}

#[test]
fn tables_in_containers() {
    gfm(
        "- | a |\n  | - |\n  | b |\n\n  x\n",
        &format!("<ul><li>{}<p>x</p></li></ul>", table(&["a"], &[&["b"]])),
    );
    gfm(
        "* | a |\n  |-|\n* | b |\n  |-|\n",
        &format!(
            "<ul><li>{}</li><li>{}</li></ul>",
            table(&["a"], &[]),
            table(&["b"], &[])
        ),
    );
}

/// cmark-gfm leaves the paragraph before a header unparsed for link
/// reference definitions; here they are definitions (decided before the
/// header line is known, and kept).
#[test]
fn definitions_before_a_header() {
    gfm(
        "[x]: /u\n| b |\n| - |\n[x]\n",
        &table(&["b"], &[&["<a href=\"/u\">x</a>"]]),
    );
    // The header line itself is never part of a definition.
    gfm("[x]: /u\n\"t\"\n| - |\n", &table(&["&quot;t&quot;"], &[]));
}

#[test]
fn strikethrough() {
    gfm(
        "~a~ ~~b~~ ~~~c~~~ ~d~~ ~~e~\n",
        "<p><del>a</del> <del>b</del> ~~~c~~~ ~d~~ ~~e~</p>",
    );
    gfm(
        "*~~a~~* ~~*b*~~ ~~**c**~~\n",
        "<p><em><del>a</del></em> <del><em>b</em></del> <del><strong>c</strong></del></p>",
    );
    gfm(
        "**foo ~~bar** baz~~\n",
        "<p><strong>foo ~~bar</strong> baz~~</p>",
    );
    // Double tildes only (the Obsidian rule).
    let options = ParseOptions {
        strikethrough: true,
        ..ParseOptions::COMMONMARK
    };
    let md = "~a~ ~~b~~\n";
    let html = gv_syntax::to_gfm_html(
        &SyntaxTree::parse_with(md, options),
        md,
        gv_syntax::GfmHtmlOptions::default(),
    );
    assert_eq!(html, "<p>~a~ <del>b</del></p>\n");
}

#[test]
fn extended_autolinks() {
    // An underscore in one of the last two labels: no link.
    gfm(
        "www.a.b x www.a_b.c www.a.b_c www.a._b.c\n",
        "<p><a href=\"http://www.a.b\">www.a.b</a> x www.a_b.c www.a.b_c www.a._b.c</p>",
    );
    gfm(
        "[www.x.com] [a](www.y.com) ![www.z.com](q)\n",
        "<p>[www.x.com] <a href=\"www.y.com\">a</a> <img src=\"q\" alt=\"www.z.com\" /></p>",
    );
    gfm(
        "Ahttp://x.com http://x ftp://a.b/c.\n",
        "<p>Ahttp://x.com <a href=\"http://x\">http://x</a> \
         <a href=\"ftp://a.b/c\">ftp://a.b/c</a>.</p>",
    );
    gfm(
        "foo@bar.com mailto:foo@bar.com xmpp:a@b.c/d (a@b.c) foo@bar.com_ a@b@c.d\n",
        "<p><a href=\"mailto:foo@bar.com\">foo@bar.com</a> \
         <a href=\"mailto:foo@bar.com\">mailto:foo@bar.com</a> \
         <a href=\"xmpp:a@b.c/d\">xmpp:a@b.c/d</a> (<a href=\"mailto:a@b.c\">a@b.c</a>) \
         foo@bar.com_ a@<a href=\"mailto:b@c.d\">b@c.d</a></p>",
    );
    gfm(
        "*foo@bar.com* __a__@b.co\n",
        "<p><em><a href=\"mailto:foo@bar.com\">foo@bar.com</a></em> \
         <strong>a</strong>@b.co</p>",
    );
    gfm(
        "`www.x.com` <http://a.b> http://a.b<c\n",
        "<p><code>www.x.com</code> <a href=\"http://a.b\">http://a.b</a> \
         <a href=\"http://a.b\">http://a.b</a>&lt;c</p>",
    );
}

#[test]
fn tagfilter_and_strong_quirk() {
    gfm(
        "<script>x</script>\n\n<p><title>t</title></p>\n",
        "&lt;script>x&lt;/script>\n<p>&lt;title>t&lt;/title></p>",
    );
    gfm("**a **b** c**\n", "<p><strong>a b c</strong></p>");
    gfm(
        "**a *b **c** d* e**\n",
        "<p><strong>a <em>b <strong>c</strong> d</em> e</strong></p>",
    );
}

#[test]
fn commonmark_profile_has_no_extensions() {
    let md = "| a |\n| - |\n~~s~~ www.x.com a@b.co <script>\n\n**a **b** c**\n";
    assert_eq!(
        render_commonmark_html(md),
        "<p>| a |\n| - |\n~~s~~ www.x.com a@b.co <script></p>\n\
         <p><strong>a <strong>b</strong> c</strong></p>\n"
    );
    assert_eq!(render_gfm_html(md, &[]), {
        // cmark-gfm without extensions differs only by the strong quirk.
        "<p>| a |\n| - |\n~~s~~ www.x.com a@b.co <script></p>\n<p><strong>a b c</strong></p>\n"
    });
}

#[test]
fn block_view_and_dump() {
    let md = "Para\n| a | b |\n|:-|-|\n| c |\n\nafter\n";
    let tree = SyntaxTree::parse_with(md, ParseOptions::GFM);
    tree.validate(md).expect("valid");
    let mut leaves: Vec<(LeafKind, Vec<String>, usize, usize)> = Vec::new();
    walk(
        &tree,
        &mut gv_syntax::tree::walk::LeafFn(|l: &Leaf<'_>| {
            let lines = l
                .lines
                .iter()
                .map(|x| md[x.content.clone()].to_owned())
                .collect();
            leaves.push((l.kind, lines, l.start_line, l.end_line));
        }),
    );
    assert_eq!(
        leaves,
        [
            (LeafKind::Paragraph, vec!["Para".to_owned()], 0, 0),
            (
                LeafKind::Table,
                vec![
                    "| a | b |".to_owned(),
                    "|:-|-|".to_owned(),
                    "| c |".to_owned()
                ],
                1,
                3
            ),
            (LeafKind::Paragraph, vec!["after".to_owned()], 5, 5),
        ]
    );
    let dump = cst_dump(&tree, md);
    assert!(dump.contains("(text paragraph table 0..28"), "{dump}");
    assert!(dump.contains("(table-header)"), "{dump}");
    assert!(dump.contains("(marker 15..21 \"|:-|-|\")"), "{dump}");
    // Without tables, the same text is one paragraph.
    let cm = SyntaxTree::parse(md);
    assert!(!cst_dump(&cm, md).contains("table"));
}

#[test]
fn inline_trees() {
    let md = "~~a~~ www.b.c";
    let segments = &[0..md.len()][..];
    let tree = parse_inlines_with(
        md,
        InlineInput { segments },
        &(),
        ParseOptions::GFM.inline(),
    );
    let kinds: Vec<InlineKind> = tree.top_level().map(|n| n.kind()).collect();
    assert_eq!(
        kinds,
        [
            InlineKind::Strikethrough,
            InlineKind::Text,
            InlineKind::Autolink
        ]
    );
    let link = tree.links().next().expect("autolink");
    assert_eq!(link.destination, "http://www.b.c");
    assert_eq!(link.text_range, 6..13);
    // Tokens cover the text.
    let covered: usize = tree.tokens().map(|(_, r)| r.len()).sum();
    assert_eq!(covered, md.len());
}
