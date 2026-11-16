// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! ` ```mermaid ` fences become one diagram item in profiles with the
//! `mermaid` switch (code elsewhere), and `<picture>` asks the size of the
//! image it shows in each scheme.

mod common;

use gv_render_tree::{
    CopyOptions, ImageSize, ItemKind, LinkTarget, NodeKind, RenderOptions, Sources,
};
use gv_syntax::{ParseOptions, ProfileId};

const DOC: &str = "Intro.\n\n```mermaid\ngraph TD\n  A --> B\n```\n\n```rust\nlet x = 1;\n```\n";

fn build_in(text: &str, profile: ProfileId, sources: Sources<'_>) -> common::Doc {
    let doc = common::build_with(
        text,
        ParseOptions::builtin(profile),
        RenderOptions::builtin(profile),
        sources,
    );
    common::check(&doc);
    doc
}

#[test]
fn mermaid_fences_are_diagrams_where_the_profile_says() {
    let doc = build_in(DOC, ProfileId::CommonMark, Sources::default());
    let kinds: Vec<&str> = doc.render.items().iter().map(|i| i.kind.name()).collect();
    assert_eq!(kinds, ["paragraph", "code", "code"]);

    for profile in [ProfileId::GfmFile, ProfileId::Extended] {
        assert!(RenderOptions::builtin(profile).mermaid, "{profile}");
        let doc = build_in(DOC, profile, Sources::default());
        let kinds: Vec<&str> = doc.render.items().iter().map(|i| i.kind.name()).collect();
        assert_eq!(kinds, ["paragraph", "diagram", "code"], "{profile}");
        let item = &doc.render.items()[1];
        assert_eq!(item.kind, ItemKind::Diagram { lines: 0..2 });
        let NodeKind::Diagram(code) = &doc.render.node_of(1).expect("node").kind else {
            panic!("a diagram node");
        };
        assert_eq!(code.text, "graph TD\n  A --> B");
        // Before the view has the diagram: the source's height.
        let line = doc
            .render
            .options()
            .styles
            .role(gv_render_tree::TextRole::Code)
            .line_height;
        assert!(item.height_estimate(800.0) >= 2.0 * line);
        // Copy and find see the source.
        let text = doc.render.plain_text(0..DOC.len(), CopyOptions::default());
        assert!(text.contains("A --> B"), "{text:?}");
    }
}

#[test]
fn a_profile_without_mermaid_keeps_the_fence_as_code() {
    let mut opts = RenderOptions::builtin(ProfileId::GfmFile);
    opts.mermaid = false;
    let doc = common::build_with(
        DOC,
        ParseOptions::builtin(ProfileId::GfmFile),
        opts,
        Sources::default(),
    );
    common::check(&doc);
    assert!(
        doc.render
            .nodes()
            .iter()
            .all(|n| !matches!(n.kind, NodeKind::Diagram(_)))
    );
}

#[test]
fn pictures_are_sized_per_scheme() {
    let md = "<picture>\n  <source media=\"(prefers-color-scheme: dark)\" srcset=\"dark.png\">\n  <img alt=\"Logo\" src=\"light.png\">\n</picture>\n";
    let sizes = |t: &LinkTarget<'_>| {
        assert!(t.image);
        match t.destination {
            "light.png" => ImageSize::Known {
                width: 200.0,
                height: 100.0,
            },
            "dark.png" => ImageSize::Known {
                width: 100.0,
                height: 300.0,
            },
            _ => ImageSize::Failed,
        }
    };
    let sources = Sources {
        links: &gv_render_tree::AllPending,
        images: &sizes,
    };
    let doc = build_in(md, ProfileId::GfmFile, sources);
    let NodeKind::Picture(p) = &doc.render.nodes()[0].kind else {
        panic!("a picture node");
    };
    assert_eq!(
        p.size(false),
        ImageSize::Known {
            width: 200.0,
            height: 100.0
        }
    );
    assert_eq!(
        p.size(true),
        ImageSize::Known {
            width: 100.0,
            height: 300.0
        }
    );
    // The estimate is the light image's height.
    let item = &doc.render.items()[0];
    assert!((item.estimate.height(800.0) - 100.0).abs() < 0.01);
}
