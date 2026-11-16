// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Task list items, footnotes and GitHub alerts: the GFM-exact emitter
//! against comrak 0.59 (its `tasklist`, `footnotes` and `alerts`
//! extensions), the two task list examples the GFM spec marks `disabled`,
//! the block views, and the profiles that turn each construct on.
//!
//! Where GitHub's documented behavior and comrak differ, the case says so
//! and follows GitHub: an alert needs a body and takes no title.

mod support;

use gv_syntax::harness::{normalize_html, render_gfm_html};
use gv_syntax::{
    AlertKind, Container, ContainerKind, Leaf, LeafKind, ParseOptions, SyntaxTree, TaskLists,
    TokenKind, Visitor, to_gfm_html, walk, walk_range,
};
use support::workspace_root;

/// GFM with task lists and footnotes, as `render_gfm_html` reads them.
const EXT: &[&str] = &[
    "table",
    "strikethrough",
    "autolink",
    "tasklist",
    "footnotes",
];

fn comrak(md: &str, alerts: bool) -> String {
    let mut o = comrak::Options::default();
    o.render.r#unsafe = true;
    o.extension.table = true;
    o.extension.strikethrough = true;
    o.extension.autolink = true;
    o.extension.tasklist = true;
    o.extension.footnotes = true;
    o.extension.alerts = alerts;
    comrak::markdown_to_html(md, &o)
}

fn alerts_options() -> ParseOptions {
    ParseOptions {
        task_lists: TaskLists::Standard,
        footnotes: true,
        alerts: true,
        ..ParseOptions::GFM
    }
}

fn ours_alerts(md: &str) -> String {
    to_gfm_html(
        &SyntaxTree::parse_with(md, alerts_options()),
        md,
        gv_syntax::GfmHtmlOptions::default(),
    )
}

#[track_caller]
fn same_as_comrak(md: &str) {
    let ours = render_gfm_html(md, EXT);
    let theirs = comrak(md, false);
    assert_eq!(
        normalize_html(&ours),
        normalize_html(&theirs),
        "\ninput: {md:?}\nours:\n{ours}\ncomrak:\n{theirs}"
    );
}

#[track_caller]
fn same_as_comrak_alerts(md: &str) {
    let ours = ours_alerts(md);
    let theirs = comrak(md, true);
    assert_eq!(
        normalize_html(&ours),
        normalize_html(&theirs),
        "\ninput: {md:?}\nours:\n{ours}\ncomrak:\n{theirs}"
    );
}

#[track_caller]
fn html(md: &str, expected: &str) {
    let ours = ours_alerts(md);
    assert_eq!(
        normalize_html(&ours),
        normalize_html(expected),
        "\ninput: {md:?}\nours:\n{ours}"
    );
}

// ----- Task list items -----

/// The two examples of the GFM spec's task list section, which the spec
/// marks `disabled` (cmark-gfm's runner skips them): they pass with the
/// `tasklist` extension.
#[test]
fn disabled_gfm_spec_examples_pass() {
    let path = workspace_root().join("tests/fixtures/spec/gfm-0.29.0.gfm.13.txt");
    let text = std::fs::read_to_string(path).expect("GFM spec");
    let fence = "````````````````````````````````";
    let mut examples = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() != format!("{fence} example disabled") {
            continue;
        }
        let (mut md, mut out, mut in_html) = (String::new(), String::new(), false);
        for l in lines.by_ref() {
            if l.trim() == fence {
                break;
            }
            if l == "." {
                in_html = true;
                continue;
            }
            let target = if in_html { &mut out } else { &mut md };
            target.push_str(&l.replace('→', "\t"));
            target.push('\n');
        }
        examples.push((md, out));
    }
    assert_eq!(examples.len(), 2, "the spec has two disabled examples");
    for (md, expected) in &examples {
        let ours = render_gfm_html(md, &["tasklist"]);
        assert_eq!(
            normalize_html(&ours),
            normalize_html(expected),
            "\ninput: {md:?}\nours:\n{ours}"
        );
    }
}

#[test]
fn task_items_match_comrak() {
    for md in [
        "- [ ] foo\n- [x] bar\n- [X] baz\n",
        "- [x] foo\n  - [ ] bar\n  - [x] baz\n- [ ] bim\n",
        "1. [x] one\n2. [ ] two\n",
        "- [ ]\tfoo\n",
        "- [ ] foo\n\n- [x] bar\n",
        "- [ ] foo\n\n  more\n",
        "- [ ] *em* `code`\n",
        "- [ ]\n- [x]\n",
        "- [ ]\n  foo\n",
        // Not task items.
        "- [ ]foo\n",
        "- [y] foo\n",
        "- [  ] foo\n",
        "- foo [ ] bar\n",
        "- foo\n\n  [ ] bar\n",
        "[ ] not in a list\n",
        "> - [x] quoted\n",
        "- [x] # heading text\n",
    ] {
        same_as_comrak(md);
    }
}

#[test]
fn task_items_in_the_views() {
    let md = "- [ ] a\n- [x] b\n- c\n";
    let tree = SyntaxTree::parse_with(md, alerts_options());
    tree.validate(md).expect("valid tree");
    struct Items(Vec<Option<String>>, &'static str);
    impl Visitor for Items {
        fn enter(&mut self, c: &Container) {
            if c.kind == ContainerKind::Item {
                self.0.push(c.marker.clone().map(|r| self.1[r].to_owned()));
            }
        }
    }
    let mut v = Items(Vec::new(), md);
    walk(&tree, &mut v);
    assert_eq!(v.0, [Some("[ ]".to_owned()), Some("[x]".to_owned()), None]);
    // A range walk that starts inside the item finds the checkbox too.
    let mut v = Items(Vec::new(), md);
    walk_range(&tree, 13..14, &mut v);
    assert_eq!(v.0, [Some("[x]".to_owned())]);
    // The paragraph starts after the checkbox.
    let mut paras = Vec::new();
    walk(
        &tree,
        &mut gv_syntax::tree::walk::LeafFn(|l: &Leaf<'_>| {
            if l.kind == LeafKind::Paragraph {
                paras.push(md[l.lines[0].content.clone()].to_owned());
            }
        }),
    );
    assert_eq!(paras, ["a", "b", "c"]);
    // The checkbox is an extension marker token.
    let tokens = tree.tokens(2..5);
    assert!(
        tokens
            .iter()
            .any(|(r, t)| t.kind == TokenKind::ExtMarker && *r == (2..5))
    );
}

#[test]
fn any_character_tasks() {
    let options = ParseOptions {
        task_lists: TaskLists::AnyCharacter,
        ..ParseOptions::GFM
    };
    for (md, task) in [
        ("- [-] cancelled\n", true),
        ("- [/] half\n", true),
        ("- [é] accented\n", true),
        ("- [] none\n", false),
        ("- [ab] two\n", false),
    ] {
        let tree = SyntaxTree::parse_with(md, options);
        let mut found = false;
        struct F<'a>(&'a mut bool);
        impl Visitor for F<'_> {
            fn enter(&mut self, c: &Container) {
                *self.0 |= c.is_task();
            }
        }
        walk(&tree, &mut F(&mut found));
        assert_eq!(found, task, "{md:?}");
    }
}

#[test]
fn commonmark_has_no_tasks_footnotes_or_alerts() {
    let md = "- [x] a[^1]\n\n[^1]: n\n\n> [!NOTE]\n> b\n";
    let tree = SyntaxTree::parse(md);
    let kinds: Vec<_> = tree.blocks().iter().map(|b| b.kind).collect();
    assert!(!kinds.iter().any(|k| matches!(
        k,
        gv_syntax::BlockKind::Alert { .. } | gv_syntax::BlockKind::FootnoteDefinition
    )));
    assert!(
        !tree
            .tokens(0..md.len())
            .iter()
            .any(|(_, t)| t.kind == TokenKind::ExtMarker)
    );
}

// ----- Footnotes -----

#[test]
fn footnotes_match_comrak() {
    for md in [
        "Text[^1].\n\n[^1]: The note.\n",
        "A[^a] and B[^b] and A again[^a].\n\n[^b]: Bee.\n[^a]: Ay.\n",
        "Unreferenced.\n\n[^x]: Never used.\n",
        "Missing[^nope].\n",
        "Named[^note].\n\n[^note]: Named note.\n",
        "Case[^Note].\n\n[^note]: Folded.\n",
        "Para[^1].\n\n[^1]: First paragraph.\n\n    Second paragraph.\n",
        "List[^1].\n\n[^1]:\n    - one\n    - two\n",
        "Code[^1].\n\n[^1]: Text.\n\n        code\n",
        "Lazy[^1].\n\n[^1]: one\ntwo\n",
        "Ends[^1].\n\n[^1]: one\nnot indented\n\nafter\n",
        "Interrupt\n[^1]: def\n\nref[^1]\n",
        "Link [^1](http://x.com) wins.\n\n[^1]: n\n",
        "Ref [^1][r] wins.\n\n[^1]: n\n\n[r]: /url\n",
        "Emph [^*a*].\n\n[^*a*]: e\n",
        "In table\n\n| a |\n| - |\n| b[^1] |\n\n[^1]: t\n",
        "Nested > quote\n\n> q[^1]\n\n[^1]: n\n",
        "Empty[^1]\n\n[^1]:\n",
        "Heading\n\n# H[^1]\n\n[^1]: h\n",
        "[^1]: before\n\nref[^1]\n",
    ] {
        same_as_comrak(md);
    }
}

#[test]
fn footnote_definitions_in_the_views() {
    let md = "x[^n]\n\n[^n]: one\n    two\n\nafter\n";
    let tree = SyntaxTree::parse_with(md, alerts_options());
    tree.validate(md).expect("valid tree");
    let refs = tree.ref_map(md);
    assert_eq!(refs.footnote("n"), Some("n"));
    assert_eq!(refs.footnote_count(), 1);
    struct Defs(Vec<(String, String)>, &'static str);
    impl Visitor for Defs {
        fn enter(&mut self, c: &Container) {
            if let Some(l) = c.footnote_label() {
                let m = c.marker.clone().unwrap_or(0..0);
                self.0.push((self.1[l].to_owned(), self.1[m].to_owned()));
            }
        }
    }
    let mut v = Defs(Vec::new(), md);
    walk(&tree, &mut v);
    assert_eq!(v.0, [("n".to_owned(), "[^n]:".to_owned())]);
    let mut v = Defs(Vec::new(), md);
    walk_range(&tree, 20..21, &mut v);
    assert_eq!(v.0, [("n".to_owned(), "[^n]:".to_owned())]);
    // The reference is an inline node.
    let mut labels = Vec::new();
    walk(
        &tree,
        &mut gv_syntax::tree::walk::LeafFn(|l: &Leaf<'_>| {
            if l.kind == LeafKind::Paragraph {
                let t = tree.inlines(l, md);
                labels.extend(t.footnote_refs().map(|(s, r)| (s.to_owned(), r)));
            }
        }),
    );
    assert_eq!(labels, [("n".to_owned(), 1..5)]);
}

#[test]
fn obsidian_footnote_indent() {
    let options = ParseOptions {
        footnotes: true,
        footnote_indent: 2,
        ..ParseOptions::GFM
    };
    let md = "x[^1]\n\n[^1]: a\n\n  b\n";
    let tree = SyntaxTree::parse_with(md, options);
    let mut depth_of_b = None;
    struct D<'a>(usize, &'a mut Option<usize>);
    impl Visitor for D<'_> {
        fn enter(&mut self, _: &Container) {
            self.0 += 1;
        }
        fn exit(&mut self, _: &Container) {
            self.0 -= 1;
        }
        fn leaf(&mut self, l: &Leaf<'_>) {
            if l.start_line == 4 {
                *self.1 = Some(self.0);
            }
        }
    }
    walk(&tree, &mut D(0, &mut depth_of_b));
    assert_eq!(depth_of_b, Some(1), "`b` is inside the definition");
}

// ----- Alerts -----

#[test]
fn alerts_match_comrak() {
    for md in [
        "> [!NOTE]\n> Useful information.\n",
        "> [!TIP]\n> Helpful advice.\n",
        "> [!IMPORTANT]\n> Key information.\n",
        "> [!WARNING]\n> Urgent info.\n",
        "> [!CAUTION]\n> Risks.\n",
        "> [!note]\n> Lower case.\n",
        "> [!NOTE]\n> One\n>\n> Two\n",
        "> [!NOTE]\n> - a list\n",
        "> [!NOTE]   \n> trailing spaces\n",
        // Not alerts.
        "> [!NOTES]\n> no\n",
        "> Text\n> [!NOTE]\n",
    ] {
        same_as_comrak_alerts(md);
    }
}

#[test]
fn alerts_follow_github_where_comrak_differs() {
    // No body: a block quote (comrak writes an empty alert).
    html("> [!NOTE]\n", "<blockquote><p>[!NOTE]</p></blockquote>");
    html(
        "> [!WARNING]\n\nafter\n",
        "<blockquote><p>[!WARNING]</p></blockquote><p>after</p>",
    );
    // A line after the marker line is no lazy continuation: the marker
    // line is no paragraph (GitHub, which reads alerts from the parsed
    // block quote, continues the paragraph; comrak writes an empty alert).
    html(
        "> [!NOTE]\nlazy line\n",
        "<blockquote><p>[!NOTE]</p></blockquote><p>lazy line</p>",
    );
    // Not in lists (comrak makes an alert).
    html(
        "- > [!NOTE]\n  > in a list\n",
        "<ul><li><blockquote><p>[!NOTE]\nin a list</p></blockquote></li></ul>",
    );
    // Not nested (comrak makes an alert in the quote).
    html(
        "> > [!NOTE]\n> > nested\n",
        "<blockquote><blockquote><p>[!NOTE]\nnested</p></blockquote></blockquote>",
    );
    // A title is not allowed (comrak takes it as the title).
    html(
        "> [!NOTE] Title\n> body\n",
        "<blockquote><p>[!NOTE] Title\nbody</p></blockquote>",
    );
}

#[test]
fn alerts_in_the_views() {
    let md = "> [!TIP]\n> Body.\n";
    let tree = SyntaxTree::parse_with(md, alerts_options());
    tree.validate(md).expect("valid tree");
    struct A(Vec<(ContainerKind, Option<std::ops::Range<usize>>)>);
    impl Visitor for A {
        fn enter(&mut self, c: &Container) {
            self.0.push((c.kind, c.marker.clone()));
        }
    }
    let mut v = A(Vec::new());
    walk(&tree, &mut v);
    assert_eq!(
        v.0,
        [(
            ContainerKind::Alert {
                kind: AlertKind::Tip
            },
            Some(2..8)
        )]
    );
    assert!(
        ContainerKind::Alert {
            kind: AlertKind::Tip
        }
        .is_quote()
    );
    let off = ParseOptions {
        alerts: false,
        ..alerts_options()
    };
    let mut v = A(Vec::new());
    walk(&SyntaxTree::parse_with(md, off), &mut v);
    assert_eq!(v.0, [(ContainerKind::BlockQuote, None)]);
}

#[test]
fn profiles_turn_the_constructs_on() {
    use gv_syntax::ProfileId;
    let gfm = ParseOptions::builtin(ProfileId::GfmFile);
    assert_eq!(gfm.task_lists, TaskLists::Standard);
    assert!(gfm.footnotes && gfm.alerts);
    let ext = ParseOptions::builtin(ProfileId::Extended);
    assert!(ext.footnotes && ext.alerts && ext.task_lists == TaskLists::Standard);
    let ob = ParseOptions::builtin(ProfileId::Obsidian);
    assert!(ob.footnotes && !ob.alerts && ob.task_lists == TaskLists::AnyCharacter);
    let cm = ParseOptions::builtin(ProfileId::CommonMark);
    assert!(!cm.footnotes && !cm.alerts && cm.task_lists == TaskLists::Off);
}

#[test]
fn front_matter_is_one_block() {
    for (options, md, kind) in [
        (
            ParseOptions::builtin(gv_syntax::ProfileId::GfmFile),
            "---\ntitle: x\n---\n# H\n",
            "yaml",
        ),
        (
            ParseOptions::builtin(gv_syntax::ProfileId::Extended),
            "+++\ntitle = 'x'\n+++\n# H\n",
            "toml",
        ),
    ] {
        let tree = SyntaxTree::parse_with(md, options);
        let mut found = None;
        walk(
            &tree,
            &mut gv_syntax::tree::walk::LeafFn(|l: &Leaf<'_>| {
                if let LeafKind::FrontMatter { kind } = l.kind {
                    found = Some(kind.name());
                }
            }),
        );
        assert_eq!(found, Some(kind), "{md:?}");
    }
}
