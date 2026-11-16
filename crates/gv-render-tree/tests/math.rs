// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Math in profiles with math: ` ```math ` fences and `$$` blocks become
//! display math items, `$…$` formula boxes, and the macro pass runs over
//! all formulas in document order.

mod common;

use gv_render_tree::{BoxRef, CopyOptions, InlineRole, ItemKind, NodeKind, RenderOptions, Sources};
use gv_syntax::{ParseOptions, ProfileId};

const DOC: &str = "Intro.\n\n```math\n\\frac{a}{b}\n+ c\n```\n\n```rust\nlet x = 1;\n```\n";

fn build_in(text: &str, profile: ProfileId) -> common::Doc {
    let doc = common::build_with(
        text,
        ParseOptions::builtin(profile),
        RenderOptions::builtin(profile),
        Sources::default(),
    );
    common::check(&doc);
    doc
}

fn build(opts: RenderOptions) -> common::Doc {
    let doc = common::build_with(DOC, ParseOptions::default(), opts, Sources::default());
    common::check(&doc);
    doc
}

#[test]
fn commonmark_keeps_math_fences_as_code() {
    let doc = build(RenderOptions::builtin(ProfileId::CommonMark));
    assert!(
        doc.render
            .nodes()
            .iter()
            .all(|n| !matches!(n.kind, NodeKind::Math(_)))
    );
}

#[test]
fn profiles_with_math_make_one_math_item() {
    for profile in [
        ProfileId::GfmFile,
        ProfileId::GfmComment,
        ProfileId::Extended,
    ] {
        let doc = build(RenderOptions::builtin(profile));
        let kinds: Vec<&str> = doc.render.items().iter().map(|i| i.kind.name()).collect();
        assert_eq!(kinds, ["paragraph", "math", "code"], "{profile}");
        let item = &doc.render.items()[1];
        assert_eq!(item.kind, ItemKind::Math { lines: 0..2 });
        let node = doc.render.node_of(1).expect("node");
        let NodeKind::Math(code) = &node.kind else {
            panic!("{:?}", node.kind)
        };
        assert_eq!(code.text, "\\frac{a}{b}\n+ c");
        assert_eq!(
            &DOC[item.source.clone()],
            "```math\n\\frac{a}{b}\n+ c\n```\n"
        );
        assert!(item.height_estimate(600.0) > 0.0);
    }
    // Obsidian has `$$` but no ` ```math `.
    let doc = build(RenderOptions::builtin(ProfileId::Obsidian));
    assert!(
        doc.render
            .nodes()
            .iter()
            .all(|n| !matches!(n.kind, NodeKind::Math(_)))
    );
}

#[test]
fn copy_and_find_see_the_tex_source() {
    let doc = build(RenderOptions::builtin(ProfileId::GfmFile));
    assert_eq!(doc.render.item_text(1).text, "\\frac{a}{b}\n+ c");
    let all = doc.render.plain_text(0..DOC.len(), CopyOptions::default());
    assert!(all.contains("\\frac{a}{b}"), "{all}");
}

#[test]
fn the_dump_names_math() {
    let doc = build(RenderOptions::builtin(ProfileId::Extended));
    let dump = doc.render.dump();
    assert!(dump.contains("math lines=2"), "{dump}");
}

#[test]
fn dollar_blocks_are_display_math() {
    for profile in [ProfileId::GfmFile, ProfileId::Obsidian] {
        let md = "a\n\n$$\nx^2\n\ny\n$$\n\nb $$z$$ c\n";
        let doc = build_in(md, profile);
        let kinds: Vec<&str> = doc.render.items().iter().map(|i| i.kind.name()).collect();
        assert_eq!(kinds, ["paragraph", "math", "paragraph"], "{profile}");
        let NodeKind::Math(code) = &doc.render.nodes()[1].kind else {
            panic!("math node")
        };
        assert_eq!(code.text, "x^2\n\ny");
        assert_eq!(
            &md[doc.render.items()[1].source.clone()],
            "$$\nx^2\n\ny\n$$\n"
        );
        // `$$` inside a paragraph: a display-style formula box.
        let c = doc.render.nodes()[2].inline().expect("inline");
        assert_eq!(c.math.len(), 1);
        assert!(c.math[0].display);
        assert_eq!(&*c.math[0].tex, "z");
    }
}

#[test]
fn inline_math_is_a_formula_box() {
    let md = "Let $x^2$ and $`y`$ be.\n";
    let doc = build_in(md, ProfileId::GfmFile);
    let node = &doc.render.nodes()[0];
    let c = node.inline().expect("inline");
    let tex: Vec<&str> = c.math.iter().map(|m| &*m.tex).collect();
    assert_eq!(tex, ["x^2", "y"]);
    assert!(c.math.iter().all(|m| !m.display));
    assert_eq!(&md[c.math[0].source.clone()], "$x^2$");
    let boxes: Vec<_> = c
        .paragraph
        .items()
        .iter()
        .zip(c.spans.iter())
        .filter_map(|(it, s)| it.inline_box().map(|b| (BoxRef::from_id(b.id), s.role)))
        .collect();
    assert_eq!(
        boxes,
        [
            (BoxRef::Math(0), InlineRole::Math(0)),
            (BoxRef::Math(1), InlineRole::Math(1))
        ]
    );
    // Copy sees the TeX.
    assert_eq!(doc.render.item_text(0).text, "Let x^2 and y be.");
    // CommonMark: text.
    let cm = build_in(md, ProfileId::CommonMark);
    assert!(
        cm.render.nodes()[0]
            .inline()
            .expect("inline")
            .math
            .is_empty()
    );
}

#[test]
fn macros_flow_in_document_order() {
    let md = "$\\gdef\\R{\\mathbb{R}}x$\n\n$$\n\\newcommand{\\N}{\\mathbb{N}}\\R\n$$\n\n| $\\N$ |\n| - |\n| $\\R$ |\n";
    let doc = build_in(md, ProfileId::GfmFile);
    let rt = &doc.render;
    let names = |node: usize, k: usize| -> Vec<String> {
        rt.math_macros(node, k)
            .iter()
            .map(|(n, _)| n.to_owned())
            .collect()
    };
    assert!(names(0, 0).is_empty());
    assert_eq!(names(1, 0), ["\\R"]);
    // The table's formulas, row by row.
    assert!(matches!(rt.nodes()[2].kind, NodeKind::Table(_)));
    assert_eq!(names(2, 0), ["\\N", "\\R"]);
    assert_eq!(names(2, 1), ["\\N", "\\R"]);
    // Past the last formula: none.
    assert!(names(2, 2).is_empty());
}

#[test]
fn edits_keep_math_trees_equal_to_full_builds() {
    let md = "a $x$ b\n\n$$\ny\n$$\n\nc\n";
    let mut doc = common::build_with(
        md,
        ParseOptions::builtin(ProfileId::GfmFile),
        RenderOptions::builtin(ProfileId::GfmFile),
        Sources::default(),
    );
    for (range, insert) in [(3..4, "z"), (9..9, "$$\n"), (0..0, "$$ q $$\n")] {
        doc.edit(range, insert, Sources::default());
        common::check(&doc);
        // A full build of the same (incrementally reparsed) syntax tree:
        // block ids of a fresh parse differ.
        let full = gv_render_tree::RenderTree::build(
            &doc.tree,
            &doc.model,
            doc.text.as_str(),
            RenderOptions::builtin(ProfileId::GfmFile),
            Sources::default(),
        );
        assert_eq!(common::canonical(&doc.render), common::canonical(&full));
    }
}
