// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Obsidian Flavored Markdown with the Obsidian profile's switches:
//! wikilinks, embeds, callouts, highlights, comments, tags, block ids and
//! inline footnotes. One fixture per construct of the syntax coverage list,
//! checked through the HTML emitter, the block views and the inline
//! accessors; and the same text with CommonMark, where all of it is text.

use gv_syntax::inline::{InlineKind, image_size, is_tag_char};
use gv_syntax::{
    CalloutFold, Container, ContainerKind, Leaf, LeafKind, ParseOptions, ProfileId, SyntaxTree,
    Visitor, to_gfm_html, walk,
};

fn options() -> ParseOptions {
    ParseOptions::builtin(ProfileId::Obsidian)
}

fn tree(md: &str) -> SyntaxTree {
    let tree = SyntaxTree::parse_with(md, options());
    tree.validate(md).expect("valid tree");
    tree
}

fn ob(md: &str) -> String {
    to_gfm_html(&tree(md), md, gv_syntax::GfmHtmlOptions::default())
}

#[track_caller]
fn html(md: &str, want: &str) {
    assert_eq!(ob(md), want, "{md:?}");
}

/// The containers and leaves of `md`, one line each.
fn outline(md: &str) -> Vec<String> {
    struct V<'a>(&'a str, Vec<String>);
    impl Visitor for V<'_> {
        fn enter(&mut self, c: &Container) {
            let ty = c.callout_type().map(|r| self.0[r].to_owned());
            self.1.push(match c.kind {
                ContainerKind::Callout { fold } => {
                    format!("callout {} {}", ty.unwrap_or_default(), fold.name())
                }
                k => format!("{k:?}"),
            });
        }
        fn leaf(&mut self, l: &Leaf<'_>) {
            let text: Vec<&str> = l.lines.iter().map(|x| &self.0[x.content.clone()]).collect();
            self.1.push(format!("{:?} {:?}", l.kind, text.join("|")));
        }
    }
    let mut v = V(md, Vec::new());
    walk(&tree(md), &mut v);
    v.1
}

/// The inline nodes of the first paragraph-like leaf, as `Kind:text`.
fn inlines(md: &str) -> Vec<String> {
    let t = tree(md);
    let mut out = Vec::new();
    struct V<'a>(&'a SyntaxTree, &'a str, &'a mut Vec<String>);
    impl Visitor for V<'_> {
        fn leaf(&mut self, l: &Leaf<'_>) {
            if !self.2.is_empty() || l.kind == LeafKind::Table {
                return;
            }
            let it = self.0.inlines(l, self.1);
            for n in it.nodes() {
                if !n.kind().is_token() || matches!(n.kind(), InlineKind::Text) {
                    self.2.push(format!("{}:{}", n.kind().name(), n.text()));
                }
            }
        }
    }
    walk(&t, &mut V(&t, md, &mut out));
    out
}

#[test]
fn wikilinks() {
    html(
        "[[Note]] [[Note.md]] [[Folder/Note]]\n",
        "<p><a href=\"Note\" class=\"internal-link\">Note</a> <a href=\"Note.md\" class=\"internal-link\">Note.md</a> <a href=\"Folder/Note\" class=\"internal-link\">Folder/Note</a></p>\n",
    );
    // Heading links, chains, block links, display text.
    html(
        "[[#H]] [[Note#H1#H2]] [[Note#^37066d]] [[Note|Text]]\n",
        "<p><a href=\"#H\" class=\"internal-link\">H</a> <a href=\"Note#H1#H2\" class=\"internal-link\">Note &gt; H1 &gt; H2</a> <a href=\"Note#%5E37066d\" class=\"internal-link\">Note &gt; ^37066d</a> <a href=\"Note\" class=\"internal-link\">Text</a></p>\n",
    );
    let t = tree("a [[Note#H^b|Alias]] b\n");
    let mut links = Vec::new();
    walk(
        &t,
        &mut gv_syntax::tree::walk::LeafFn(|l: &Leaf<'_>| {
            let it = t.inlines(l, "a [[Note#H^b|Alias]] b\n");
            for w in it.wikilinks() {
                links.push((
                    w.embed,
                    w.inner.to_string(),
                    w.path().to_owned(),
                    w.anchor().map(str::to_owned),
                    w.alias.map(str::to_owned),
                    w.range.clone(),
                    w.target_range.clone(),
                ));
            }
        }),
    );
    assert_eq!(
        links,
        [(
            false,
            "Note#H^b|Alias".to_owned(),
            "Note".to_owned(),
            Some("H^b".to_owned()),
            Some("Alias".to_owned()),
            2..20,
            4..12
        )]
    );
    // No line breaks, no `[[` inside, not empty; the latest `[[` wins.
    html("[[a\nb]]\n", "<p>[[a\nb]]</p>\n");
    html("[[]] [[ ]]\n", "<p>[[]] [[ ]]</p>\n");
    html(
        "[[a [[b]]\n",
        "<p>[[a <a href=\"b\" class=\"internal-link\">b</a></p>\n",
    );
    // Wikilinks come before link brackets and code spans that start later.
    html(
        "[x [[a]] y](u) [[`c`]]\n",
        "<p><a href=\"u\">x <a href=\"a\" class=\"internal-link\">a</a> y</a> <a href=\"%60c%60\" class=\"internal-link\">`c`</a></p>\n",
    );
    html("`[[a]]`\n", "<p><code>[[a]]</code></p>\n");
    // `\|` in a table cell.
    assert!(
        ob("| a |\n| -- |\n| [[x\\|y]] |\n")
            .contains("<td><a href=\"x\" class=\"internal-link\">y</a></td>")
    );
    // CommonMark: text (a shortcut reference without a definition).
    let cm = SyntaxTree::parse("[[Note]]\n");
    assert_eq!(
        to_gfm_html(&cm, "[[Note]]\n", gv_syntax::GfmHtmlOptions::default()),
        "<p>[[Note]]</p>\n"
    );
}

#[test]
fn embeds() {
    html(
        "![[Note]] ![[Note#H]] ![[Note#^id]] ![[img.png|100x145]]\n",
        "<p><span class=\"internal-embed\" src=\"Note\"></span> <span class=\"internal-embed\" src=\"Note#H\"></span> <span class=\"internal-embed\" src=\"Note#^id\"></span> <span class=\"internal-embed\" src=\"img.png\"></span></p>\n",
    );
    assert_eq!(inlines("![[img.png|100]]\n"), ["Embed:![[img.png|100]]"]);
    assert_eq!(image_size("100"), Some((100, None)));
    assert_eq!(image_size("100x145"), Some((100, Some(145))));
    assert_eq!(image_size("Text"), None);
    assert_eq!(image_size("100x"), None);
    // An image with brackets that are no embed stays an image.
    html("![a](b.png)\n", "<p><img src=\"b.png\" alt=\"a\" /></p>\n");
}

#[test]
fn callouts() {
    assert_eq!(
        outline("> [!info] Optional *title*\n> body\n"),
        [
            "callout info ",
            "CalloutTitle \"Optional *title*\"",
            "Paragraph \"body\""
        ]
    );
    html(
        "> [!faq]- Folded\n> body\n",
        "<div class=\"callout\" data-callout=\"faq\" data-callout-fold=\"-\">\n<div class=\"callout-title\">Folded</div>\n<p>body</p>\n</div>\n",
    );
    assert_eq!(
        outline("> [!faq]+\n> body\n"),
        ["callout faq +", "Paragraph \"body\""]
    );
    // Title-only callouts, case-insensitive types, unknown types.
    assert_eq!(
        outline("> [!NOTE] Only a title\n"),
        ["callout NOTE ", "CalloutTitle \"Only a title\""]
    );
    assert_eq!(
        outline("> [!custom-type]\n> x\n")[0],
        "callout custom-type "
    );
    // Nested to any depth, and in lists.
    assert_eq!(
        outline("> [!note] A\n> > [!todo] B\n> > > [!bug]\n> > > c\n"),
        [
            "callout note ",
            "CalloutTitle \"A\"",
            "callout todo ",
            "CalloutTitle \"B\"",
            "callout bug ",
            "Paragraph \"c\""
        ]
    );
    assert_eq!(
        outline("- > [!tip] In a list\n")[..3],
        [
            "BulletList { marker: 45, tight: true }",
            "Item",
            "callout tip "
        ]
    );
    // The title is one line: the next line is the body, not a lazy
    // continuation of it.
    assert_eq!(
        outline("> [!note] T\nlazy\n"),
        ["callout note ", "CalloutTitle \"T\"", "Paragraph \"lazy\""]
    );
    // `[!]`, and text before the marker, make block quotes.
    assert_eq!(outline("> [!] x\n")[0], "BlockQuote");
    assert_eq!(outline("> a [!note]\n")[0], "BlockQuote");
    let fold = |md: &str| match tree(md).events().iter().find_map(|(_, e)| match e {
        gv_syntax::Event::Open {
            kind: gv_syntax::BlockKind::Callout { fold },
            ..
        } => Some(*fold),
        _ => None,
    }) {
        Some(f) => f,
        None => panic!("no callout in {md:?}"),
    };
    assert_eq!(fold("> [!a]-\n"), CalloutFold::Closed);
    assert_eq!(fold("> [!a]+ t\n"), CalloutFold::Open);
    assert_eq!(fold("> [!a] t\n"), CalloutFold::None);
    // GitHub alerts are not Obsidian callouts: another profile.
    let gfm = SyntaxTree::parse_with("> [!note] t\n", ParseOptions::builtin(ProfileId::GfmFile));
    assert!(!gfm.events().iter().any(|(_, e)| matches!(
        e,
        gv_syntax::Event::Open {
            kind: gv_syntax::BlockKind::Callout { .. },
            ..
        }
    )));
}

#[test]
fn highlights() {
    html(
        "==mark== =no= ===no=== ==🔴red==\n",
        "<p><mark>mark</mark> =no= ===no=== <mark>🔴red</mark></p>\n",
    );
    html("==a *b* c==\n", "<p><mark>a <em>b</em> c</mark></p>\n");
    html("== a ==\n", "<p>== a ==</p>\n");
}

#[test]
fn comments() {
    html("a %%hidden%% b\n", "<p>a  b</p>\n");
    // Inline: both markers on one line; exactly two `%`.
    html("a %%x\ny%% b\n", "<p>a %%x\ny%% b</p>\n");
    html("a %%%x%%% b\n", "<p>a %%%x%%% b</p>\n");
    // Block: from a line starting with `%%` to the next `%%`, across blank
    // lines.
    html(
        "before\n\n%%\nhidden\n\nstill\n%%\nafter\n",
        "<p>before</p>\n<p>after</p>\n",
    );
    assert_eq!(outline("%% a\nb\nc %%\n"), ["Comment \"a|b|c \""]);
    // Unclosed: the rest of the container.
    html("a\n\n%%\nb\n\nc\n", "<p>a</p>\n");
    html(
        "> %%\n> b\n\nc\n",
        "<blockquote>\n</blockquote>\n<p>c</p>\n",
    );
    // `%%%%` is no block comment (the 1.13 fix).
    html("%%%%\nx\n", "<p>%%%%\nx</p>\n");
    // In code they are text.
    html("`%%a%%`\n", "<p><code>%%a%%</code></p>\n");
}

#[test]
fn tags() {
    html(
        "#tag #a/b x#no #123 #1a (#p) #über #🙂x\n",
        "<p><a href=\"#tag\" class=\"tag\">#tag</a> <a href=\"#a/b\" class=\"tag\">#a/b</a> x#no #123 <a href=\"#1a\" class=\"tag\">#1a</a> (#p) <a href=\"#%C3%BCber\" class=\"tag\">#über</a> <a href=\"#%F0%9F%99%82x\" class=\"tag\">#🙂x</a></p>\n",
    );
    assert_eq!(inlines("#a-b_c, d\n"), ["Tag:#a-b_c", "Text:, d"]);
    assert!(is_tag_char('é') && is_tag_char('🙂') && is_tag_char('/'));
    assert!(!is_tag_char('.') && !is_tag_char('…') && !is_tag_char(' ') && !is_tag_char('#'));
    // A heading is no tag.
    assert_eq!(
        outline("# Title #t\n"),
        ["Heading { level: 1, setext: false } \"Title #t\""]
    );
}

#[test]
fn block_ids() {
    assert_eq!(
        inlines("Some text ^abc-1\n"),
        ["Text:Some text", "BlockId:^abc-1"]
    );
    html("para ^abc-1\n", "<p>para</p>\n");
    html("- item ^x\n", "<ul>\n<li>item</li>\n</ul>\n");
    // On its own line (after a list, quote or table: its own paragraph).
    assert_eq!(inlines("^lonely\n"), ["BlockId:^lonely"]);
    // Not in the middle, not without a space, not with other characters.
    assert_eq!(inlines("a ^b c\n"), ["Text:a ^b c"]);
    assert_eq!(inlines("a^b\n"), ["Text:a^b"]);
    assert_eq!(inlines("a ^b_c\n"), ["Text:a ^b_c"]);
    // Not in headings.
    assert_eq!(inlines("# H ^x\n"), ["Text:H ^x"]);
}

#[test]
fn inline_footnotes() {
    html(
        "note^[inline *f*] end\n",
        "<p>note<sup class=\"footnote-inline\">inline <em>f</em></sup> end</p>\n",
    );
    html("a ^[unclosed\n", "<p>a ^[unclosed</p>\n");
    html(
        "^[a [b](c) d]\n",
        "<p><sup class=\"footnote-inline\">a <a href=\"c\">b</a> d</sup></p>\n",
    );
}

#[test]
fn tasks_and_footnotes_keep_working() {
    html(
        "- [?] q\n- [-] c\n",
        "<ul>\n<li><input type=\"checkbox\" checked=\"\" disabled=\"\" /> q</li>\n<li><input type=\"checkbox\" checked=\"\" disabled=\"\" /> c</li>\n</ul>\n",
    );
}

#[test]
fn commonmark_reads_it_all_as_text() {
    let md = "==a== %%b%% #c ^[d] e ^f\n";
    let cm = SyntaxTree::parse(md);
    assert_eq!(
        to_gfm_html(&cm, md, gv_syntax::GfmHtmlOptions::default()),
        "<p>==a== %%b%% #c ^[d] e ^f</p>\n"
    );
}
