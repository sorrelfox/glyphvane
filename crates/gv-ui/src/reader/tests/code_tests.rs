// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Code highlighting in the Reading view and the Split view's preview, in
//! headless windows: blocks are drawn plain while their job runs on the
//! background executor (a job that runs inside a frame fails the test),
//! then laid out again with the classes' colours; unknown languages stay
//! plain.
//!
//! Each test uses code of its own: the highlighter's cache is shared by
//! the whole process.

use gpui::Focusable as _;
use gv_inline::ColorRole;
use gv_render_tree::{ColorToken, TextRole, Variant};

use super::*;
use crate::inline::Token;
use crate::reader::code_highlight::tests::{jobs_run, pending_draws};
use crate::reader::items::is_code;
use crate::shell::ViewMode;

/// The colour of the first `needle` in the code items laid out.
fn colour(reader: &Entity<Reader>, vcx: &mut VisualTestContext, needle: &str) -> ColorRole {
    let st = state(reader, vcx);
    let st = st.borrow();
    let doc = st.doc.as_ref().expect("loaded");
    for item in doc.tree.items() {
        if !is_code(&item.kind) {
            continue;
        }
        let Some(block) = st.cache.cached(item.id) else {
            continue;
        };
        let block = block.borrow();
        let p = block.paragraph();
        if let Some(at) = p.text().find(needle) {
            let ix = p.item_at(at).expect("an item");
            return p.items()[ix].style().expect("a run").color;
        }
    }
    panic!("{needle:?} is not in a code item laid out");
}

fn plain() -> ColorRole {
    gv_render_tree::StyleTable::preview()
        .run_style(TextRole::Code, Variant::PLAIN)
        .color
}

const DOC: &str = "# Code\n\n```rust\nfn reading_view_fence() -> u32 { 42 }\n```\n\n```nosuchlanguage\nfn unknown() {}\n```\n\n    fn indented() {}\n";

#[gpui::test]
fn blocks_are_drawn_plain_then_highlighted_when_the_job_lands(cx: &mut TestAppContext) {
    let dir = temp(&[("a.md", DOC)]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let (jobs, pending) = (jobs_run(), pending_draws());
    // Frames run no job (a job inside a frame fails the test): the first
    // frame draws the block plain and asks for one job (the other blocks
    // have no known language), which lands, and the block is laid out
    // again.
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    draw(&mut vcx);
    assert_eq!(jobs_run(), jobs + 1);
    assert_eq!(pending_draws(), pending + 1, "drawn plain while pending");
    assert_eq!(
        colour(&reader, &mut vcx, "fn reading"),
        Token::SyntaxTag.role()
    );
    assert_eq!(
        colour(&reader, &mut vcx, "reading_view_fence"),
        Token::SyntaxKey.role()
    );
    assert_eq!(colour(&reader, &mut vcx, "42"), Token::SyntaxNumber.role());
    assert_eq!(colour(&reader, &mut vcx, "fn unknown"), plain());
    assert_eq!(colour(&reader, &mut vcx, "fn indented"), plain());
    assert_eq!(plain(), ColorToken::TextPrimary.role());
    draw(&mut vcx);
    assert_eq!(jobs_run(), jobs + 1);
}

#[gpui::test]
fn the_split_preview_follows_the_editor(cx: &mut TestAppContext) {
    let text = DOC.replace("reading_view_fence", "split_preview_fence");
    let dir = temp(&[("a.md", &text)]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    workspace.update(&mut vcx, |ws, cx| ws.show_mode(ViewMode::Split, cx));
    for _ in 0..5 {
        draw(&mut vcx);
    }
    let split = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().split().cloned())
        .expect("a Split view");
    let preview = split.read_with(&vcx, |s, _| s.reader().clone());
    let editor = split.read_with(&vcx, |s, _| s.editor().clone());
    assert_eq!(
        colour(&preview, &mut vcx, "fn split"),
        Token::SyntaxTag.role()
    );
    // A comment typed in the editor's fence reaches the preview, coloured.
    vcx.update(|window, cx| {
        let focus = editor.read(cx).focus_handle(cx);
        focus.focus(window, cx);
    });
    vcx.simulate_keystrokes("ctrl-home down down down end");
    vcx.simulate_input(" // typed");
    for _ in 0..5 {
        draw(&mut vcx);
    }
    assert_eq!(
        colour(&preview, &mut vcx, "// typed"),
        Token::TextTertiary.role()
    );
}
