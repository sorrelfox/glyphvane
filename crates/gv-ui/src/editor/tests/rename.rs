// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Rename heading or block ID in headless windows: the prompt, the
//! preview, the buffer edit as one undo step, the other notes written,
//! and the infobar's Undo.

use std::time::Duration;

use gv_profile::ProfileId;
use gv_vault::refactor::AnchorTarget;

use super::autocomplete::{editor_in, vault};
use super::*;
use crate::file_prompt::{FilePrompt, prompt_of};
use crate::files::notice::NoticeAction;
use crate::keymap::actions::refactor::RenameHeading;

fn prompt(vcx: &mut VisualTestContext) -> Entity<FilePrompt> {
    vcx.update(|window, cx| prompt_of(window, cx))
        .expect("prompt")
}

fn caret_at(view: &Entity<EditorView>, vcx: &mut VisualTestContext, offset: usize) {
    view.update(vcx, |v, cx| v.set_cursor(offset, cx));
    draw(vcx);
}

/// Lets the planning and the writes on the background executor finish.
fn settle(vcx: &mut VisualTestContext) {
    for _ in 0..20 {
        draw(vcx);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[gpui::test]
fn renaming_a_heading_updates_other_notes_and_undoes_in_one_step(cx: &mut TestAppContext) {
    let note = "# Top\n\n## Old name\n\ntext [[#Old name]]\n";
    let other = "see [[Note#Old name|there]] and ![[Note#Old name]]\n";
    let v = vault(&[("Note.md", note), ("a.md", other)]);
    let (view, mut vcx) = editor_in(cx, &v, "Note.md", ProfileId::Obsidian);
    caret_at(&view, &mut vcx, note.find("Old").expect("heading"));
    let anchor = view.update(&mut vcx, |v, _| v.anchor_at_caret());
    assert_eq!(
        anchor.map(|a| (a.target, a.name)),
        Some((AnchorTarget::Heading(1), "Old name".to_owned()))
    );

    vcx.dispatch_action(RenameHeading);
    draw(&mut vcx);
    let prompt = prompt(&mut vcx);
    assert_eq!(
        prompt.read_with(&vcx, |p, _| p.title().map(str::to_owned)),
        Some("Rename heading".to_owned())
    );
    assert_eq!(vcx.update(|_, cx| prompt.read(cx).selection(cx)), 0..8);
    vcx.update(|window, cx| prompt.update(cx, |p, cx| p.set_text("New | name", window, cx)));
    draw(&mut vcx);
    vcx.simulate_keystrokes("enter");
    settle(&mut vcx);

    // Another note changes: the preview lists both files.
    assert!(prompt.read_with(&vcx, |p, _| p.is_open()));
    let rows: Vec<String> = prompt.read_with(&vcx, |p, _| {
        p.rows().into_iter().map(|(text, _)| text).collect()
    });
    assert!(rows.iter().any(|r| r == "Note.md (2)"), "{rows:?}");
    assert!(rows.iter().any(|r| r == "a.md (2)"), "{rows:?}");
    vcx.simulate_keystrokes("enter");
    settle(&mut vcx);

    let renamed = "# Top\n\n## New | name\n\ntext [[#New name]]\n";
    assert_eq!(text_of(&view, &mut vcx), renamed);
    assert_eq!(
        std::fs::read_to_string(v.root.join("a.md")).expect("read"),
        "see [[Note#New name|there]] and ![[Note#New name]]\n"
    );
    assert!(view.read_with(&vcx, |v, _| {
        v.notice()
            .is_some_and(|n| n.offers(&NoticeAction::UndoFileOp))
    }));

    // The infobar's Undo reverts the note and the other file.
    view.update(&mut vcx, |v, cx| {
        v.notice_action(&NoticeAction::UndoFileOp, cx)
    });
    settle(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx), note);
    assert_eq!(
        std::fs::read_to_string(v.root.join("a.md")).expect("read"),
        other
    );
}

#[gpui::test]
fn a_block_id_rename_in_the_note_alone_is_one_undo_step(cx: &mut TestAppContext) {
    let note = "Para ^abc\n\nsee [[#^abc]]\n";
    let v = vault(&[("Note.md", note)]);
    let (view, mut vcx) = editor_in(cx, &v, "Note.md", ProfileId::Obsidian);
    caret_at(&view, &mut vcx, 2);
    vcx.dispatch_action(RenameHeading);
    draw(&mut vcx);
    let prompt = prompt(&mut vcx);
    assert_eq!(
        prompt.read_with(&vcx, |p, _| p.title().map(str::to_owned)),
        Some("Rename block ID".to_owned())
    );
    // Not a valid id: Enter does nothing.
    vcx.update(|window, cx| prompt.update(cx, |p, cx| p.set_text("a b", window, cx)));
    draw(&mut vcx);
    assert!(prompt.read_with(&vcx, |p, _| p.problem().is_some()));
    vcx.update(|window, cx| prompt.update(cx, |p, cx| p.set_text("key-1", window, cx)));
    draw(&mut vcx);
    vcx.simulate_keystrokes("enter");
    settle(&mut vcx);
    // Nothing else changes: no preview, applied at once.
    assert!(!prompt.read_with(&vcx, |p, _| p.is_open()));
    assert_eq!(text_of(&view, &mut vcx), "Para ^key-1\n\nsee [[#^key-1]]\n");
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(text_of(&view, &mut vcx), note);
}

#[gpui::test]
fn the_caret_picks_the_heading_block_or_section(cx: &mut TestAppContext) {
    let note = "intro\n\n# A\n\nbody text\n\n- item ^blk\n";
    let v = vault(&[("Note.md", note)]);
    let (view, mut vcx) = editor_in(cx, &v, "Note.md", ProfileId::Obsidian);
    let at = |view: &Entity<EditorView>, vcx: &mut VisualTestContext, offset: usize| {
        caret_at(view, vcx, offset);
        view.update(vcx, |v, _| v.anchor_at_caret())
            .map(|a| a.target)
    };
    assert_eq!(at(&view, &mut vcx, 1), None);
    assert_eq!(
        at(&view, &mut vcx, note.find("body").expect("body")),
        Some(AnchorTarget::Heading(0))
    );
    assert_eq!(
        at(&view, &mut vcx, note.find("item").expect("item")),
        Some(AnchorTarget::Block("blk".into()))
    );
    // Nothing at the caret: the infobar says so.
    caret_at(&view, &mut vcx, 1);
    vcx.dispatch_action(RenameHeading);
    draw(&mut vcx);
    assert!(view.read_with(&vcx, |v, _| v.notice().is_some()));
}
