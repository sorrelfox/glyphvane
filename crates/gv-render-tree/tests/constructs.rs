// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Reading view's GitHub constructs in the render tree: task items,
//! footnotes (numbered references, the footnote section with its
//! backreferences), alerts, front matter, `<details>` and `<picture>`.

mod common;

use common::{Doc, build_with, check};
use gv_render_tree::{
    FrameKind, InlineRole, ItemKind, ListMarker, NodeKind, RenderOptions, RenderTree, Sources,
};
use gv_syntax::{AlertKind, ParseOptions, ProfileId};

fn gfm(text: &str) -> Doc {
    build_with(
        text,
        ParseOptions::builtin(ProfileId::GfmFile),
        RenderOptions::builtin(ProfileId::GfmFile),
        Sources::default(),
    )
}

/// The items' kinds and the innermost frame of each.
fn outline(rt: &RenderTree) -> Vec<String> {
    rt.items()
        .iter()
        .map(|it| {
            let frame = it.context.last().map_or(String::new(), |f| match &f.kind {
                FrameKind::Item { marker } => match marker {
                    ListMarker::Task { checked, status } => format!(" task({checked},{status:?})"),
                    _ => " item".to_owned(),
                },
                FrameKind::Alert { kind } => format!(" alert({})", kind.name()),
                FrameKind::Footnote { number, label } => format!(" footnote({number},{label})"),
                FrameKind::Details => " details".to_owned(),
                FrameKind::Quote { .. } => " quote".to_owned(),
                FrameKind::List { .. } => " list".to_owned(),
                FrameKind::Callout { name, .. } => format!(" callout({name})"),
                FrameKind::Embed { depth } => format!(" embed({depth})"),
            });
            format!("{}{frame}", it.kind.name())
        })
        .collect()
}

fn text_of(rt: &RenderTree, ix: usize) -> String {
    rt.item_text(ix).text.clone()
}

#[test]
fn task_items_carry_their_checkbox() {
    let d = gfm("- [ ] open\n- [x] done\n- plain\n");
    check(&d);
    assert_eq!(
        outline(&d.render),
        [
            "paragraph task(false,' ')",
            "paragraph task(true,'x')",
            "paragraph item"
        ]
    );
    // The checkbox is not part of the paragraph.
    assert_eq!(text_of(&d.render, 0), "open");
    // CommonMark: no task items.
    let cm = common::build("- [ ] open\n");
    assert_eq!(outline(&cm.render), ["paragraph item"]);
    assert_eq!(text_of(&cm.render, 0), "[ ] open");
}

#[test]
fn obsidian_statuses() {
    let d = build_with(
        "- [-] cancelled\n- [/] half\n",
        ParseOptions::builtin(ProfileId::Obsidian),
        RenderOptions::builtin(ProfileId::Obsidian),
        Sources::default(),
    );
    assert_eq!(
        outline(&d.render),
        ["paragraph task(true,'-')", "paragraph task(true,'/')"]
    );
}

#[test]
fn an_image_shown_as_its_alt_text_leaves_out_its_size() {
    // No image sizes: each image shows its alt text in its place.
    let d = build_with(
        "A ![cat |300](a.png) B ![*big* dog|30x20](b.png) C ![plain|wide](c.png)\n",
        ParseOptions::builtin(ProfileId::Obsidian),
        RenderOptions::builtin(ProfileId::Obsidian),
        Sources::default(),
    );
    check(&d);
    assert_eq!(text_of(&d.render, 0), "A cat B big dog C plain|wide");
    // GitHub reads the alt text as written.
    let d = gfm("A ![cat|300](a.png)\n");
    assert_eq!(text_of(&d.render, 0), "A cat|300");
}

#[test]
fn footnotes_are_numbered_and_collected_at_the_end() {
    let md = "Alpha[^b] beta[^a] and again[^b].\n\n[^a]: First def.\n[^unused]: Never.\n\n> [^b]: Second def.\n\nAfter.\n";
    let d = gfm(md);
    let rt = &d.render;
    // References: numbers by first reference.
    let node = &rt.nodes()[0];
    let content = node.inline().expect("paragraph");
    let numbers: Vec<_> = content
        .footnotes
        .iter()
        .map(|f| (f.label.to_string(), f.number))
        .collect();
    assert_eq!(
        numbers,
        [
            ("b".to_owned(), 1),
            ("a".to_owned(), 2),
            ("b".to_owned(), 1)
        ]
    );
    assert_eq!(text_of(rt, 0), "Alpha1 beta2 and again1.");
    let refs: Vec<_> = content
        .spans
        .iter()
        .filter(|s| matches!(s.role, InlineRole::FootnoteRef(_)))
        .count()
        .to_string()
        .into_bytes();
    assert_eq!(refs, b"3");
    // The definitions are not where they are written: the main items are
    // the paragraph, the (empty) quote and "After.".
    let section = rt.footnote_section_start();
    assert_eq!(
        outline(rt)[..section],
        ["paragraph", "empty-quote quote", "paragraph"]
    );
    // The section: a rule, then the definitions by number, the unused one
    // dropped, the quote around one left out.
    assert_eq!(
        outline(rt)[section..],
        [
            "thematic-break",
            "paragraph footnote(1,b)",
            "paragraph footnote(2,a)"
        ]
    );
    assert_eq!(text_of(rt, section + 1), "Second def.");
    // Backreferences after the last paragraph: two for `b`, one for `a`.
    let backrefs = |ix: usize| {
        rt.item_inline(ix)
            .expect("content")
            .spans
            .iter()
            .filter(|s| matches!(s.role, InlineRole::FootnoteBackref(_)))
            .count()
    };
    assert_eq!(backrefs(section + 1), 2);
    assert_eq!(backrefs(section + 2), 1);
    // Jumps: the definition of a label, and its references in order.
    assert_eq!(rt.footnote_item("a"), Some(section + 2));
    assert_eq!(rt.footnote_item("unused"), None);
    let b_refs = rt.footnote_refs("b");
    assert_eq!(b_refs.len(), 2);
    assert_eq!(&md[b_refs[0].1.clone()], "[^b]");
    assert!(b_refs[0].1.start < b_refs[1].1.start);
    // Lookups by offset still find the document's items and the section's.
    let after = md.find("After").expect("After");
    assert_eq!(rt.item_at(after), Some(2));
    let def_a = md.find("First def").expect("def");
    assert_eq!(rt.item_at(def_a), Some(section + 2));
}

#[test]
fn a_definition_that_does_not_end_with_a_paragraph_gets_a_line_of_backrefs() {
    let d = gfm("x[^n]\n\n[^n]:\n    - one\n    - two\n");
    let rt = &d.render;
    let section = rt.footnote_section_start();
    let kinds = outline(rt)[section..].to_vec();
    assert_eq!(
        kinds,
        [
            "thematic-break",
            "paragraph item",
            "paragraph item",
            "paragraph footnote(1,n)"
        ]
    );
    let last = rt.items().len() - 1;
    assert_eq!(text_of(rt, last), "");
}

#[test]
fn edits_rebuild_footnote_documents_whole() {
    let fresh = |d: &Doc| {
        RenderTree::build(
            &d.tree,
            &d.model,
            d.text.as_str(),
            RenderOptions::builtin(ProfileId::GfmFile),
            Sources::default(),
        )
    };
    let mut d = gfm("a[^1]\n\n[^1]: one\n");
    let (_, report) = d.edit(0..0, "b[^2] ", Sources::default());
    assert!(report.full);
    assert_eq!(common::canonical(&d.render), common::canonical(&fresh(&d)));
    // Adding the definition makes the reference a footnote.
    let end = d.text.len();
    d.edit(end..end, "\n[^2]: two\n", Sources::default());
    assert_eq!(common::canonical(&d.render), common::canonical(&fresh(&d)));
    assert_eq!(text_of(&d.render, 0), "b1 a2");
}

#[test]
fn alerts_are_framed_with_insets() {
    let d = gfm("> [!TIP]\n> Body one.\n>\n> Body two.\n\nAfter.\n");
    check(&d);
    let rt = &d.render;
    assert_eq!(
        outline(rt),
        ["paragraph alert(tip)", "paragraph alert(tip)", "paragraph"]
    );
    let m = rt.options().metrics;
    let first = &rt.items()[0];
    let second = &rt.items()[1];
    assert_eq!(
        first.frame_insets(),
        (
            m.callout_padding_top + m.callout_title + m.callout_title_gap,
            0.0
        )
    );
    assert_eq!(second.frame_insets(), (0.0, m.callout_padding_bottom));
    assert_eq!(first.content_x(&m), m.callout_padding_x);
    assert_eq!(first.content_right(&m), m.callout_padding_x);
    assert!(
        first.height_estimate(600.0)
            > first.gap_before + first.estimate.height(600.0) + m.callout_title
    );
    if let FrameKind::Alert { kind } = first.context[0].kind {
        assert_eq!(kind, AlertKind::Tip);
    }
}

#[test]
fn an_alert_without_a_body_is_a_quote() {
    let d = gfm("> [!WARNING]\n\nAfter.\n");
    check(&d);
    assert_eq!(outline(&d.render), ["paragraph quote", "paragraph"]);
    assert_eq!(text_of(&d.render, 0), "[!WARNING]");
}

#[test]
fn details_fold_the_blocks_up_to_their_end() {
    let md = "<details open>\n<summary>More <b>info</b></summary>\n\nHidden *text*.\n\n- a list\n\n</details>\n\nAfter.\n";
    let d = gfm(md);
    check(&d);
    let rt = &d.render;
    assert_eq!(
        outline(rt),
        [
            "details",
            "paragraph details",
            "paragraph item",
            "paragraph"
        ]
    );
    assert_eq!(text_of(rt, 0), "More info");
    let NodeKind::Details { open, .. } = &rt.nodes()[0].kind else {
        panic!("details node");
    };
    assert!(*open);
    // The frame is the summary row's block.
    assert_eq!(rt.items()[1].context[0].id, rt.items()[0].id.block);
    // No summary: "Details".
    let d = gfm("<details>\n\nx\n\n</details>\n");
    assert_eq!(text_of(&d.render, 0), "Details");
    // An unclosed details folds to the end of its container.
    let d = gfm("> <details>\n>\n> in\n\nout\n");
    assert_eq!(
        outline(&d.render),
        ["details quote", "paragraph details", "paragraph"]
    );
    // One block that opens and closes stays raw HTML.
    let d = gfm("<details><summary>a</summary>b</details>\n");
    assert_eq!(outline(&d.render), ["html"]);
}

#[test]
fn pictures_pick_by_theme() {
    let md = "<picture>\n  <source media=\"(prefers-color-scheme: dark)\" srcset=\"logo-dark.png\">\n  <img alt=\"Logo\" src=\"logo.png\">\n</picture>\n";
    let d = gfm(md);
    check(&d);
    let NodeKind::Picture(p) = &d.render.nodes()[0].kind else {
        panic!("picture node");
    };
    assert_eq!(p.pick(true), Some("logo-dark.png"));
    assert_eq!(p.pick(false), Some("logo.png"));
    assert_eq!(&*p.alt, "Logo");
    assert_eq!(d.render.items()[0].kind, ItemKind::Picture);
}

#[test]
fn front_matter_is_a_properties_block() {
    let d = gfm("---\ntitle: Notes\ntags: [a, b]\n---\n# Heading\n");
    check(&d);
    assert!(matches!(
        d.render.nodes()[0].kind,
        NodeKind::Properties { .. }
    ));
    assert!(matches!(
        d.render.items()[0].kind,
        ItemKind::Properties { .. }
    ));
}

/// `:shortcode:` emoji are drawn as the emoji in the profiles that have
/// them (GitHub), one unit mapped to the whole code, and stay text in the
/// others (CommonMark, Obsidian).
#[test]
fn emoji_shortcodes_follow_the_profile() {
    let text = "Done :tada: and :nope: **:smile:**\n";
    let d = gfm(text);
    check(&d);
    let shown = d.render.item_text(0);
    assert_eq!(shown.text, "Done 🎉 and :nope: 😄");
    let tada = text.find(":tada:").expect("code");
    assert_eq!(shown.source_offset("Done ".len()), tada);
    assert_eq!(shown.source_offset("Done 🎉".len()), tada + ":tada:".len());
    for id in [ProfileId::CommonMark, ProfileId::Obsidian] {
        let plain = build_with(
            text,
            ParseOptions::builtin(id),
            RenderOptions::builtin(id),
            Sources::default(),
        );
        check(&plain);
        assert_eq!(text_of(&plain.render, 0), "Done :tada: and :nope: :smile:");
    }
}
