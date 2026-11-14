// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Incremental updates, refreshes, copy and find on hand-picked cases.

mod common;

use gv_inline::Bias;
use std::cell::Cell;

use gv_inline::ItemKind as RunKind;
use gv_render_tree::{
    BoxRef, CopyOptions, FindOptions, FrameKind, ImageSize, InlineRole, ItemKind, LinkStatus,
    LinkTarget, ListMarker, NodeKind, RenderOptions, Sources,
};
use gv_syntax::{Change, ParseOptions};

fn markers(d: &common::Doc) -> Vec<String> {
    d.render
        .items()
        .iter()
        .filter_map(|it| {
            it.context.iter().rev().find_map(|f| match f.kind {
                FrameKind::Item { marker } => Some(marker.text()),
                _ => None,
            })
        })
        .collect()
}

#[test]
fn typing_in_a_paragraph_rebuilds_only_its_block() {
    let mut d = common::build("# Title\n\nfirst para\n\nsecond para\n\n- a\n- b\n");
    let before = d.render.items().to_vec();
    let at = d.text.find("first").expect("found") + 5;
    let (_, report) = d.edit(at..at, "!", Sources::default());
    assert!(!report.full);
    assert_eq!(report.nodes_built, 1);
    assert_eq!(report.inserted.len(), 1);
    assert_eq!(report.changed, vec![before[1].id]);
    let after = d.render.items();
    assert_eq!(after.len(), before.len());
    for (a, b) in before.iter().zip(after) {
        assert_eq!(a.id, b.id);
    }
    assert_eq!(after[2].source.start, before[2].source.start + 1);
    let t = d.render.item_text(1);
    assert_eq!(t.text, "first! para");
    common::check(&d);
}

#[test]
fn inserting_an_item_renumbers_the_list() {
    let mut d = common::build("intro\n\n1. a\n2. b\n3. c\n\nafter\n");
    assert_eq!(markers(&d), ["1.", "2.", "3."]);
    let at = d.text.find("2. b").expect("found");
    let (_, report) = d.edit(at..at, "2. new\n", Sources::default());
    assert!(!report.full);
    assert_eq!(markers(&d), ["1.", "2.", "3.", "4."]);
    // The paragraphs of the old items were reused.
    assert!(report.nodes_reused >= 2, "{report:?}");
    common::check(&d);
    // Changing the start number renumbers every item.
    let (_, report) = d.edit(7..8, "5", Sources::default());
    assert!(!report.full);
    assert_eq!(markers(&d), ["5.", "6.", "7.", "8."]);
    common::check(&d);
}

#[test]
fn a_blank_line_makes_the_whole_list_loose() {
    let mut d = common::build("- a\n- b\n- c\n- d\n");
    let gaps = |d: &common::Doc| {
        d.render
            .items()
            .iter()
            .map(|it| it.gap_before)
            .collect::<Vec<_>>()
    };
    assert_eq!(gaps(&d), [0.0, 0.0, 0.0, 0.0]);
    let at = d.text.find("- c").expect("found");
    let (_, report) = d.edit(at..at, "\n", Sources::default());
    assert!(!report.full);
    assert_eq!(gaps(&d), [0.0, 12.0, 12.0, 12.0]);
    common::check(&d);
}

#[test]
fn a_changed_definition_rebuilds_the_links_that_use_it() {
    let mut d = common::build("[x] and [y]\n\nmiddle\n\nmore\n\n[x]: /a\n");
    let link = |d: &common::Doc| match &d.render.nodes()[0].kind {
        NodeKind::Paragraph(c) => c.links.iter().map(|l| l.destination.to_string()).collect(),
        _ => Vec::new(),
    };
    assert_eq!(link(&d), ["/a"]);
    let id = d.render.items()[0].id;
    let at = d.text.find("/a").expect("found") + 1;
    let (_, report) = d.edit(at..at + 1, "b", Sources::default());
    assert_eq!(link(&d), ["/b"]);
    assert!(report.changed.contains(&id), "{report:?}");
    // A new definition turns `[y]` into a link.
    let end = d.text.len();
    d.edit(end..end, "[y]: /c\n", Sources::default());
    assert_eq!(link(&d), ["/b", "/c"]);
    common::check(&d);
}

#[test]
fn renaming_a_heading_moves_the_duplicate_suffixes() {
    let mut d = common::build("# A\n\ntext\n\n# A\n");
    let anchors = |d: &common::Doc| {
        d.render
            .nodes()
            .iter()
            .filter_map(|n| match &n.kind {
                NodeKind::Heading { anchor, .. } => anchor.as_deref().map(str::to_owned),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(anchors(&d), ["a", "a-1"]);
    d.edit(2..3, "B", Sources::default());
    assert_eq!(anchors(&d), ["b", "a"]);
    assert_eq!(d.render.heading_item("a"), Some(2));
}

#[test]
fn refresh_restyles_links_when_statuses_arrive() {
    let mut d = common::build("[a](x.md) text\n\nno links\n\n[b](https://e.org)\n");
    let status = |t: &LinkTarget<'_>| {
        if t.destination.starts_with("https:") {
            LinkStatus::External
        } else {
            LinkStatus::Exact
        }
    };
    let report = d.render.refresh(
        &d.tree,
        d.text.as_str(),
        Sources {
            links: &status,
            ..Sources::default()
        },
    );
    assert_eq!(report.nodes_built, 2);
    assert_eq!(report.changed.len(), 2);
    let statuses: Vec<_> = d
        .render
        .nodes()
        .iter()
        .filter_map(|n| n.inline())
        .flat_map(|c| c.links.iter().map(|l| l.status))
        .collect();
    assert_eq!(statuses, [LinkStatus::Exact, LinkStatus::External]);
    // External links get an icon box after the link.
    let last = d.render.nodes()[2].inline().expect("inline");
    assert_eq!(last.paragraph.items().len(), 2);
}

#[test]
fn copy_clips_to_the_selection() {
    let d =
        common::build("Hello **world** and `co de`\n\n- one\n- two\n\n```\nx = 1\ny = 2\n```\n");
    let t = &d.text;
    let from = t.find("llo").expect("found");
    let to = t.find("rld").expect("found") + 1;
    assert_eq!(
        d.render.plain_text(from..to, CopyOptions::default()),
        "llo wor"
    );
    let to = t.find("co de").expect("found") + 4;
    assert_eq!(
        d.render.plain_text(0..to, CopyOptions::default()),
        "Hello world and co d"
    );
    assert_eq!(
        d.render.plain_text(0..t.len(), CopyOptions::default()),
        "Hello world and co de\n\n- one\n- two\n\nx = 1\ny = 2"
    );
    assert_eq!(
        d.render.plain_text(
            0..t.len(),
            CopyOptions {
                list_markers: false,
                blank_lines: false
            }
        ),
        "Hello world and co de\none\ntwo\nx = 1\ny = 2"
    );
    // Part of a code block.
    let from = t.find("1\ny").expect("found");
    assert_eq!(
        d.render.plain_text(from..from + 3, CopyOptions::default()),
        "1\ny"
    );
}

#[test]
fn item_text_maps_escapes_and_entities() {
    let d = common::build("a\\*b &amp; c\n");
    let t = d.render.item_text(0);
    assert_eq!(t.text, "a*b & c");
    // `*` comes from `\*` (1..3), `&` from `&amp;` (5..10).
    assert_eq!(t.source_offset(1), 1);
    assert_eq!(t.source_offset(2), 3);
    assert_eq!(t.source_offset(4), 5);
    assert_eq!(t.source_offset(5), 10);
    assert_eq!(t.text_offset(2, Bias::Left), 1);
    assert_eq!(t.text_offset(2, Bias::Right), 2);
    assert_eq!(t.text_offset(12, Bias::Left), 7);
}

#[test]
fn find_reports_source_ranges() {
    let d = common::build("Alpha *beta* alpha\n\n```\nALPHA\n```\n");
    let found = d.render.find("alpha", FindOptions::default());
    let sources: Vec<&str> = found.iter().map(|m| &d.text[m.source.clone()]).collect();
    assert_eq!(sources, ["Alpha", "alpha", "ALPHA"]);
    let exact = d.render.find(
        "alpha",
        FindOptions {
            case_sensitive: true,
        },
    );
    assert_eq!(exact.len(), 1);
    let found = d.render.find("a b", FindOptions::default());
    assert_eq!(&d.text[found[0].source.clone()], "a *b");
}

#[test]
fn code_chunks_have_paragraphs_and_stable_ids() {
    let mut code = String::from("```\n");
    for i in 0..130 {
        code.push_str(&format!("l{i}\n"));
    }
    code.push_str("```\n\nafter\n");
    let mut d = common::build(&code);
    let kinds: Vec<_> = d.render.items().iter().map(|it| it.kind.clone()).collect();
    assert_eq!(kinds.len(), 4);
    assert!(
        matches!(kinds[2], ItemKind::Code { chunk: 2, chunks: 3, ref lines } if *lines == (100..130))
    );
    let p = d.render.item_paragraph(1).expect("paragraph");
    assert!(p.text().starts_with("l50\nl51\n"));
    // Editing the last paragraph keeps the code chunks.
    let ids: Vec<_> = d.render.items().iter().map(|it| it.id).collect();
    let end = d.text.len() - 1;
    let (_, report) = d.edit(end..end, "!", Sources::default());
    assert_eq!(report.inserted.len(), 1);
    let now: Vec<_> = d.render.items().iter().map(|it| it.id).collect();
    assert_eq!(ids, now);
}

#[test]
fn deep_nesting_is_capped() {
    let md = format!("{}x\n", "> ".repeat(100));
    let d = common::build_with(
        &md,
        ParseOptions::default(),
        RenderOptions::default(),
        Sources::default(),
    );
    assert_eq!(d.render.items().len(), 1);
    assert_eq!(
        d.render.items()[0].context.len(),
        gv_render_tree::MAX_FRAMES
    );
    common::check(&d);
    let list = format!("{}y\n", "- ".repeat(60));
    let d = common::build(&list);
    assert_eq!(
        d.render.items()[0].context.len(),
        gv_render_tree::MAX_FRAMES
    );
    assert!(matches!(
        d.render.items()[0].context[1].kind,
        FrameKind::Item {
            marker: ListMarker::Bullet { depth: 0 }
        }
    ));
}

#[test]
fn image_and_link_suffix_boxes_have_distinct_ids() {
    // Image 0 and link 0 in one paragraph: both used to get box id 0.
    let md = "![pic](pic.png) and [web](https://example.org)\n";
    let links = |_: &LinkTarget<'_>| LinkStatus::External;
    let images = |_: &LinkTarget<'_>| ImageSize::Known {
        width: 40.0,
        height: 20.0,
    };
    let d = common::build_with(
        md,
        ParseOptions::default(),
        RenderOptions::default(),
        Sources {
            links: &links,
            images: &images,
        },
    );
    common::check(&d);
    let c = d.render.nodes()[0].inline().expect("inline");
    let boxes: Vec<_> = c
        .paragraph
        .items()
        .iter()
        .zip(c.spans.iter())
        .filter_map(|(it, span)| match it.kind() {
            RunKind::Box(b) => Some((b.id, *span)),
            _ => None,
        })
        .collect();
    assert_eq!(boxes.len(), 2, "an image box and a suffix box");
    assert_ne!(boxes[0].0, boxes[1].0);
    // Each id names what its span says the box is.
    for (id, span) in boxes {
        match (BoxRef::from_id(id), span.role) {
            (BoxRef::Image(i), InlineRole::Image(j)) => assert_eq!(i, j),
            (BoxRef::LinkSuffix(l), InlineRole::LinkSuffix) => assert_eq!(Some(l), span.link),
            other => panic!("box id {id} for {other:?}"),
        }
    }
    for r in [
        BoxRef::Image(0),
        BoxRef::LinkSuffix(0),
        BoxRef::LinkSuffix(7),
    ] {
        assert_eq!(BoxRef::from_id(r.id()), r);
    }
}

#[test]
fn wide_images_fit_the_text_column() {
    let images = |_: &LinkTarget<'_>| ImageSize::Known {
        width: 1360.0,
        height: 400.0,
    };
    let d = common::build_with(
        "![wide](w.png)\n",
        ParseOptions::default(),
        RenderOptions::default(),
        Sources {
            images: &images,
            ..Sources::default()
        },
    );
    let c = d.render.nodes()[0].inline().expect("inline");
    let RunKind::Box(b) = c.paragraph.items()[0].kind() else {
        panic!("an image box");
    };
    // The column is 680 wide (its padding is outside it).
    assert_eq!((b.width, b.height()), (680.0, 200.0));
}

fn code_doc(lines: usize) -> String {
    let mut md = String::from("intro\n\n```\n");
    for i in 0..lines {
        md.push_str(&format!(
            "line {i} of a code block that is long enough to wrap\n"
        ));
    }
    md.push_str("```\n\nafter\n");
    md
}

#[test]
fn items_of_a_block_are_its_chunks() {
    let d = common::build(&code_doc(120));
    let code = d.render.items()[1].id.block;
    assert_eq!(d.render.items_of(code), 1..4);
    let intro = d.render.items()[0].id.block;
    assert_eq!(d.render.items_of(intro), 0..1);
    let after = d.render.items()[4].id.block;
    assert_eq!(d.render.items_of(after), 4..5);
    assert_eq!(d.render.items_of(gv_syntax::BlockId(u64::MAX)), 0..0);
}

#[test]
fn code_wrap_is_per_block_and_survives_updates() {
    let md = format!("{}\n```\nshort\n```\n", code_doc(60));
    let mut d = common::build(&md);
    let block = d.render.items()[1].id.block;
    let other = d.render.items().last().expect("items").id.block;
    let width = 300.0;
    let heights = |d: &common::Doc| -> Vec<f32> {
        d.render
            .items()
            .iter()
            .map(|it| it.height_estimate(width))
            .collect()
    };
    let before = heights(&d);
    assert!(!d.render.code_wrap(block));
    let range = d.render.set_code_wrap(block, true);
    assert_eq!(range, d.render.items_of(block));
    assert_eq!(range.len(), 2);
    assert!(d.render.code_wrap(block));
    assert!(!d.render.code_wrap(other));
    let after = heights(&d);
    for (i, (a, b)) in before.iter().zip(&after).enumerate() {
        if range.contains(&i) {
            assert!(b > a, "item {i} wraps: {b} > {a}");
        } else {
            assert_eq!(a, b, "item {i} is not this block's");
        }
    }
    common::check(&d);
    // An edit elsewhere keeps the setting.
    let end = d.text.find("after").expect("found");
    d.edit(end..end, "x", Sources::default());
    assert_eq!(heights(&d), after);
    // So does a rebuild of the whole tree.
    let text = format!("{}!", d.text);
    let at = d.text.len();
    let (tree, mut changes) = d.tree.reparse_full(
        text.as_str(),
        Change {
            start: at,
            old_end: at,
            new_len: 1,
        },
    );
    let model = d.model.update(&tree, text.as_str(), &changes);
    // A report that does not fit the tree forces a full rebuild.
    changes.new_range = text.len() + 1..text.len() + 1;
    let report = d
        .render
        .update(&tree, &model, text.as_str(), &changes, Sources::default());
    assert!(report.full);
    assert!(d.render.code_wrap(block));
    assert_eq!(heights(&d)[range.clone()], after[range.clone()]);
    // Turning it off restores the estimates.
    d.render.set_code_wrap(block, false);
    assert_eq!(heights(&d)[range.clone()], before[range]);
}

#[test]
fn refresh_where_asks_only_what_it_selects() {
    let mut d = common::build("[a](a.md) ![i](i.png)\n\n[b](https://e.org)\n");
    let asked = Cell::new(0);
    let links = |t: &LinkTarget<'_>| {
        asked.set(asked.get() + 1);
        if t.destination.starts_with("https:") {
            LinkStatus::External
        } else {
            LinkStatus::Exact
        }
    };
    let images = |_: &LinkTarget<'_>| ImageSize::Known {
        width: 10.0,
        height: 10.0,
    };
    let sources = Sources {
        links: &links,
        images: &images,
    };
    // Only images: the first block's image changed; links are not asked
    // except while that block is rebuilt (its one link).
    let report = d
        .render
        .refresh_where(&d.tree, d.text.as_str(), sources, &|t| t.image);
    assert_eq!(report.nodes_built, 1);
    assert_eq!(asked.get(), 1);
    let first = d.render.nodes()[0].inline().expect("inline");
    assert!(matches!(first.images[0].size, ImageSize::Known { .. }));
    assert_eq!(first.links[0].status, LinkStatus::Exact);
    let last = d.render.nodes()[1].inline().expect("inline");
    assert_eq!(last.links[0].status, LinkStatus::Pending);
    // Only local links: the URL keeps its (stale) status.
    asked.set(0);
    let local = |t: &LinkTarget<'_>| !t.image && !t.destination.contains(':');
    let report = d
        .render
        .refresh_where(&d.tree, d.text.as_str(), sources, &local);
    assert_eq!(report.nodes_built, 0);
    assert_eq!(asked.get(), 1);
    // The full refresh asks everything.
    asked.set(0);
    let report = d.render.refresh(&d.tree, d.text.as_str(), sources);
    assert_eq!(report.nodes_built, 1);
    assert_eq!(asked.get(), 3, "two links checked, one rebuilt");
    let last = d.render.nodes()[1].inline().expect("inline");
    assert_eq!(last.links[0].status, LinkStatus::External);
    common::check(&d);
}
