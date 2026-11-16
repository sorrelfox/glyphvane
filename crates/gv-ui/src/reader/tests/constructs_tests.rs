// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! GitHub constructs in the Reading view, in headless windows: task
//! checkboxes write back through the tab's Source view (one undo step, in
//! Preview and in Split, never in viewer mode), footnote references jump
//! to the footnote and its `↩` back, `<details>` fold, alerts get their
//! box and title row.

use gv_render_tree::{FrameKind, InlineRole, ItemKind, ListMarker};

use super::*;
use crate::reader::state::{ControlKind, FootnoteHit, Hover};
use crate::settings::Target;
use crate::shell::ViewMode;

/// The centre of the `n`th control of kind `pick` the last frame drew.
fn control(st: &State, n: usize, pick: impl Fn(&ControlKind) -> bool) -> Point<Pixels> {
    st.geometry
        .controls
        .iter()
        .filter(|c| pick(&c.kind))
        .nth(n)
        .map(|c| c.bounds.center())
        .expect("the control is drawn")
}

fn task(st: &State, n: usize) -> Point<Pixels> {
    control(st, n, |k| matches!(k, ControlKind::Task { .. }))
}

/// Whether each task item of the shown document is checked, in order.
fn checked(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> Vec<bool> {
    let st = state(reader, vcx);
    let st = st.borrow();
    let doc = st.doc.as_ref().expect("loaded");
    doc.tree
        .items()
        .iter()
        .filter_map(|it| {
            it.context.iter().rev().find_map(|f| match f.kind {
                FrameKind::Item {
                    marker: ListMarker::Task { checked, .. },
                } if it.opens_frame(it.context.len() - 1) => Some(checked),
                _ => None,
            })
        })
        .collect()
}

fn editor_text(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Option<String> {
    workspace.read_with(vcx, |ws, cx| {
        let editor = ws.active_tab().editor()?;
        let text = editor.read(cx).editor()?.text();
        text.text_for_range(0..text.len()).ok()
    })
}

fn settle(vcx: &mut VisualTestContext) {
    for _ in 0..4 {
        draw(vcx);
    }
}

const TASKS: &str = "# Tasks\n\n- [ ] first\n- [x] second\n- plain\n";

#[gpui::test]
fn a_task_click_in_preview_writes_back_and_undoes(cx: &mut TestAppContext) {
    let dir = temp(&[("tasks.md", TASKS)]);
    let file = dir.path().join("tasks.md");
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    assert_eq!(checked(&reader, &mut vcx), [false, true]);
    // No Source view yet: the click makes one, loads the file and writes
    // the toggle back once it is in.
    assert!(editor_text(&workspace, &mut vcx).is_none());
    let at = task(&state(&reader, &mut vcx).borrow(), 0);
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(
        editor_text(&workspace, &mut vcx).as_deref(),
        Some("# Tasks\n\n- [x] first\n- [x] second\n- plain\n")
    );
    // The Reading view shows the edit, in place; the file is not saved.
    assert_eq!(checked(&reader, &mut vcx), [true, true]);
    assert_eq!(std::fs::read_to_string(&file).expect("file"), TASKS);
    // The second box unchecks.
    let at = task(&state(&reader, &mut vcx).borrow(), 1);
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(checked(&reader, &mut vcx), [true, false]);
    // Each toggle is one undo step in the Source view.
    workspace.update(&mut vcx, |ws, cx| ws.show_mode(ViewMode::Edit, cx));
    settle(&mut vcx);
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(
        editor_text(&workspace, &mut vcx).as_deref(),
        Some("# Tasks\n\n- [x] first\n- [x] second\n- plain\n")
    );
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(editor_text(&workspace, &mut vcx).as_deref(), Some(TASKS));
    workspace.update(&mut vcx, |ws, cx| ws.show_mode(ViewMode::Preview, cx));
    settle(&mut vcx);
    assert_eq!(checked(&reader, &mut vcx), [false, true]);
}

#[gpui::test]
fn a_task_click_in_split_edits_the_split_editor(cx: &mut TestAppContext) {
    let dir = temp(&[("tasks.md", TASKS)]);
    let (window, workspace) = open(cx, &dir.path().join("tasks.md"));
    let mut vcx = drawn(window, cx);
    workspace.update(&mut vcx, |ws, cx| ws.show_mode(ViewMode::Split, cx));
    settle(&mut vcx);
    let preview = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().split().cloned())
        .expect("a Split view")
        .read_with(&vcx, |s, _| s.reader().clone());
    let at = task(&state(&preview, &mut vcx).borrow(), 0);
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(
        editor_text(&workspace, &mut vcx).as_deref(),
        Some("# Tasks\n\n- [x] first\n- [x] second\n- plain\n")
    );
    // The pane follows the editor.
    assert_eq!(checked(&preview, &mut vcx), [true, true]);
}

#[gpui::test]
fn viewer_mode_leaves_tasks_alone(cx: &mut TestAppContext) {
    let dir = temp(&[("tasks.md", TASKS)]);
    let (window, workspace) = open(cx, &dir.path().join("tasks.md"));
    let mut vcx = drawn(window, cx);
    vcx.update(|_, cx| {
        AppSettings::set_value(
            cx,
            Target::AllVaults,
            "general.viewer_mode",
            &toml::Value::Boolean(true),
        )
        .expect("a valid setting");
    });
    settle(&mut vcx);
    let reader = reader(&workspace, &mut vcx);
    let at = task(&state(&reader, &mut vcx).borrow(), 0);
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(checked(&reader, &mut vcx), [false, true]);
    let text = editor_text(&workspace, &mut vcx);
    assert!(text.is_none() || text.as_deref() == Some(TASKS));
}

/// The middle of the first piece of item `ix` with role `pick`.
fn piece_point(st: &State, ix: usize, pick: impl Fn(InlineRole) -> bool) -> Point<Pixels> {
    let doc = st.doc.as_ref().expect("loaded");
    let content = doc.tree.item_inline(ix).expect("inline content");
    let piece = content
        .spans
        .iter()
        .position(|s| pick(s.role))
        .expect("the piece");
    let g = st
        .geometry
        .items
        .iter()
        .find(|g| g.ix == ix)
        .expect("the item is on screen");
    let block = g.text.as_ref().expect("text").borrow();
    let layout = block.layout().expect("laid out");
    let origin = block.bounds().expect("placed").origin;
    let frag = layout
        .fragments()
        .iter()
        .find(|f| f.item == piece)
        .expect("a fragment");
    let line = layout
        .lines()
        .iter()
        .find(|l| {
            let k = layout.fragments().iter().position(|f| f.item == piece);
            k.is_some_and(|k| l.frags.contains(&k))
        })
        .expect("its line");
    origin
        + point(
            px(frag.x + frag.width / 2.0),
            px(line.top + line.height / 2.0),
        )
}

/// The index of the first item at the top of the view.
fn top_item(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> usize {
    let st = state(reader, vcx);
    let st = st.borrow();
    let anchor = st.engine.anchor().expect("an anchor");
    st.doc
        .as_ref()
        .expect("loaded")
        .tree
        .index_of(anchor.id)
        .expect("the anchor's item")
}

#[gpui::test]
fn footnote_references_jump_to_the_footnote_and_back(cx: &mut TestAppContext) {
    let mut doc = String::from("# Notes\n\nSee the note[^n] here.\n\n");
    for i in 0..60 {
        doc.push_str(&format!("Filler paragraph {i} with some words.\n\n"));
    }
    doc.push_str("[^n]: The footnote text.\n");
    let dir = temp(&[("notes.md", &doc)]);
    let (window, workspace) = open(cx, &dir.path().join("notes.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    let (para, note) = {
        let st = state(&reader, &mut vcx);
        let st = st.borrow();
        let tree = &st.doc.as_ref().expect("loaded").tree;
        (
            1,
            tree.footnote_item("n")
                .expect("the footnote in the section"),
        )
    };
    let at = piece_point(&state(&reader, &mut vcx).borrow(), para, |r| {
        matches!(r, InlineRole::FootnoteRef(_))
    });
    let hover = state(&reader, &mut vcx).borrow().hover_at(at);
    assert_eq!(
        hover,
        Hover::Footnote(FootnoteHit::Ref {
            label: std::sync::Arc::from("n")
        })
    );
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    let st = state(&reader, &mut vcx);
    assert!(
        st.borrow().geometry.items.iter().any(|g| g.ix == note),
        "the footnote is on screen"
    );
    assert!(top_item(&reader, &mut vcx) > para);
    // Its ↩ leads back to the reference.
    let back = piece_point(&st.borrow(), note, |r| {
        matches!(r, InlineRole::FootnoteBackref(0))
    });
    vcx.simulate_click(back, Modifiers::none());
    settle(&mut vcx);
    assert!(
        state(&reader, &mut vcx)
            .borrow()
            .geometry
            .items
            .iter()
            .any(|g| g.ix == para),
        "the reference is on screen again"
    );
}

#[gpui::test]
fn details_fold_and_unfold(cx: &mut TestAppContext) {
    let doc = "# D\n\n<details>\n<summary>More</summary>\n\nHidden body.\n\n</details>\n\nAfter.\n";
    let dir = temp(&[("d.md", doc)]);
    let (window, workspace) = open(cx, &dir.path().join("d.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    let painted = |vcx: &mut VisualTestContext| -> Vec<ItemKind> {
        let st = state(&reader, vcx);
        let st = st.borrow();
        let tree = &st.doc.as_ref().expect("loaded").tree;
        st.geometry
            .items
            .iter()
            .map(|g| tree.items()[g.ix].kind.clone())
            .collect()
    };
    // Closed: the summary row and what follows the details.
    assert_eq!(
        painted(&mut vcx),
        [
            ItemKind::Heading { level: 1 },
            ItemKind::Details,
            ItemKind::Paragraph
        ]
    );
    let row = control(&state(&reader, &mut vcx).borrow(), 0, |k| {
        matches!(k, ControlKind::Details { .. })
    });
    vcx.simulate_click(row, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(painted(&mut vcx).len(), 4, "the body shows");
    let row = control(&state(&reader, &mut vcx).borrow(), 0, |k| {
        matches!(k, ControlKind::Details { .. })
    });
    vcx.simulate_click(row, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(painted(&mut vcx).len(), 3, "folded again");
}

#[gpui::test]
fn alerts_reserve_their_title_row(cx: &mut TestAppContext) {
    let doc = "# A\n\n> [!NOTE]\n> Useful information.\n\nAfter.\n";
    let dir = temp(&[("a.md", doc)]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    let painted = reader.read_with(&vcx, |r, _| r.painted_items());
    let st = state(&reader, &mut vcx);
    let st = st.borrow();
    let tree = &st.doc.as_ref().expect("loaded").tree;
    let body = tree
        .items()
        .iter()
        .position(|it| {
            matches!(
                it.context.first().map(|f| &f.kind),
                Some(FrameKind::Alert { .. })
            )
        })
        .expect("the alert's paragraph");
    let m = tree.options().metrics;
    let (above, below) = tree.items()[body].frame_insets();
    assert!(above >= m.callout_padding_top + m.callout_title);
    assert!(below >= m.callout_padding_bottom);
    // The alert's text sits below its title row, indented by the padding.
    let g = st
        .geometry
        .items
        .iter()
        .find(|g| g.ix == body)
        .expect("on screen");
    let block = g.text.as_ref().expect("text").borrow();
    let origin = block.bounds().expect("placed").origin;
    let (x0, _) = st.geometry.column;
    assert!((f32::from(origin.x) - x0 - m.callout_padding_x).abs() < 0.5);
    let top_of_box = g.top + tree.items()[body].gap_before;
    assert!(f32::from(origin.y) >= top_of_box + m.callout_padding_top + m.callout_title);
    assert_eq!(painted.len(), 3);
}

#[gpui::test]
fn front_matter_shows_keys_in_their_column(cx: &mut TestAppContext) {
    let doc = "---\ntitle: Notes\ntags: [links, navigation]\n---\n# Heading\n";
    let dir = temp(&[("p.md", doc)]);
    let (window, workspace) = open(cx, &dir.path().join("p.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    let st = state(&reader, &mut vcx);
    let st = st.borrow();
    let tree = &st.doc.as_ref().expect("loaded").tree;
    let ix = tree
        .items()
        .iter()
        .position(|it| matches!(it.kind, ItemKind::Properties { .. }))
        .expect("the properties");
    let g = st
        .geometry
        .items
        .iter()
        .find(|g| g.ix == ix)
        .expect("on screen");
    let block = g.text.as_ref().expect("values").borrow();
    assert_eq!(block.paragraph().text(), "Notes\n#links\u{a0}#navigation");
    let m = tree.options().metrics;
    let (x0, _) = st.geometry.column;
    let origin = block.bounds().expect("placed").origin;
    assert!((f32::from(origin.x) - x0 - m.property_key - m.property_gap).abs() < 0.5);
}
