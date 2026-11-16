// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Math: `$…$`, `` $`…`$ ``, `$$…$$` in paragraphs, `$$` blocks and
//! `\(…\)` / `\[…\]`, per profile switch. Output is comrak's math markup
//! (`<span data-math-style="inline|display">`), which the GFM emitter
//! writes for math nodes.

use gv_syntax::inline::InlineKind;
use gv_syntax::tree::walk::LeafFn;
use gv_syntax::{
    DollarMath, GfmHtmlOptions, Leaf, LeafKind, ParseOptions, ProfileId, SyntaxTree, to_gfm_html,
    walk,
};

fn github() -> ParseOptions {
    ParseOptions::builtin(ProfileId::GfmFile)
}

fn pandoc() -> ParseOptions {
    ParseOptions::builtin(ProfileId::Obsidian)
}

fn brackets() -> ParseOptions {
    ParseOptions {
        math_brackets: true,
        ..github()
    }
}

fn html(md: &str, options: ParseOptions) -> String {
    let tree = SyntaxTree::parse_with(md, options);
    tree.validate(md).expect("valid tree");
    to_gfm_html(&tree, md, GfmHtmlOptions::default())
}

fn inline(tex: &str) -> String {
    format!("<span data-math-style=\"inline\">{tex}</span>")
}

fn display(tex: &str) -> String {
    format!("<span data-math-style=\"display\">{tex}</span>")
}

#[track_caller]
fn para(md: &str, options: ParseOptions, expected: &str) {
    assert_eq!(html(md, options), format!("<p>{expected}</p>\n"), "{md:?}");
}

#[test]
fn off_without_the_switch() {
    let cm = ParseOptions::COMMONMARK;
    para("$x$ and $$y$$", cm, "$x$ and $$y$$");
    para("$`x`$", cm, "$<code>x</code>$");
    assert_eq!(html("$$\nx\n$$\n", cm), "<p>$$\nx\n$$</p>\n");
    para("\\(x\\)", github(), "(x)");
}

#[test]
fn dollar_inline_github() {
    let o = github();
    para("$x$", o, &inline("x"));
    para("a $x^2$ b", o, &format!("a {} b", inline("x^2")));
    // `$5 and $10`: the second `$` follows a space.
    para("$5 and $10", o, "$5 and $10");
    // The closer is not followed by a letter or digit (GitHub).
    para("$a$b", o, "$a$b");
    // The first closer that qualifies closes.
    para("$a$b $c$", o, &inline("a$b $c"));
    para("$a$1", o, "$a$1");
    para("$a$.", o, &format!("{}.", inline("a")));
    // Blank after the opener or before the closer.
    para("$ x$", o, "$ x$");
    para("$x $", o, "$x $");
    // The content is raw TeX: no emphasis, escapes or entities.
    para("$a*b*c\\_d &amp;$", o, &inline("a*b*c\\_d &amp;amp;"));
    // An escaped closer does not close; a doubled backslash does.
    para("$a\\$b$", o, &inline("a\\$b"));
    para("$a\\\\$", o, &inline("a\\\\"));
    // GitHub finds math before escapes: `\$x$` is a backslash and math.
    para("\\$x$", o, &format!("\\{}", inline("x")));
    // Without a closer, `\$` stays an escape.
    para("\\$5", o, "$5");
    // Spans lines of a paragraph.
    para("$a\nb$", o, &inline("a\nb"));
    // Math wins over a later code span, left to right.
    para("$a `b$ c`", o, &format!("{} c`", inline("a `b")));
    para("`$a` b$", o, "<code>$a</code> b$");
}

#[test]
fn dollar_inline_pandoc() {
    let o = pandoc();
    assert_eq!(o.math, DollarMath::Pandoc);
    para("$x$", o, &inline("x"));
    // A letter may follow the closer; a digit may not.
    para("$a$b", o, &format!("{}b", inline("a")));
    para("$a$1", o, "$a$1");
    para("$5 and $10", o, "$5 and $10");
    // `\$` is an escaped dollar.
    para("\\$x$", o, "$x$");
    // No `$`-backtick form: the `$` rules apply to `` $`x`$ ``.
    para("$`x`$", o, &inline("`x`"));
}

#[test]
fn backtick_math() {
    let o = github();
    para("$`x`$", o, &inline("x"));
    para("$``a`b``$", o, &inline("a`b"));
    para("$`x` y", o, "$<code>x</code> y");
    // Without `` `$ ``, the `$` rules apply.
    para("$`x$ y`", o, &format!("{} y`", inline("`x")));
    para("$`x` $", o, "$<code>x</code> $");
}

#[test]
fn display_in_paragraph() {
    let o = github();
    para("a $$x$$ b", o, &format!("a {} b", display("x")));
    para("a $$x$ b", o, "a $$x$ b");
    para("a $$$x$$$ b", o, "a $$$x$$$ b");
    para("a $$x\ny$$ b", o, &format!("a {} b", display("x\ny")));
}

#[test]
fn bracket_math() {
    let o = brackets();
    para("\\(x\\)", o, &inline("x"));
    para("a \\[x\\] b", o, &format!("a {} b", display("x")));
    para("\\(x", o, "(x");
    para("\\[x", o, "[x");
}

#[test]
fn math_blocks() {
    let o = github();
    assert_eq!(
        html("$$\nx^2\n\ny\n$$\n", o),
        format!("<p>{}</p>\n", display("x^2\n\ny"))
    );
    assert_eq!(html("$$x$$\n", o), format!("<p>{}</p>\n", display("x")));
    assert_eq!(
        html("$$ a\nb $$\nafter\n", o),
        format!("<p>{}</p>\n<p>after</p>\n", display("a\nb "))
    );
    // Interrupts a paragraph; unclosed runs to the end of its container.
    assert_eq!(
        html("text\n$$\nx\n", o),
        format!("<p>text</p>\n<p>{}</p>\n", display("x"))
    );
    assert_eq!(
        html("> $$\n> x\n\nafter\n", o),
        format!(
            "<blockquote>\n<p>{}</p>\n</blockquote>\n<p>after</p>\n",
            display("x")
        )
    );
    // Not a block: a later `$$` with text after it, or three dollars.
    assert_eq!(html("$$a$$ b\n", o), format!("<p>{} b</p>\n", display("a")));
    assert_eq!(html("$$$\n", o), "<p>$$$</p>\n");
    // Indented four spaces: code.
    assert_eq!(html("    $$\n", o), "<pre><code>$$\n</code></pre>\n");
}

#[test]
fn math_block_leaf_lines() {
    let md = "$$ a\n  b\n$$\n";
    let tree = SyntaxTree::parse_with(md, github());
    let mut seen = Vec::new();
    walk(
        &tree,
        &mut LeafFn(|leaf: &Leaf<'_>| {
            seen.push((
                leaf.kind,
                leaf.lines
                    .iter()
                    .map(|l| md[l.content.clone()].to_owned())
                    .collect::<Vec<_>>(),
                leaf.start_line,
                leaf.end_line,
            ));
        }),
    );
    assert_eq!(
        seen,
        vec![(
            LeafKind::MathBlock,
            vec!["a".to_owned(), "  b".to_owned()],
            0,
            2
        )]
    );
}

#[test]
fn inline_nodes_expose_their_tex() {
    let md = "a $x+1$ b $$y$$\n";
    let tree = SyntaxTree::parse_with(md, github());
    let mut found = Vec::new();
    walk(
        &tree,
        &mut LeafFn(|leaf: &Leaf<'_>| {
            let inlines = tree.inlines(leaf, md);
            for n in inlines.nodes() {
                if let Some(tex) = n.math() {
                    found.push((n.kind(), tex.to_owned(), n.range()));
                }
            }
            let tokens: Vec<_> = inlines.tokens().map(|(k, _)| k).collect();
            assert!(tokens.contains(&InlineKind::MathText));
        }),
    );
    assert_eq!(
        found,
        vec![
            (InlineKind::InlineMath, "x+1".to_owned(), 2..7),
            (InlineKind::DisplayMath, "y".to_owned(), 10..15),
        ]
    );
}

/// Incremental reparses agree with full parses around math blocks.
#[test]
fn incremental_math_blocks() {
    use gv_syntax::harness::{TextEdit, apply_edits, cst_dump_after_edits_with, cst_dump_with};
    let o = github();
    let cases: &[(&str, std::ops::Range<usize>, &str)] = &[
        ("a\n\nb\n\nc\n", 0..0, "$$\n"),
        ("$$\na\n\nb\n$$\nc\n", 0..2, ""),
        ("$$\na\n$$\nc\n", 5..7, "x"),
        ("x\n$$ a $$\ny\n", 5..6, "$"),
        ("> $$\n> a\n\nb\n", 2..4, "x"),
    ];
    for (text, range, insert) in cases {
        let edits = [TextEdit {
            range: range.clone(),
            insert: (*insert).to_owned(),
        }];
        assert_eq!(
            cst_dump_after_edits_with(text, &edits, o),
            cst_dump_with(&apply_edits(text, &edits), o),
            "{text:?} {range:?} {insert:?}"
        );
    }
}
