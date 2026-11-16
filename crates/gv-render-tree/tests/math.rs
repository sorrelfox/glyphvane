// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Display math from ` ```math ` fences in profiles with math.

mod common;

use gv_render_tree::{CopyOptions, ItemKind, NodeKind, RenderOptions, Sources};
use gv_syntax::ParseOptions;

const DOC: &str = "Intro.\n\n```math\n\\frac{a}{b}\n+ c\n```\n\n```rust\nlet x = 1;\n```\n";

fn build(opts: RenderOptions) -> common::Doc {
    let doc = common::build_with(DOC, ParseOptions::default(), opts, Sources::default());
    common::check(&doc);
    doc
}

#[test]
fn commonmark_keeps_math_fences_as_code() {
    let doc = build(RenderOptions::for_profile("commonmark"));
    assert!(
        doc.render
            .nodes()
            .iter()
            .all(|n| !matches!(n.kind, NodeKind::Math(_)))
    );
}

#[test]
fn profiles_with_math_make_one_math_item() {
    for profile in ["gfm-file", "gfm-comment", "extended"] {
        let doc = build(RenderOptions::for_profile(profile));
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
}

#[test]
fn copy_and_find_see_the_tex_source() {
    let doc = build(RenderOptions::for_profile("gfm-file"));
    assert_eq!(doc.render.item_text(1).text, "\\frac{a}{b}\n+ c");
    let all = doc.render.plain_text(0..DOC.len(), CopyOptions::default());
    assert!(all.contains("\\frac{a}{b}"), "{all}");
}

#[test]
fn the_dump_names_math() {
    let doc = build(RenderOptions::for_profile("extended"));
    let dump = doc.render.dump();
    assert!(dump.contains("math lines=2"), "{dump}");
}
