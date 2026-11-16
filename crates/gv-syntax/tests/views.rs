// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Unit tests of the views and options added on top of the block pass:
//! front matter ([`ParseOptions`]), range walks, the block index, inline
//! text projections, located link reference definitions and composed
//! reparse reports. The randomized counterparts are in `incremental.rs`.

// Segment lists of a single range are what the inline tests need.
#![allow(clippy::single_range_in_vec_init)]

mod support;

use std::ops::Range;
use std::sync::Arc;

use gv_syntax::inline::{
    InlineInput, InlineKind, TextProjection, parse_inlines, parse_ref_def, parse_ref_def_spans,
};
use gv_syntax::{
    BlockKind, Changes, FrontMatterKind, FrontMatterOptions, Leaf, LeafKind, ParseOptions,
    SyntaxTree, Visitor, to_commonmark_html,
};
use support::edits::{Mode, apply, check_edit};
use support::views::{filtered_walk, record_walk_range};

fn extended() -> ParseOptions {
    ParseOptions::builtin(gv_syntax::ProfileId::Extended)
}

/// Leaves of a full walk: (kind, the content of their lines).
fn leaves(tree: &SyntaxTree, text: &str) -> Vec<(LeafKind, Vec<String>)> {
    let mut out = Vec::new();
    gv_syntax::walk(
        tree,
        &mut gv_syntax::tree::walk::LeafFn(|l: &Leaf<'_>| {
            let lines = l
                .lines
                .iter()
                .map(|x| text[x.content.clone()].to_owned())
                .collect();
            out.push((l.kind, lines));
        }),
    );
    out
}

fn parsed(text: &str, opts: ParseOptions) -> SyntaxTree {
    let tree = SyntaxTree::parse_with(text, opts);
    tree.validate(text).expect("valid tree");
    assert_eq!(gv_syntax::cst_dump::cst_text(&tree, text), text.as_bytes());
    tree
}

const YAML: LeafKind = LeafKind::FrontMatter {
    kind: FrontMatterKind::Yaml,
};
const TOML: LeafKind = LeafKind::FrontMatter {
    kind: FrontMatterKind::Toml,
};

#[test]
fn front_matter_block() {
    let text = "---\ntitle: x\ntags: [a]\n---\n# H\n";
    let tree = parsed(text, extended());
    let l = leaves(&tree, text);
    assert_eq!(l[0], (YAML, vec!["title: x".into(), "tags: [a]".into()]));
    assert_eq!(
        l[1].0,
        LeafKind::Heading {
            level: 1,
            setext: false
        }
    );
    let fm = &tree.blocks()[0];
    assert_eq!(
        fm.kind,
        BlockKind::FrontMatter {
            kind: FrontMatterKind::Yaml
        }
    );
    assert_eq!(fm.range, 0..27);
    let dump = gv_syntax::cst_dump::cst_dump(&tree, text);
    assert!(dump.contains("(front-matter yaml 0..27"), "{dump}");
    assert!(dump.contains("(marker 23..26 \"---\")"), "{dump}");
    // The emitter renders nothing for it.
    assert_eq!(to_commonmark_html(&tree, text), "<h1>H</h1>\n");
}

#[test]
fn front_matter_is_off_in_commonmark() {
    let text = "---\ntitle: x\n---\n";
    let cm = parsed(text, ParseOptions::default());
    assert_eq!(cm.options(), ParseOptions::COMMONMARK);
    let kinds: Vec<LeafKind> = leaves(&cm, text).into_iter().map(|l| l.0).collect();
    assert_eq!(
        kinds,
        [
            LeafKind::ThematicBreak,
            LeafKind::Heading {
                level: 2,
                setext: true
            }
        ]
    );
    assert_eq!(
        to_commonmark_html(&cm, text),
        gv_syntax::harness::render_commonmark_html(text)
    );
}

#[test]
fn front_matter_variants() {
    let ext = extended();
    let gfm = ParseOptions::builtin(gv_syntax::ProfileId::GfmFile);
    let first = |text: &str, opts| leaves(&parsed(text, opts), text).first().cloned();
    // Closing with `...`, CRLF line endings, trailing spaces and tabs.
    assert_eq!(
        first("--- \t\r\na: 1\r\n... \r\nx\r\n", ext),
        Some((YAML, vec!["a: 1".into()]))
    );
    // Empty front matter, a closing line without a line ending.
    assert_eq!(first("---\n---", ext), Some((YAML, vec![])));
    // After a BOM.
    let bom = "\u{feff}---\na\n---\n";
    assert_eq!(first(bom, ext), Some((YAML, vec!["a".into()])));
    assert_eq!(parsed(bom, ext).blocks()[0].range.start, 3);
    // TOML only where enabled.
    assert_eq!(
        first("+++\na = 1\n+++\n", ext),
        Some((TOML, vec!["a = 1".into()]))
    );
    assert_eq!(
        first("+++\na = 1\n+++\n", gfm).map(|l| l.0),
        Some(LeafKind::Paragraph)
    );
    // A TOML block is not closed by `---` (here a setext underline), a YAML
    // block not by `+++`.
    assert_eq!(
        first("+++\na\n---\n", ext).map(|l| l.0),
        Some(LeafKind::Heading {
            level: 2,
            setext: true
        })
    );
    assert_eq!(
        first("---\na\n+++\n", ext).map(|l| l.0),
        Some(LeafKind::ThematicBreak)
    );
    // Only at byte 0, exactly three characters, at column 0, closed.
    for text in [
        "\n---\na\n---\n",
        " ---\na\n---\n",
        "----\na\n----\n",
        "---\na: 1\n",
    ] {
        let k = first(text, ext).map(|l| l.0);
        assert!(
            !matches!(k, Some(LeafKind::FrontMatter { .. })),
            "{text:?}: {k:?}"
        );
    }
    // The closing line must start at column 0; content lines are literal.
    assert_eq!(
        first("---\n  ---\n# not a heading\n---\n", ext),
        Some((YAML, vec!["  ---".into(), "# not a heading".into()]))
    );
    assert_eq!(
        FrontMatterOptions::YAML,
        ParseOptions::builtin(gv_syntax::ProfileId::Obsidian).front_matter
    );
}

/// Reparses `text` after replacing `range` with `insert` and checks the
/// result against a full parse.
fn edit(
    tree: &SyntaxTree,
    text: &str,
    range: Range<usize>,
    insert: &str,
) -> (SyntaxTree, String, Changes) {
    let (new_text, change) = apply(text, range.start, range.end, insert);
    for mode in [Mode::Direct, Mode::Sliced(5)] {
        check_edit(tree, text, &new_text, change, mode).expect("incremental == full");
    }
    let (new, changes) = tree.reparse_full(new_text.as_str(), change);
    (new, new_text, changes)
}

#[test]
fn front_matter_incremental() {
    let body = "para\n\n- a\n- b\n\n".repeat(50);
    let text = format!("---\ntitle: x\nmore: y\n---\n{body}");
    let tree = parsed(&text, extended());
    // Typing inside the front matter converges at once.
    let (t1, s1, c1) = edit(&tree, &text, 9..9, "z");
    assert!(c1.converged && c1.lines_parsed <= 3, "{c1:?}");
    // Breaking the closing line: no front matter any more, from line 0.
    let (t2, s2, c2) = edit(&t1, &s1, 22..23, "");
    assert_eq!(c2.restart_line, 0, "{c2:?}");
    assert_eq!(leaves(&t2, &s2)[0].0, LeafKind::ThematicBreak);
    // Restoring it brings the front matter back (a closing line under a
    // rejected opening line).
    let (t3, s3, c3) = edit(&t2, &s2, 22..22, "-");
    assert_eq!(c3.restart_line, 0, "{c3:?}");
    assert_eq!(leaves(&t3, &s3)[0].0, YAML);
    // A closing line far below a rejected opening line.
    let text = format!("---\n{body}");
    let tree = parsed(&text, extended());
    assert_eq!(leaves(&tree, &text)[0].0, LeafKind::ThematicBreak);
    let at = text.len() - 1;
    let (t4, s4, _) = edit(&tree, &text, at..at, "\n...\n");
    assert_eq!(leaves(&t4, &s4)[0].0, YAML);
    // Removing the opening line's last dash: the old closing line now
    // underlines a setext heading.
    let (t5, s5, _) = edit(&t3, &s3, 2..3, "");
    assert_eq!(
        leaves(&t5, &s5)[0].0,
        LeafKind::Heading {
            level: 2,
            setext: true
        }
    );
    // A second closing line earlier inside the front matter.
    let (t6, s6, _) = edit(&t3, &s3, 4..4, "...\n");
    assert_eq!(leaves(&t6, &s6)[0], (YAML, vec![]));
}

/// A document with nested containers of every kind.
fn nested_doc() -> String {
    let mut s = String::from("# Title\n\nIntro *text*.\n\n");
    for i in 0..30 {
        s.push_str(&format!(
            "- item {i}\n  continued\n\n  > quote {i}\n  > - inner\n  >   lazy\nmore\n"
        ));
        if i % 7 == 0 {
            s.push_str("\n```\ncode\n\n```\n");
        }
    }
    s.push_str("\n1. a\n2. b\n\n[ref]: /url\n");
    s
}

#[test]
fn range_walks_match_the_full_walk() {
    let text = nested_doc();
    let tree = parsed(&text, ParseOptions::default());
    let n = text.len();
    let mut ranges = vec![0..0, 0..n, n..n, n - 1..n, 3..3];
    for start in (0..n).step_by(37) {
        ranges.push(start..start);
        ranges.push(start..start + 1);
        ranges.push(start..(start + 120).min(n));
    }
    for r in ranges {
        assert_eq!(
            record_walk_range(&tree, r.clone()),
            filtered_walk(&tree, r.clone()),
            "range {r:?}"
        );
    }
}

#[test]
fn range_walks_read_little_of_a_big_document() {
    // One huge tight list: the classic case where a walk from the top is
    // O(document).
    let mut text = String::new();
    for i in 0..20_000 {
        text.push_str(&"  ".repeat(i % 3));
        text.push_str(&format!("- item {i}\n"));
    }
    let tree = parsed(&text, ParseOptions::default());
    let mid = text.len() / 2;
    struct Count(usize, usize);
    impl Visitor for Count {
        fn enter(&mut self, _: &gv_syntax::Container) {
            self.0 += 1;
        }
        fn leaf(&mut self, _: &Leaf<'_>) {
            self.1 += 1;
        }
    }
    let mut c = Count(0, 0);
    tree.walk_range(mid..mid + 30, &mut c);
    assert!(
        c.0 <= 16 && c.1 <= 5,
        "entered {} containers, {} leaves",
        c.0,
        c.1
    );
    assert_eq!(
        record_walk_range(&tree, mid..mid + 30),
        filtered_walk(&tree, mid..mid + 30)
    );
}

#[test]
fn block_index() {
    let text = "> - a\n>   b\n\n# H\n";
    let tree = parsed(text, ParseOptions::default());
    let index = tree.block_index();
    assert!(Arc::ptr_eq(&index, &tree.block_index()), "cached");
    assert_eq!(index.len(), tree.blocks().len());
    let path: Vec<BlockKind> = index.path_at(9).iter().map(|b| b.kind).collect();
    assert_eq!(
        path,
        [
            BlockKind::BlockQuote,
            BlockKind::BulletList { marker: b'-' },
            BlockKind::Item,
            BlockKind::Text
        ]
    );
    let text_block = index.path_at(9)[3].clone();
    assert_eq!(index.get(text_block.id), Some(&text_block));
    assert_eq!(
        index.parent(text_block.id).map(|b| b.kind),
        Some(BlockKind::Item)
    );
    assert!(
        index.path_at(12).is_empty(),
        "the blank line belongs to no block"
    );
    let hit: Vec<BlockKind> = index.intersecting(10..15).iter().map(|b| b.kind).collect();
    assert_eq!(
        hit,
        [
            BlockKind::BlockQuote,
            BlockKind::BulletList { marker: b'-' },
            BlockKind::Item,
            BlockKind::Text,
            BlockKind::AtxHeading { level: 1 }
        ]
    );
}

fn inline(src: &str) -> gv_syntax::inline::InlineTree {
    let mut segments = Vec::new();
    let mut start = 0;
    for line in src.split_inclusive('\n') {
        segments.push(start..start + line.trim_end_matches('\n').len());
        start += line.len();
    }
    parse_inlines(
        src,
        InlineInput {
            segments: &segments,
        },
        &(),
    )
}

#[test]
fn text_projections() {
    let p = |src: &str| {
        let t = inline(src);
        (t.text_content(), t.collect_text())
    };
    assert_eq!(p("Foo *bar* `x`").0, "Foo bar x");
    assert_eq!(
        p("a ![img *x*](p.png) b"),
        ("a  b".into(), "a img x b".into())
    );
    assert_eq!(
        p("Foo <b>Bar</b> &amp; \\* [l](u \"t\")"),
        ("Foo Bar & * l".into(), "Foo Bar & * l".into())
    );
    assert_eq!(
        p("<https://x.org/&amp;> <a@b.c>").0,
        "https://x.org/& a@b.c"
    );
    assert_eq!(p("a\nb"), ("a\nb".into(), "a b".into()));
    assert_eq!(p("a  \nb"), ("a\nb".into(), "a b".into()));
    assert_eq!(p("`` ` `` and ` a\nb `").0, "` and a b");
    assert_eq!(p("&#0; &copy; \0").0, "\u{fffd} © \u{fffd}");
    assert_eq!(p("![a ![b](c)](d)").1, "a b");
    // A subtree: the text of a link.
    let t = inline("see [the *docs*](u) now");
    let link = t
        .nodes()
        .find(|n| n.kind() == InlineKind::Link)
        .expect("a link");
    let mut s = String::new();
    link.push_text(TextProjection::TextContent, &mut s);
    assert_eq!(s, "the docs");
    let mut all = String::new();
    t.push_text(TextProjection::CollectText, &mut all);
    assert_eq!(all, "see the docs now");
}

#[test]
fn ref_def_spans() {
    let text = "[Foo\\]x]: <my url> \"t\\\"\"\nrest";
    let (n, d) = parse_ref_def_spans(text).expect("a definition");
    assert_eq!(
        Some((n, d.label.clone(), d.def.clone())),
        parse_ref_def(text)
    );
    assert_eq!(&text[d.label_range.clone()], "Foo\\]x");
    assert_eq!(&text[d.destination_range.clone()], "<my url>");
    assert_eq!(&text[d.title_range.clone().expect("title")], "\"t\\\"\"");
    assert_eq!(&text[d.range.clone()], "[Foo\\]x]: <my url> \"t\\\"\"");
    assert_eq!(d.def.destination, "my url");
    let (_, d) = parse_ref_def_spans("[b]: /u(1)  \n").expect("a definition");
    assert_eq!(
        (d.destination_range, d.title_range, d.range),
        (5..10, None, 0..12)
    );
}

#[test]
fn located_ref_defs_in_containers() {
    let text = "> [a]:\n>     /url\n> 'multi\n> line'\n\n- [Foo\\]x]: <my url> \"t\"\n";
    let tree = parsed(text, ParseOptions::default());
    let mut defs = Vec::new();
    gv_syntax::walk(
        &tree,
        &mut gv_syntax::tree::walk::LeafFn(|l: &Leaf<'_>| {
            if let Some(d) = l.link_ref_def(text) {
                defs.push(d);
            } else {
                assert_ne!(l.kind, LeafKind::LinkRefDef);
            }
        }),
    );
    assert_eq!(defs.len(), 2);
    let d = &defs[0];
    assert_eq!(&text[d.label_range.clone()], "a");
    assert_eq!(&text[d.destination_range.clone()], "/url");
    assert_eq!(
        &text[d.title_range.clone().expect("title")],
        "'multi\n> line'"
    );
    assert_eq!(
        &text[d.range.clone()],
        "[a]:\n>     /url\n> 'multi\n> line'"
    );
    let d = &defs[1];
    assert_eq!(d.label, "foo\\]x");
    assert_eq!(&text[d.label_range.clone()], "Foo\\]x");
    assert_eq!(&text[d.destination_range.clone()], "<my url>");
    assert_eq!(&text[d.title_range.clone().expect("title")], "\"t\"");
}

#[test]
fn composed_changes() {
    let text = "a\n\nb\n\nc\n\nd\n";
    let tree = parsed(text, ParseOptions::default());
    // Edit near the end, then near the start.
    let (t1, s1, c1) = edit(&tree, text, 9..9, "x");
    let (t2, s2, c2) = edit(&t1, &s1, 1..1, "y");
    let c = c1.compose(&c2);
    assert_eq!(c, Changes::compose_all([&c1, &c2]));
    assert_eq!(c.restart_line, 0);
    assert_eq!(c.old_range.start, c.new_range.start);
    assert_eq!(&text[..c.old_range.start], &s2[..c.new_range.start]);
    assert_eq!(&text[c.old_range.end..], &s2[c.new_range.end..]);
    assert_eq!(
        tree.line_count() - c.old_end_line,
        t2.line_count() - c.new_end_line
    );
    assert!(Changes::default().is_empty());
    assert_eq!(Changes::default().compose(&c1), c1);
    assert_eq!(c1.compose(&Changes::default()), c1);
}
