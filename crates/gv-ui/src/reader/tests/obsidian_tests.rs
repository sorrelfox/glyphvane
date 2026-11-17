// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Obsidian constructs in the Reading view, in headless windows: a
//! wikilink click opens its note, a note embed shows the note (and stops
//! at a cycle), a foldable callout folds on a click, a tag click asks the
//! vault search for the tag.

use gv_render_tree::{FrameKind, InlineRole, NodeKind};

use super::*;
use crate::reader::state::ControlKind;

/// A vault folder (with `.obsidian/`) holding `files`.
fn vault(files: &[(&str, &str)]) -> tempfile::TempDir {
    let mut all: Vec<(&str, &str)> = vec![(".obsidian/app.json", "{}")];
    all.extend_from_slice(files);
    temp(&all)
}

fn settle(vcx: &mut VisualTestContext) {
    for _ in 0..4 {
        draw(vcx);
    }
}

/// The text of each painted item.
fn painted(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> Vec<String> {
    let st = state(reader, vcx);
    let st = st.borrow();
    let tree = &st.doc.as_ref().expect("loaded").tree;
    st.geometry
        .items
        .iter()
        .map(|g| tree.item_text(g.ix).text.clone())
        .collect()
}

/// The centre of the first painted piece of role `role` in the item that
/// holds `needle`.
fn piece_of(st: &State, needle: &str, role: InlineRole) -> Point<Pixels> {
    let doc = st.doc.as_ref().expect("loaded");
    let offset = doc.text.find(needle).expect("needle in the document");
    for g in &st.geometry.items {
        let node = doc.tree.node_of(g.ix).expect("node");
        if !(node.source.start <= offset && offset < node.source.end) {
            continue;
        }
        let content = node.inline().expect("inline content");
        let piece = content
            .spans
            .iter()
            .position(|s| s.role == role)
            .expect("the piece");
        let block = g.text.as_ref().expect("text").borrow();
        let layout = block.layout().expect("laid out");
        let origin = block.bounds().expect("painted").origin;
        let r = layout.item_rects(piece)[0];
        return origin + point(px(r.x + r.width / 2.0), px(r.y + r.height / 2.0));
    }
    panic!("{needle:?} is not on screen");
}

#[gpui::test]
fn the_vault_reads_in_the_obsidian_profile(cx: &mut TestAppContext) {
    let dir = vault(&[("a.md", "Text ==marked== %%hidden%% end.\n")]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    assert_eq!(
        reader.read_with(&vcx, |r, _| r.profile()),
        gv_profile::ProfileId::Obsidian
    );
    assert_eq!(painted(&reader, &mut vcx), ["Text marked  end."]);
}

#[gpui::test]
fn a_click_on_a_wikilink_opens_its_note(cx: &mut TestAppContext) {
    let dir = vault(&[
        ("a.md", "# A\n\nGo to [[b#Second|the second note]] now.\n"),
        ("b.md", "# B\n\n## Second\n\nText.\n"),
    ]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    let at = link_point(&st.borrow(), "the second note");
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(workspace.read_with(&vcx, |ws, _| ws.tabs().len()), 1);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");
}

#[gpui::test]
fn a_note_embed_shows_the_note_and_stops_at_a_cycle(cx: &mut TestAppContext) {
    let dir = vault(&[
        ("a.md", "# A\n\n![[b]]\n\nAfter.\n"),
        ("b.md", "B body.\n\n![[a]]\n"),
    ]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    let texts = painted(&reader, &mut vcx);
    assert_eq!(
        texts,
        [
            "A",
            "b",
            "B body.",
            "\u{29c9} a",
            "Embed cycle: a \u{2192} b \u{2192} a",
            "After."
        ]
    );
    // The embedded blocks are in the embed's frame.
    let st = state(&reader, &mut vcx);
    let st = st.borrow();
    let tree = &st.doc.as_ref().expect("loaded").tree;
    assert!(
        tree.items()[2]
            .context
            .iter()
            .any(|f| matches!(f.kind, FrameKind::Embed { depth: 0 }))
    );
}

#[gpui::test]
fn a_foldable_callout_folds_on_a_click(cx: &mut TestAppContext) {
    let dir = vault(&[("c.md", "> [!faq]- Question?\n> The answer.\n\nAfter.\n")]);
    let (window, workspace) = open(cx, &dir.path().join("c.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    // Folded at first (`-`): the title and what follows.
    assert_eq!(painted(&reader, &mut vcx), ["Question?", "After."]);
    {
        let st = state(&reader, &mut vcx);
        let st = st.borrow();
        let tree = &st.doc.as_ref().expect("loaded").tree;
        let title = tree.node_of(st.geometry.items[0].ix).expect("node");
        assert!(matches!(title.kind, NodeKind::CalloutTitle { .. }));
    }
    let row = |vcx: &mut VisualTestContext| {
        let st = state(&reader, vcx);
        let st = st.borrow();
        st.geometry
            .controls
            .iter()
            .find(|c| matches!(c.kind, ControlKind::Details { .. }))
            .map(|c| c.bounds.center())
            .expect("the title row folds")
    };
    let at = row(&mut vcx);
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(
        painted(&reader, &mut vcx),
        ["Question?", "The answer.", "After."]
    );
    let at = row(&mut vcx);
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(painted(&reader, &mut vcx), ["Question?", "After."]);
}

#[gpui::test]
fn a_click_on_a_tag_asks_for_a_search(cx: &mut TestAppContext) {
    let dir = vault(&[("t.md", "Some text #project/alpha here.\n")]);
    let (window, workspace) = open(cx, &dir.path().join("t.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    let st = state(&reader, &mut vcx);
    let at = piece_of(&st.borrow(), "#project", InlineRole::Tag);
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    // The window's `search.vault` handler took it: the Search panel
    // searches for it.
    assert_eq!(st.borrow().tag_search.as_deref(), None);
    let query = workspace.read_with(&vcx, |ws, cx| ws.search_query(cx));
    assert_eq!(query.as_deref(), Some("tag:#project/alpha"));
    // No navigation.
    assert_eq!(file_of(&workspace, &mut vcx), "t.md");
}
