// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Note embeds keep up with their notes, in headless windows: a changed
//! note is read off the UI thread and its embeds are built again; an
//! unchanged note is not read again; an unsaved buffer of the note shows
//! instead of the file.

use std::collections::HashSet;
use std::path::PathBuf;

use super::*;
use crate::reader::embed_texts::{EMBED_DEBOUNCE, reads_of};

/// A vault folder (with `.obsidian/`) holding `files`.
fn vault(files: &[(&str, &str)]) -> tempfile::TempDir {
    let mut all: Vec<(&str, &str)> = vec![(".obsidian/app.json", "{}")];
    all.extend_from_slice(files);
    temp(&all)
}

/// The text of every item of the shown document.
fn texts(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> Vec<String> {
    let st = state(reader, vcx);
    let st = st.borrow();
    let tree = &st.doc.as_ref().expect("loaded").tree;
    (0..tree.items().len())
        .map(|ix| tree.item_text(ix).text.to_string())
        .collect()
}

/// Lets a change notice through the debounce, and the jobs run.
fn after_debounce(vcx: &mut VisualTestContext) {
    vcx.executor().advance_clock(EMBED_DEBOUNCE * 2);
    draw(vcx);
    draw(vcx);
}

#[gpui::test]
fn a_changed_note_is_read_off_the_ui_thread_and_its_embed_is_built_again(cx: &mut TestAppContext) {
    let dir = vault(&[
        ("a.md", "# A\n\n![[b]]\n\nAfter.\n"),
        ("b.md", "Old body.\n"),
    ]);
    let b: PathBuf = dir.path().join("b.md");
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    assert_eq!(texts(&reader, &mut vcx), ["A", "b", "Old body.", "After."]);
    let read = reads_of(&b);
    assert_eq!(read, 1, "the note is read once");

    // Building the embed again, unchanged: from the cache.
    reader.update(&mut vcx, |r, cx| {
        r.embeds_changed(HashSet::from([b.clone()]), cx);
    });
    draw(&mut vcx);
    assert_eq!(reads_of(&b), read);

    // Changed on disk (another size): the UI thread starts a job and reads
    // nothing; the job reads the note and the embed shows it, in place.
    std::fs::write(&b, "New body, longer.\n").expect("write");
    let ids = reader.read_with(&vcx, |r, _| {
        let st = r.state.borrow();
        let doc = st.doc.as_ref().expect("loaded");
        doc.tree.items().iter().map(|it| it.id).collect::<Vec<_>>()
    });
    reader.update(&mut vcx, |r, cx| {
        r.embeds_changed(HashSet::from([b.clone()]), cx);
        assert_eq!(reads_of(&b), read, "no read on the UI thread");
    });
    assert_eq!(reads_of(&b), read);
    draw(&mut vcx);
    assert_eq!(reads_of(&b), read + 1);
    assert_eq!(
        texts(&reader, &mut vcx),
        ["A", "b", "New body, longer.", "After."]
    );
    let after = reader.read_with(&vcx, |r, _| {
        let st = r.state.borrow();
        let doc = st.doc.as_ref().expect("loaded");
        doc.tree.items().iter().map(|it| it.id).collect::<Vec<_>>()
    });
    assert_eq!(ids, after, "built in place");

    // The watcher reports a change with more blocks: after the debounce,
    // the document is built again.
    std::fs::write(&b, "One.\n\nTwo.\n").expect("write");
    crate::reader::embed_texts::changed(&b);
    after_debounce(&mut vcx);
    assert_eq!(
        texts(&reader, &mut vcx),
        ["A", "b", "One.", "Two.", "After."]
    );
}

#[gpui::test]
fn an_unsaved_buffer_of_the_note_shows_in_its_embed(cx: &mut TestAppContext) {
    let dir = vault(&[
        ("a.md", "# A\n\n![[b]]\n\nAfter.\n"),
        ("b.md", "Disk body.\n"),
    ]);
    let b: PathBuf = dir.path().join("b.md");
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    assert_eq!(texts(&reader, &mut vcx), ["A", "b", "Disk body.", "After."]);

    crate::files::overlay::dirty(&b, &gv_text::TextSnapshot::new("Typed body.\n"));
    after_debounce(&mut vcx);
    assert_eq!(
        texts(&reader, &mut vcx),
        ["A", "b", "Typed body.", "After."]
    );
    crate::files::overlay::dirty(&b, &gv_text::TextSnapshot::new("Typed more.\n"));
    after_debounce(&mut vcx);
    assert_eq!(
        texts(&reader, &mut vcx),
        ["A", "b", "Typed more.", "After."]
    );

    // Closed (or saved): the file again.
    let read = reads_of(&b);
    crate::files::overlay::clean(&b);
    after_debounce(&mut vcx);
    assert_eq!(texts(&reader, &mut vcx), ["A", "b", "Disk body.", "After."]);
    assert_eq!(reads_of(&b), read + 1, "dropped from the cache: read again");
}
