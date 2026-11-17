// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live blocks: canvas and base embeds, ` ```base ` and ` ```query `
//! fences; their items, keys, reserved heights, profiles, copy and find,
//! and their place inside note embeds (host, depth limit, cycles).

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::{Doc, build_with, check};
use gv_render_tree::{
    CopyOptions, EmbedFetch, FindOptions, ItemKind, LIVE_EMBED_HEIGHT, LIVE_QUERY_HEIGHT,
    LinkStatus, LinkStatusSource, LinkTarget, LiveBlock, LiveKind, NodeKind, RenderOptions,
    RenderTree, Sources,
};
use gv_syntax::{ParseOptions, ProfileId};

/// Notes by name.
#[derive(Default)]
struct Vault {
    notes: HashMap<&'static str, &'static str>,
}

impl LinkStatusSource for Vault {
    fn status(&self, _: &LinkTarget<'_>) -> LinkStatus {
        LinkStatus::Exact
    }

    fn embed(&self, destination: &str, _from: Option<&str>) -> EmbedFetch {
        let name = destination.split(['#', '|']).next().unwrap_or("").trim();
        match self.notes.get(name) {
            Some(text) => EmbedFetch::Note {
                key: Arc::from(format!("{name}.md")),
                title: Arc::from(name),
                text: Arc::from(*text),
            },
            None => EmbedFetch::Unavailable,
        }
    }

    fn document_key(&self) -> Option<(Arc<str>, Arc<str>)> {
        Some((Arc::from("Doc.md"), Arc::from("Doc")))
    }
}

fn build(text: &str, profile: ProfileId, vault: &Vault) -> Doc {
    let d = build_with(
        text,
        ParseOptions::builtin(profile),
        RenderOptions::builtin(profile),
        Sources {
            links: vault,
            images: &gv_render_tree::NoImageSizes,
        },
    );
    check(&d);
    d
}

fn ob(text: &str) -> Doc {
    build(text, ProfileId::Obsidian, &Vault::default())
}

/// The live blocks of a tree, in order.
fn lives(rt: &RenderTree) -> Vec<Arc<LiveBlock>> {
    rt.items()
        .iter()
        .filter_map(|it| match (&it.kind, &rt.nodes()[it.node].kind) {
            (ItemKind::Live { kind }, NodeKind::Live(l)) => {
                assert_eq!(*kind, l.kind);
                Some(Arc::clone(l))
            }
            _ => None,
        })
        .collect()
}

/// Every item's text, one per line.
fn texts(rt: &RenderTree) -> String {
    (0..rt.items().len())
        .map(|ix| rt.item_text(ix).text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn canvas_and_base_embeds_are_live_items() {
    let d = ob("Intro.\n\n![[Plan.canvas]]\n\n![[Books.base#Shelf]]\n\n![[Note]]\n");
    let l = lives(&d.render);
    assert_eq!(l.len(), 2, "{}", d.render.dump());
    assert_eq!(l[0].kind, LiveKind::Canvas);
    assert_eq!(l[0].target.as_deref(), Some("Plan.canvas"));
    assert_eq!(l[0].anchor, None);
    assert_eq!(l[1].kind, LiveKind::Base);
    assert_eq!(l[1].target.as_deref(), Some("Books.base"));
    assert_eq!(l[1].anchor.as_deref(), Some("Shelf"));
    assert!(l.iter().all(|b| b.host.is_none() && b.depth == 0));
    // One item each, estimated at the reserved height.
    let it = d
        .render
        .items()
        .iter()
        .find(|it| matches!(it.kind, ItemKind::Live { .. }))
        .expect("live item");
    assert_eq!(it.estimate.height(600.0), LIVE_EMBED_HEIGHT);
    assert_eq!(it.estimate.height(200.0), LIVE_EMBED_HEIGHT);
    // The source is the embed's paragraph.
    assert_eq!(&d.text[it.source.clone()], "![[Plan.canvas]]\n");
}

#[test]
fn fences_are_live_where_the_profile_has_note_embeds() {
    let text = "```base\nviews:\n  - type: table\n```\n\n```query\ntag:#book\n```\n\n```rust\nfn x() {}\n```\n";
    let d = ob(text);
    let l = lives(&d.render);
    assert_eq!(l.len(), 2);
    assert_eq!(l[0].kind, LiveKind::Base);
    assert_eq!(l[0].fence_text(), Some("views:\n  - type: table"));
    assert_eq!(l[1].kind, LiveKind::Query);
    assert_eq!(l[1].fence_text(), Some("tag:#book"));
    let q = d
        .render
        .items()
        .iter()
        .find(|it| {
            it.kind
                == ItemKind::Live {
                    kind: LiveKind::Query,
                }
        })
        .expect("query");
    assert_eq!(q.estimate.height(600.0), LIVE_QUERY_HEIGHT);
    // CommonMark and GFM: code blocks; their embeds are not live either.
    for profile in [ProfileId::CommonMark, ProfileId::GfmFile] {
        let d = build(
            &format!("{text}\n![[Plan.canvas]]\n"),
            profile,
            &Vault::default(),
        );
        assert!(lives(&d.render).is_empty(), "{profile:?}");
    }
}

#[test]
fn keys_survive_rebuilds_and_edits_elsewhere() {
    let mut d = ob("![[Plan.canvas]]\n\nText.\n\n```query\nfoo\n```\n");
    let before: Vec<_> = lives(&d.render).iter().map(|l| l.key).collect();
    let ids: Vec<_> = d
        .render
        .items()
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Live { .. }))
        .map(|it| it.id)
        .collect();
    let vault = Vault::default();
    let at = d.text.find("Text").expect("text");
    d.edit(
        at..at,
        "More ",
        Sources {
            links: &vault,
            images: &gv_render_tree::NoImageSizes,
        },
    );
    check(&d);
    let after: Vec<_> = lives(&d.render).iter().map(|l| l.key).collect();
    assert_eq!(before, after);
    let ids_after: Vec<_> = d
        .render
        .items()
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Live { .. }))
        .map(|it| it.id)
        .collect();
    assert_eq!(ids, ids_after);
    // A rebuild at another zoom keeps the keys and scales the reserve.
    let z = build_with(
        &d.text,
        ParseOptions::builtin(ProfileId::Obsidian),
        RenderOptions::builtin(ProfileId::Obsidian).zoomed(2.0),
        Sources::default(),
    );
    let zk: Vec<_> = lives(&z.render).iter().map(|l| l.key).collect();
    assert_eq!(before, zk);
    let it = z
        .render
        .items()
        .iter()
        .find(|it| matches!(it.kind, ItemKind::Live { .. }))
        .expect("live");
    assert_eq!(it.estimate.height(600.0), 2.0 * LIVE_EMBED_HEIGHT);
    // Another query is another key.
    let other = ob("![[Plan.canvas]]\n\nText.\n\n```query\nbar\n```\n");
    assert_ne!(lives(&other.render)[1].key, before[1]);
}

#[test]
fn copy_and_find_see_the_embed_line_and_the_fence() {
    let d = ob("A\n\n![[Plan.canvas]]\n\n```query\ntag:#book\n```\n\nB\n");
    let all = d.render.plain_text(0..d.text.len(), CopyOptions::default());
    assert!(all.contains("Plan.canvas"), "{all:?}");
    assert!(all.contains("tag:#book"), "{all:?}");
    let find = |needle: &str| d.render.find(needle, FindOptions::default());
    let matches = find("#book");
    assert_eq!(matches.len(), 1);
    assert_eq!(&d.text[matches[0].source.clone()], "#book");
    let matches = find("Plan");
    assert_eq!(matches.len(), 1);
}

#[test]
fn inside_note_embeds_live_blocks_carry_their_host_and_the_depth_limit() {
    let mut vault = Vault::default();
    vault.notes.insert(
        "A",
        "A text\n\n![[Board.canvas]]\n\n```base\nviews: []\n```\n\n```query\nx\n```\n",
    );
    let d = build("![[A]]\n", ProfileId::Obsidian, &vault);
    let l = lives(&d.render);
    assert_eq!(l.len(), 3, "{}", d.render.dump());
    assert!(
        l.iter()
            .all(|b| b.host.as_deref() == Some("A.md") && b.depth == 1)
    );
    assert_eq!(l[0].kind, LiveKind::Canvas);
    assert_eq!(l[1].kind, LiveKind::Base);
    assert_eq!(l[1].fence_text(), Some("views: []"));
    assert_eq!(l[2].kind, LiveKind::Query);
    // Inside the embed's frame, mapped to the end of its line.
    let at = d.text.len();
    for it in d
        .render
        .items()
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Live { .. }))
    {
        assert_eq!(it.source, at..at);
        assert!(it.context.len() == 1);
    }
    // The same block in another note is another key.
    let mut v2 = Vault::default();
    v2.notes.insert("B", "![[Board.canvas]]\n");
    let d2 = build("![[B]]\n", ProfileId::Obsidian, &v2);
    assert_ne!(lives(&d2.render)[0].key, l[0].key);

    // A chain of embeds deep enough: the live block past the limit is a
    // notice.
    let mut vault = Vault::default();
    let names = ["N0", "N1", "N2", "N3", "N4"];
    for i in 0..4 {
        let text: &'static str = Box::leak(format!("n{i}\n\n![[{}]]\n", names[i + 1]).into());
        vault.notes.insert(names[i], text);
    }
    vault
        .notes
        .insert("N4", "```query\ndeep\n```\n\n![[Deep.canvas]]\n");
    let d = build("![[N0]]\n", ProfileId::Obsidian, &vault);
    assert!(lives(&d.render).is_empty(), "{}", d.render.dump());
    let all = texts(&d.render);
    assert!(all.contains("Embeds nested more than 5 deep"), "{all:?}");

    // A note embedding itself around a live block: the cycle stops, the
    // block shows once per level reached.
    let mut vault = Vault::default();
    vault
        .notes
        .insert("Doc", "```query\nself\n```\n\n![[Doc]]\n");
    let d = build(
        "```query\nself\n```\n\n![[Doc]]\n",
        ProfileId::Obsidian,
        &vault,
    );
    let all = texts(&d.render);
    assert!(all.contains("Embed cycle"), "{all:?}");
    assert_eq!(lives(&d.render).len(), 1);
}
