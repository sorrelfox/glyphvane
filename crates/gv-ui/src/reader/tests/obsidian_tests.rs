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
    assert_eq!(painted(&reader, &mut vcx), ["Text marked end."]);
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
            "a",
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

/// The centre of the first link piece of the painted item whose text
/// holds `needle` (an embedded note's item: not in the document's text).
fn embedded_link(st: &State, needle: &str) -> (usize, Point<Pixels>) {
    let doc = st.doc.as_ref().expect("loaded");
    for g in &st.geometry.items {
        if !doc.tree.item_text(g.ix).text.contains(needle) {
            continue;
        }
        let content = doc.tree.item_inline(g.ix).expect("inline content");
        let piece = content
            .spans
            .iter()
            .position(|s| s.link.is_some() && s.role == InlineRole::Text)
            .expect("a link");
        let block = g.text.as_ref().expect("text").borrow();
        let layout = block.layout().expect("laid out");
        let origin = block.bounds().expect("painted").origin;
        let r = layout.item_rects(piece)[0];
        return (
            g.ix,
            origin + point(px(r.x + r.width / 2.0), px(r.y + r.height / 2.0)),
        );
    }
    panic!("{needle:?} is not on screen");
}

#[gpui::test]
fn links_inside_a_note_embed_resolve_and_follow_from_the_embedded_note(cx: &mut TestAppContext) {
    // `./d.md` is beside the embedded note, not beside the document.
    let dir = vault(&[
        ("a.md", "# A\n\n![[sub/b]]\n"),
        ("sub/b.md", "B has **bold** and [the d note](./d.md).\n"),
        ("sub/d.md", "# D\n"),
    ]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    settle(&mut vcx);
    let reader = reader(&workspace, &mut vcx);
    assert_eq!(
        painted(&reader, &mut vcx),
        ["A", "sub/b", "B has bold and the d note."]
    );
    let st = state(&reader, &mut vcx);
    let (item, at) = embedded_link(&st.borrow(), "the d note");
    {
        let s = st.borrow();
        let link = &s
            .doc
            .as_ref()
            .expect("loaded")
            .tree
            .item_inline(item)
            .expect("inline")
            .links[0];
        assert_eq!(link.from.as_deref(), Some("sub/b.md"));
        // Pending until the vault index exists; resolved from the
        // embedded note's folder, not the document's.
        assert_ne!(link.status, gv_render_tree::LinkStatus::Unresolved);
        let resolver = crate::reader::Resolver::for_click(
            &dir.path().join("a.md"),
            None,
            gv_profile::ProfileId::Obsidian,
        );
        let kind = gv_vault::LinkKind::Inline;
        let r = resolver.resolve_from(kind, "./d.md", Some("sub/b.md"));
        assert!(r.status.is_resolved(), "{r:?}");
        assert!(!resolver.resolve(kind, "./d.md").status.is_resolved());
        let hit = s.link_at(at).expect("the link under the pointer");
        assert_eq!((hit.range, hit.from.as_deref()), (None, Some("sub/b.md")));
    }
    // Hovering it: the link the window previews is the embedded note's.
    vcx.simulate_mouse_move(at, None, Modifiers::none());
    draw(&mut vcx);
    let hovered = reader
        .read_with(&vcx, |r, _| r.hovered_link())
        .expect("a hovered link");
    assert!(hovered.file.ends_with("sub/b.md"), "{:?}", hovered.file);
    // Mod+click: a new tab with the note beside the embedded one.
    let secondary = vcx.update(|_, cx| Modifiers::secondary_key(cx));
    vcx.simulate_click(at, secondary);
    settle(&mut vcx);
    assert_eq!(workspace.read_with(&vcx, |ws, _| ws.tabs().len()), 2);
    let path = workspace.read_with(&vcx, |ws, _| ws.active_tab().path().map(Path::to_path_buf));
    assert!(path.is_some_and(|p| p.ends_with("sub/d.md")));
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

/// The kind of node each painted item shows.
fn painted_kinds(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> Vec<&'static str> {
    let st = state(reader, vcx);
    let st = st.borrow();
    let tree = &st.doc.as_ref().expect("loaded").tree;
    st.geometry
        .items
        .iter()
        .map(|g| match tree.node_of(g.ix).map(|n| &n.kind) {
            Some(NodeKind::Code(_)) => "code",
            Some(NodeKind::Table(_)) => "table",
            Some(NodeKind::CalloutTitle { .. }) => "callout",
            Some(NodeKind::Paragraph(_)) => "paragraph",
            Some(NodeKind::Heading { .. }) => "heading",
            _ => "other",
        })
        .collect()
}

#[gpui::test]
fn blocks_inside_a_note_embed_are_drawn_and_a_callout_folds(cx: &mut TestAppContext) {
    let dir = vault(&[
        ("a.md", "# A\n\n![[b]]\n\nAfter.\n"),
        (
            "b.md",
            "```rust\nfn main() {}\n```\n\n| x | y |\n|---|---|\n| 1 | 2 |\n\n\
             > [!faq]- Question?\n> The answer.\n",
        ),
    ]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    settle(&mut vcx);
    // The code block and the table rows are drawn as such, in the embed;
    // the callout is folded (`-`).
    assert_eq!(
        painted_kinds(&reader, &mut vcx),
        [
            "heading",
            "paragraph",
            "code",
            "table",
            "table",
            "callout",
            "paragraph"
        ]
    );
    {
        let st = state(&reader, &mut vcx);
        let st = st.borrow();
        let tree = &st.doc.as_ref().expect("loaded").tree;
        for g in &st.geometry.items[2..6] {
            assert!(
                tree.items()[g.ix]
                    .context
                    .iter()
                    .any(|f| matches!(f.kind, FrameKind::Embed { depth: 0 })),
                "item {} is in the embed",
                g.ix
            );
        }
    }
    let texts = painted(&reader, &mut vcx);
    assert!(texts[2].contains("fn main() {}"), "{texts:?}");
    assert!(!texts.iter().any(|t| t == "The answer."), "{texts:?}");
    let row = |vcx: &mut VisualTestContext| {
        let st = state(&reader, vcx);
        let st = st.borrow();
        st.geometry
            .controls
            .iter()
            .find(|c| matches!(c.kind, ControlKind::Details { .. }))
            .map(|c| c.bounds.center())
            .expect("the embedded callout's title row folds")
    };
    let at = row(&mut vcx);
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    let texts = painted(&reader, &mut vcx);
    assert!(texts.iter().any(|t| t == "The answer."), "{texts:?}");
    let at = row(&mut vcx);
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    let texts = painted(&reader, &mut vcx);
    assert!(!texts.iter().any(|t| t == "The answer."), "{texts:?}");
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

#[gpui::test]
fn callout_folds_are_named_for_the_session_and_come_back_after_an_edit(cx: &mut TestAppContext) {
    use crate::reader::fold_state::{FoldKind, ReadingFold};

    let path_text = "> [!faq]- Question?\n> The answer.\n\nAfter.\n";
    let dir = vault(&[("c.md", path_text)]);
    let path = dir.path().join("c.md");
    let (window, workspace) = open(cx, &path);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    assert!(reader.read_with(&vcx, |r, _| r.reading_folds()).is_empty());
    let at = {
        let st = state(&reader, &mut vcx);
        let st = st.borrow();
        st.geometry
            .controls
            .iter()
            .find(|c| matches!(c.kind, ControlKind::Details { .. }))
            .map(|c| c.bounds.center())
            .expect("the title row folds")
    };
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    let folds = reader.read_with(&vcx, |r, _| r.reading_folds());
    assert_eq!(
        folds,
        [ReadingFold {
            kind: FoldKind::Callout,
            line: "Question?".into(),
            nth: 0,
            open: true,
            at: Some([0, 1]),
        }]
    );

    // The note changed above the callout; its fold comes back by name.
    std::fs::write(&path, format!("# Added\n\nText.\n\n{path_text}")).expect("write");
    state(&reader, &mut vcx).borrow_mut().cache.folds.clear();
    reader.update(&mut vcx, |r, cx| {
        r.open(path.clone(), None, cx);
        r.restore_reading_folds(&path, folds.clone(), cx);
    });
    settle(&mut vcx);
    assert!(
        painted(&reader, &mut vcx)
            .iter()
            .any(|t| t == "The answer."),
        "{:?}",
        painted(&reader, &mut vcx)
    );
    assert_eq!(reader.read_with(&vcx, |r, _| r.reading_folds()), folds);

    // The callout's title was renamed: its fold comes back by its place.
    std::fs::write(&path, path_text.replace("Question?", "Renamed?")).expect("write");
    state(&reader, &mut vcx).borrow_mut().cache.folds.clear();
    reader.update(&mut vcx, |r, cx| {
        r.open(path.clone(), None, cx);
        r.restore_reading_folds(&path, folds.clone(), cx);
    });
    settle(&mut vcx);
    assert!(
        painted(&reader, &mut vcx)
            .iter()
            .any(|t| t == "The answer."),
        "{:?}",
        painted(&reader, &mut vcx)
    );
    let renamed = reader.read_with(&vcx, |r, _| r.reading_folds());
    assert_eq!(renamed.len(), 1);
    assert_eq!(renamed[0].line, "Renamed?");
    assert!(renamed[0].open);
}

/// The centre of the first painted piece of role `role` (by `pick`) in
/// any item, and the item.
fn piece_where(st: &State, pick: impl Fn(InlineRole) -> bool) -> (usize, Point<Pixels>) {
    let doc = st.doc.as_ref().expect("loaded");
    for g in &st.geometry.items {
        let Some(content) = doc.tree.item_inline(g.ix) else {
            continue;
        };
        let Some(piece) = content.spans.iter().position(|s| pick(s.role)) else {
            continue;
        };
        let block = g.text.as_ref().expect("text").borrow();
        let layout = block.layout().expect("laid out");
        let origin = block.bounds().expect("painted").origin;
        let r = layout.item_rects(piece)[0];
        return (
            g.ix,
            origin + point(px(r.x + r.width / 2.0), px(r.y + r.height / 2.0)),
        );
    }
    panic!("no such piece on screen");
}

#[gpui::test]
fn footnotes_inside_a_note_embed_jump_to_its_list_and_back(cx: &mut TestAppContext) {
    use crate::reader::state::FootnoteHit;

    let dir = vault(&[
        ("a.md", "# A\n\n![[b]]\n\nAfter.\n"),
        ("b.md", "Text[^x] here.\n\n[^x]: The note.\n"),
    ]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    settle(&mut vcx);
    let st = state(&reader, &mut vcx);
    let st = st.borrow_mut();
    let (from, at) = piece_where(&st, |r| matches!(r, InlineRole::FootnoteRef(_)));
    let hit = st.footnote_at(at).expect("a reference");
    assert_eq!(
        hit,
        FootnoteHit::EmbedRef {
            item: from,
            number: 1
        }
    );
    let (entry, back) = piece_where(&st, |r| matches!(r, InlineRole::FootnoteBackref(_)));
    let tree = &st.doc.as_ref().expect("loaded").tree;
    assert!(tree.item_text(entry).text.starts_with("The note."));
    assert_eq!(tree.embed_footnote_item(from, 1), Some(entry));
    let back_hit = st.footnote_at(back).expect("a backreference");
    assert_eq!(back_hit, FootnoteHit::EmbedBack { item: entry, k: 0 });
    assert_eq!(tree.embed_footnote_refs(entry), [from]);
    // The host's footnote section is not involved.
    assert_eq!(tree.footnote_item("x"), None);
    drop(st);
    let st = state(&reader, &mut vcx);
    assert!(st.borrow_mut().follow_footnote(&hit));
    assert!(st.borrow_mut().follow_footnote(&back_hit));
}

#[gpui::test]
fn callout_folds_inside_a_note_embed_are_named_for_the_session(cx: &mut TestAppContext) {
    use crate::reader::fold_state::{FoldKind, ReadingFold};

    let dir = vault(&[
        ("a.md", "![[b]]\n\nAfter.\n"),
        ("b.md", "> [!faq]- Question?\n> The answer.\n"),
    ]);
    let path = dir.path().join("a.md");
    let (window, workspace) = open(cx, &path);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    settle(&mut vcx);
    let at = {
        let st = state(&reader, &mut vcx);
        let st = st.borrow();
        st.geometry
            .controls
            .iter()
            .find(|c| matches!(c.kind, ControlKind::Details { .. }))
            .map(|c| c.bounds.center())
            .expect("the embedded callout's title row folds")
    };
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    let folds = reader.read_with(&vcx, |r, _| r.reading_folds());
    assert_eq!(
        folds,
        [ReadingFold {
            kind: FoldKind::Callout,
            line: "![[b]] \u{203a} Question?".into(),
            nth: 0,
            open: true,
            at: Some([0, 1]),
        }]
    );
    // The embedded note changed above the callout, the document is read
    // again: the fold comes back.
    std::fs::write(
        dir.path().join("b.md"),
        "Added.\n\n> [!faq]- Question?\n> The answer.\n",
    )
    .expect("write");
    state(&reader, &mut vcx).borrow_mut().cache.folds.clear();
    reader.update(&mut vcx, |r, cx| {
        r.open(path.clone(), None, cx);
        r.restore_reading_folds(&path, folds.clone(), cx);
    });
    settle(&mut vcx);
    let texts = painted(&reader, &mut vcx);
    assert!(texts.iter().any(|t| t == "The answer."), "{texts:?}");
    assert_eq!(reader.read_with(&vcx, |r, _| r.reading_folds()), folds);
}
