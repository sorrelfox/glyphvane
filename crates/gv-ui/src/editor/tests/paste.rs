// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Smart paste, the status bar's facts and viewer mode, in headless
//! windows: HTML on the clipboard becomes Markdown, a URL over a selection
//! becomes a link, Paste as plain text pastes the text, several carets
//! paste as before; the caret's line and column follow it, the encoding
//! and line endings of a UTF-16 CRLF file; a read-only view takes no
//! edits, and viewer mode keeps tabs out of Edit and Split except for
//! `--wait`.

use std::rc::Rc;

use gv_settings::{CURRENT_VERSION, LayerKind, SettingsFile};

use super::*;
use crate::editor::{ClipboardHtml, EditorFacts, HtmlClipboard, set_html_clipboard};

/// A clipboard whose HTML flavor is fixed.
struct FakeHtml(ClipboardHtml);

impl HtmlClipboard for FakeHtml {
    fn read_html(&self) -> Option<ClipboardHtml> {
        Some(self.0.clone())
    }
}

/// Puts `text` on the clipboard, with `html` as its HTML flavor.
fn put(vcx: &mut VisualTestContext, text: &str, html: Option<&str>) {
    vcx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
    let reader = html.map(|html| {
        Rc::new(FakeHtml(ClipboardHtml {
            html: html.to_owned(),
            source_url: None,
        })) as Rc<dyn HtmlClipboard>
    });
    vcx.update(|_, cx| set_html_clipboard(cx, reader));
}

const PAGE: &str = "<meta charset=\"utf-8\"><p><b>Bold</b> and \
                    <a href=\"https://example.com/docs\">a link</a></p>\
                    <ul><li>one</li><li>two</li></ul><script>alert(1)</script>";

#[gpui::test]
fn html_on_the_clipboard_becomes_markdown(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "Notes\n");
    vcx.simulate_keystrokes("down");
    put(&mut vcx, "Bold and a link\none\ntwo", Some(PAGE));
    vcx.dispatch_action(commands::Paste);
    assert_eq!(
        text_of(&view, &mut vcx),
        "Notes\n**Bold** and [a link](https://example.com/docs)\n\n- one\n- two"
    );
    // One undo step.
    vcx.dispatch_action(commands::Undo);
    assert_eq!(text_of(&view, &mut vcx), "Notes\n");
}

#[gpui::test]
fn a_url_over_a_selection_makes_a_link(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "see the docs here\n");
    vcx.simulate_keystrokes("ctrl-right ctrl-right right");
    vcx.simulate_keystrokes("ctrl-shift-right");
    assert_eq!(selections(&view, &mut vcx), [(8, 12)]);
    // A browser's Copy link puts the link's HTML beside the URL.
    put(
        &mut vcx,
        "https://example.com/a",
        Some("<a href=\"https://example.com/a\">https://example.com/a</a>"),
    );
    vcx.dispatch_action(commands::Paste);
    assert_eq!(
        text_of(&view, &mut vcx),
        "see the [docs](https://example.com/a) here\n"
    );
    vcx.dispatch_action(commands::Undo);
    assert_eq!(text_of(&view, &mut vcx), "see the docs here\n");
    // Without a selection, the URL as it is.
    vcx.simulate_keystrokes("end");
    vcx.simulate_input(" ");
    vcx.dispatch_action(commands::Paste);
    assert_eq!(
        text_of(&view, &mut vcx),
        "see the docs here https://example.com/a\n"
    );
}

#[gpui::test]
fn paste_as_plain_text_pastes_the_text(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "word\n");
    vcx.simulate_keystrokes("shift-end");
    put(&mut vcx, "https://example.com/", Some(PAGE));
    vcx.dispatch_action(commands::PastePlain);
    assert_eq!(text_of(&view, &mut vcx), "https://example.com/\n");
    vcx.simulate_keystrokes("end");
    put(&mut vcx, " *raw* text", Some(PAGE));
    vcx.simulate_keystrokes("ctrl-shift-v");
    assert_eq!(
        text_of(&view, &mut vcx),
        "https://example.com/ *raw* text\n"
    );
}

#[gpui::test]
fn several_carets_paste_the_text_as_before(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "ab\ncd\n");
    vcx.dispatch_action(AddCursorBelow);
    vcx.simulate_keystrokes("shift-right");
    put(&mut vcx, "1\n2", Some(PAGE));
    vcx.dispatch_action(commands::Paste);
    assert_eq!(text_of(&view, &mut vcx), "1b\n2d\n");
    put(&mut vcx, "https://example.com/", Some(PAGE));
    vcx.dispatch_action(commands::Paste);
    assert_eq!(
        text_of(&view, &mut vcx),
        "1https://example.com/b\n2https://example.com/d\n"
    );
}

// --- Status bar facts --------------------------------------------------------------

fn facts(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> EditorFacts {
    view.read_with(vcx, |v, _| v.status_facts())
        .expect("loaded")
}

#[gpui::test]
fn the_caret_place_follows_the_caret(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "# Title\n\nSome wörds here.\n");
    let f = facts(&view, &mut vcx);
    assert_eq!((f.line, f.column, f.selected), (1, 1, 0));
    assert_eq!((f.encoding, f.line_endings), ("UTF-8", "LF"));
    assert_eq!(f.words, Some(4));
    vcx.simulate_keystrokes("down down end");
    let f = facts(&view, &mut vcx);
    // Columns count characters, not bytes.
    assert_eq!((f.line, f.column), (3, 17));
    vcx.simulate_keystrokes("shift-ctrl-left shift-ctrl-left");
    let f = facts(&view, &mut vcx);
    // "here." is selected.
    assert_eq!((f.line, f.column, f.selected), (3, 12, 5));
    // Edits are counted again.
    vcx.simulate_keystrokes("end");
    vcx.simulate_input(" More 日本");
    draw(&mut vcx);
    assert_eq!(facts(&view, &mut vcx).words, Some(7));
}

#[gpui::test]
fn a_large_document_is_counted_when_edits_pause(cx: &mut TestAppContext) {
    let text = "word ".repeat(60_000);
    let (view, mut vcx) = editor(cx, &text);
    vcx.executor()
        .advance_clock(super::super::stats::COUNT_DELAY);
    draw(&mut vcx);
    assert_eq!(facts(&view, &mut vcx).words, Some(60_000));
    vcx.simulate_input("more ");
    draw(&mut vcx);
    // The last count shows until the new one lands.
    assert_eq!(facts(&view, &mut vcx).words, Some(60_000));
    vcx.executor()
        .advance_clock(super::super::stats::COUNT_DELAY);
    draw(&mut vcx);
    assert_eq!(facts(&view, &mut vcx).words, Some(60_001));
}

#[gpui::test]
fn a_utf16_crlf_file_shows_its_encoding_and_line_endings(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("wide.md");
    let bytes: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(
            "# Title\r\n\r\nSome words.\r\n"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        )
        .collect();
    std::fs::write(&path, bytes).expect("write");
    let (window, workspace) = open_workspace(cx, &path);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    let shown = |vcx: &mut VisualTestContext| {
        workspace
            .read_with(vcx, |ws, cx| ws.editor_facts(cx))
            .expect("the editor is loaded")
    };
    let f = shown(&mut vcx);
    assert_eq!((f.encoding, f.line_endings), ("UTF-16 LE", "CRLF"));
    assert_eq!((f.line, f.column, f.words), (1, 1, Some(3)));
    vcx.simulate_keystrokes("down down end");
    draw(&mut vcx);
    let f = shown(&mut vcx);
    assert_eq!((f.line, f.column), (3, 12));
}

// --- Viewer mode -----------------------------------------------------------------------

#[gpui::test]
fn a_read_only_view_takes_no_edits(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "hello\n");
    view.update(&mut vcx, |v, cx| v.set_read_only(true, cx));
    draw(&mut vcx);
    vcx.simulate_input("typed ");
    vcx.simulate_keystrokes("delete enter backspace");
    put(&mut vcx, "pasted", Some(PAGE));
    vcx.dispatch_action(commands::Paste);
    vcx.dispatch_action(commands::PastePlain);
    vcx.simulate_keystrokes("shift-end");
    vcx.dispatch_action(commands::Bold);
    vcx.dispatch_action(commands::Cut);
    assert_eq!(text_of(&view, &mut vcx), "hello\n");
    // The carets still move and copy still works; the edit commands are
    // disabled.
    assert_eq!(selections(&view, &mut vcx), [(0, 5)]);
    vcx.dispatch_action(commands::Copy);
    assert_eq!(
        vcx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("hello")
    );
    let available = vcx.update(|window, cx| {
        [
            window.is_action_available(&commands::Paste, cx),
            window.is_action_available(&commands::Bold, cx),
            window.is_action_available(&commands::Undo, cx),
            window.is_action_available(&commands::Copy, cx),
        ]
    });
    assert_eq!(available, [false, false, false, true]);

    view.update(&mut vcx, |v, cx| v.set_read_only(false, cx));
    draw(&mut vcx);
    vcx.dispatch_action(commands::Bold);
    assert_eq!(text_of(&view, &mut vcx), "**hello**\n");
}

/// A settings file in `dir` with viewer mode `on`.
fn viewer_settings(dir: &Path, on: bool) -> SettingsFile {
    let path = dir.join("settings.toml");
    let text = format!("version = {CURRENT_VERSION}\n[general]\nviewer_mode = {on}\n");
    std::fs::write(&path, text).expect("write settings");
    SettingsFile::load(&path, LayerKind::Global).expect("settings load")
}

fn viewer_workspace(
    cx: &mut TestAppContext,
    dir: &Path,
    file: &Path,
) -> (AnyWindowHandle, Entity<Workspace>) {
    let settings = viewer_settings(dir, true);
    cx.update(|cx| {
        crate::init(
            cx,
            AppSettings::new(Some(settings), Some(ThemeChoice::Light)),
        );
        let target = Some(OpenTarget::new(file));
        gpui::open_window(options(), cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    })
}

fn mode(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> ViewMode {
    workspace.read_with(vcx, |ws, _| ws.active_tab().mode())
}

#[gpui::test]
fn viewer_mode_keeps_the_tab_in_preview(cx: &mut TestAppContext) {
    let dir = temp(&[("note.md", "# Title\n\nText.\n")]);
    let (window, workspace) = viewer_workspace(cx, dir.path(), &dir.path().join("note.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let available = workspace.read_with(&vcx, |ws, cx| {
        [ViewMode::Preview, ViewMode::Edit, ViewMode::Split].map(|m| ws.mode_available(m, cx))
    });
    assert_eq!(available, [true, false, false]);

    // Mod+E, Split, the View menu's commands and the mode switch do nothing.
    vcx.simulate_keystrokes("secondary-e");
    cx.dispatch_action(window, crate::keymap::actions::view::ToggleSplit);
    cx.dispatch_action(window, crate::keymap::actions::view::Edit);
    draw(&mut vcx);
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Preview);
    let edit = vcx
        .debug_bounds("mode-edit")
        .expect("the Edit segment")
        .center();
    vcx.simulate_click(edit, Modifiers::none());
    draw(&mut vcx);
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Preview);
    assert!(workspace.read_with(&vcx, |ws, _| ws.active_tab().editor().is_none()));

    // Viewer mode off: Edit works, and the editor takes edits.
    let off = viewer_settings(dir.path(), false);
    vcx.update(|_, cx| AppSettings::reload_global(cx, off));
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Edit);
    let editor = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("editor");
    vcx.simulate_input("New ");
    assert_eq!(text_of(&editor, &mut vcx), "New # Title\n\nText.\n");

    // On again: the tab goes back to Preview and its editor is read-only.
    let on = viewer_settings(dir.path(), true);
    vcx.update(|_, cx| AppSettings::reload_global(cx, on));
    draw(&mut vcx);
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Preview);
    assert!(editor.read_with(&vcx, |e, _| e.is_read_only()));
}

#[gpui::test]
fn wait_overrides_viewer_mode_for_its_tab(cx: &mut TestAppContext) {
    let dir = temp(&[("a.md", "alpha\n"), ("b.md", "beta\n")]);
    let (window, workspace) = viewer_workspace(cx, dir.path(), &dir.path().join("a.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    // `--wait` for b.md in the same window.
    workspace.update_in(&mut vcx, |ws, window, cx| {
        ws.open_tab_for_edit(OpenTarget::new(dir.path().join("b.md")), None, window, cx);
    });
    draw(&mut vcx);
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Edit);
    let editor = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("editor");
    assert!(!editor.read_with(&vcx, |e, _| e.is_read_only()));
    vcx.simulate_input("edited ");
    assert_eq!(text_of(&editor, &mut vcx), "edited beta\n");

    // The other tab stays protected.
    workspace.update_in(&mut vcx, |ws, window, cx| ws.activate_tab(0, window, cx));
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Preview);
}

/// *Viewer mode* switches viewer mode for the active tab only, both ways.
#[gpui::test]
fn the_viewer_mode_command_switches_the_active_tab(cx: &mut TestAppContext) {
    use crate::keymap::actions::view::ToggleViewerMode;
    let dir = temp(&[("a.md", "alpha\n"), ("b.md", "beta\n")]);
    let (window, workspace) = viewer_workspace(cx, dir.path(), &dir.path().join("a.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let viewer = |vcx: &mut VisualTestContext| {
        workspace.read_with(vcx, |ws, cx| !ws.mode_available(ViewMode::Edit, cx))
    };
    assert!(viewer(&mut vcx));

    // Off for a.md: Edit works and the editor takes edits.
    cx.dispatch_action(window, ToggleViewerMode);
    draw(&mut vcx);
    assert!(!viewer(&mut vcx));
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Edit);
    let editor = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("editor");
    vcx.simulate_input("edited ");
    assert_eq!(text_of(&editor, &mut vcx), "edited alpha\n");

    // Another tab still follows the setting.
    workspace.update_in(&mut vcx, |ws, window, cx| {
        ws.open_tab(OpenTarget::new(dir.path().join("b.md")), window, cx);
    });
    draw(&mut vcx);
    assert!(viewer(&mut vcx));
    workspace.update_in(&mut vcx, |ws, window, cx| ws.activate_tab(0, window, cx));
    draw(&mut vcx);

    // On again: back to Preview, and the editor is read-only.
    cx.dispatch_action(window, ToggleViewerMode);
    draw(&mut vcx);
    assert!(viewer(&mut vcx));
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Preview);
    assert!(editor.read_with(&vcx, |e, _| e.is_read_only()));

    // With the setting off, the command turns viewer mode on for the tab.
    let off = viewer_settings(dir.path(), false);
    vcx.update(|_, cx| AppSettings::reload_global(cx, off));
    draw(&mut vcx);
    workspace.update_in(&mut vcx, |ws, window, cx| ws.activate_tab(1, window, cx));
    draw(&mut vcx);
    assert!(!viewer(&mut vcx));
    cx.dispatch_action(window, ToggleViewerMode);
    draw(&mut vcx);
    assert!(viewer(&mut vcx));
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    assert_eq!(mode(&workspace, &mut vcx), ViewMode::Preview);
}
