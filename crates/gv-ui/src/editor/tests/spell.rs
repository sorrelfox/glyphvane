// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Spell check in the Source view: squiggles on misspelled prose only,
//! re-checks after edits, Next misspelling, Add to dictionary (global and
//! vault, persisted), Ignore, the context menu's suggestions, the language
//! from the front matter, and a 1 MB document.

use std::path::Path;
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, Modifiers, MouseButton, Point, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_settings::Locations;
use gv_syntax::ParseOptions;

use super::{HEIGHT, WIDTH, draw, editor, options, point_of, selections, text_of};
use crate::editor::EditorView;
use crate::keymap::actions::{navigation, spell as commands};
use crate::shell::{OpenTarget, Workspace};
use crate::{AppSettings, ThemeChoice, ToggleEditMode, WindowChrome};

/// Longer than a large document's wait for edits to pause.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// The misspelled words found, in order.
fn found(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Vec<String> {
    view.read_with(vcx, |v, _| {
        v.spell.found().iter().map(|m| m.word.to_string()).collect()
    })
}

/// The squiggles of the last frame.
fn squiggles(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> usize {
    view.read_with(vcx, |v, _| v.probe.squiggles.len())
}

fn set_caret(view: &Entity<EditorView>, vcx: &mut VisualTestContext, offset: usize) {
    view.update(vcx, |v, cx| v.set_cursor(offset, cx));
    draw(vcx);
}

#[gpui::test]
fn misspelled_prose_gets_squiggles(cx: &mut TestAppContext) {
    let text = "Some teh text with `teh code`, a [lnk](https://wrnog.org) and <b>tehh</b>.\n\n```\nwrnog\n```\n\nAnother lne.\n";
    let (view, mut vcx) = editor(cx, text);
    // Text inside inline HTML is prose; the tags are not.
    assert_eq!(found(&view, &mut vcx), ["teh", "lnk", "tehh", "lne"]);
    assert_eq!(squiggles(&view, &mut vcx), 4);
    // Under the word: from its first character to its end.
    let (origin, width) = view.read_with(&vcx, |v, _| v.probe.squiggles[0]);
    let start = point_of(&view, &mut vcx, 5);
    let end = point_of(&view, &mut vcx, 8);
    assert!((f32::from(origin.x) - f32::from(start.x)).abs() < 1.0);
    assert!((f32::from(origin.x + width) - f32::from(end.x)).abs() < 1.0);
    assert!(origin.y > start.y);
}

#[gpui::test]
fn edits_are_checked_again(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "A fine line.\n\nSecond paragrph.\n");
    assert_eq!(found(&view, &mut vcx), ["paragrph"]);
    vcx.simulate_keystrokes("end");
    vcx.simulate_input(" wiht");
    draw(&mut vcx);
    assert_eq!(found(&view, &mut vcx), ["wiht", "paragrph"]);
    // The word being typed (an empty caret at its end) is not marked yet.
    assert_eq!(squiggles(&view, &mut vcx), 1);
    vcx.simulate_input(" x");
    draw(&mut vcx);
    assert_eq!(squiggles(&view, &mut vcx), 2);
    // Fixing a word clears its mark.
    let at = text_of(&view, &mut vcx).find("paragrph").expect("there") + 6;
    set_caret(&view, &mut vcx, at);
    vcx.simulate_input("a");
    draw(&mut vcx);
    assert_eq!(found(&view, &mut vcx), ["wiht"]);
}

#[gpui::test]
fn next_misspelling_selects_each_in_turn(cx: &mut TestAppContext) {
    let text = "Frist word.\n\nThe secnod and thrid.\n";
    let (view, mut vcx) = editor(cx, text);
    let ranges: Vec<(usize, usize)> = ["Frist", "secnod", "thrid"]
        .iter()
        .map(|w| {
            let at = text.find(w).expect("there");
            (at, at + w.len())
        })
        .collect();
    set_caret(&view, &mut vcx, 3);
    for expected in [ranges[1], ranges[2], ranges[0]] {
        vcx.dispatch_action(navigation::NextMisspelling);
        draw(&mut vcx);
        assert_eq!(selections(&view, &mut vcx), [expected]);
    }
}

/// A window for a Source view of `text`, with the app's files under
/// `root` and, when `vault` is given, that folder open as the vault.
fn editor_with_locations(
    cx: &mut TestAppContext,
    text: &str,
    root: &Path,
    vault: Option<&Path>,
) -> (Entity<EditorView>, VisualTestContext) {
    let text = text.to_owned();
    let locations = Locations::with_root(root);
    let vault = vault.map(Path::to_path_buf);
    let (window, view) = cx.update(|cx| {
        let settings = AppSettings::new(None, Some(ThemeChoice::Light)).with_locations(locations);
        crate::init(cx, settings);
        if let Some(vault) = &vault {
            AppSettings::open_vault(cx, vault);
        }
        gpui::open_window(options(), cx, |_, cx| {
            cx.new(|cx| {
                let mut v = EditorView::new(ParseOptions::default(), cx);
                v.set_text(&text, cx);
                v
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        focus.focus(window, cx);
    });
    draw(&mut vcx);
    (view, vcx)
}

#[gpui::test]
fn added_words_persist_and_ignored_ones_last_the_session(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let vault = dir.path().join("notes");
    std::fs::create_dir_all(&vault).expect("vault folder");
    let text = "Glyphvane and zettel and Glyphvane and frobnicate.\n";
    let (view, mut vcx) = editor_with_locations(cx, text, &dir.path().join("app"), Some(&vault));
    assert_eq!(
        found(&view, &mut vcx),
        ["Glyphvane", "zettel", "Glyphvane", "frobnicate"]
    );

    // Add to dictionary: the caret in the word.
    set_caret(&view, &mut vcx, 2);
    vcx.dispatch_action(commands::AddToDictionary);
    assert_eq!(found(&view, &mut vcx), ["zettel", "frobnicate"]);
    draw(&mut vcx);
    assert_eq!(found(&view, &mut vcx), ["zettel", "frobnicate"]);
    let words = dir.path().join("app/config/dictionaries/words.txt");
    assert_eq!(
        std::fs::read_to_string(&words).expect("saved"),
        "Glyphvane\n"
    );

    // Add to vault dictionary.
    set_caret(&view, &mut vcx, text.find("zettel").expect("there") + 1);
    vcx.dispatch_action(commands::AddToVaultDictionary);
    draw(&mut vcx);
    assert_eq!(found(&view, &mut vcx), ["frobnicate"]);
    let id = crate::settings::vault_id(&vault);
    let vault_words = dir.path().join(format!("app/data/vaults/{id}/words.txt"));
    assert_eq!(
        std::fs::read_to_string(&vault_words).expect("saved"),
        "zettel\n"
    );

    // Ignore: gone, and nothing written.
    set_caret(&view, &mut vcx, text.find("frobnicate").expect("there"));
    vcx.dispatch_action(commands::Ignore);
    draw(&mut vcx);
    assert!(found(&view, &mut vcx).is_empty());
    assert_eq!(
        std::fs::read_to_string(&words).expect("saved"),
        "Glyphvane\n"
    );
    assert_eq!(squiggles(&view, &mut vcx), 0);
}

#[gpui::test]
fn saved_words_are_read_at_start(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let words = dir.path().join("config/dictionaries/words.txt");
    std::fs::create_dir_all(words.parent().expect("parent")).expect("folder");
    std::fs::write(&words, "frobnicate\n").expect("write");
    let (view, mut vcx) = editor_with_locations(cx, "We frobnicate the teh.\n", dir.path(), None);
    assert_eq!(found(&view, &mut vcx), ["teh"]);
}

#[gpui::test]
fn front_matter_languages_and_settings(cx: &mut TestAppContext) {
    // A language without a dictionary: nothing is checked.
    let text = "---\nlang: xx\n---\nWrnog wrods.\n";
    let options = ParseOptions {
        front_matter: gv_syntax::FrontMatterOptions {
            yaml: true,
            toml: false,
        },
        ..ParseOptions::default()
    };
    let (view, mut vcx) = super::editor_with(cx, text, options);
    assert!(found(&view, &mut vcx).is_empty());
    // `en` takes the bundled en_US.
    view.update(&mut vcx, |v, cx| {
        v.set_text("---\nlang: en\n---\nWrnog wrods.\n", cx);
    });
    draw(&mut vcx);
    assert_eq!(found(&view, &mut vcx), ["Wrnog", "wrods"]);
    // Turning spell check off clears the marks.
    vcx.update(|_, cx| {
        AppSettings::set_value(
            cx,
            crate::settings::Target::AllVaults,
            "spelling.enabled",
            &toml::Value::Boolean(false),
        )
        .expect("set");
    });
    draw(&mut vcx);
    assert!(found(&view, &mut vcx).is_empty());
    assert_eq!(squiggles(&view, &mut vcx), 0);
}

/// A 1 MB document: the edit that schedules a re-check returns before the
/// check runs (it runs on the background executor), and the results land
/// once the executor has run.
#[gpui::test]
fn a_one_megabyte_document_is_checked_off_the_ui_thread(cx: &mut TestAppContext) {
    let mut text = String::new();
    let mut i = 0;
    while text.len() < 1 << 20 {
        text.push_str(&format!(
            "Paragraph {i} is ordinary prose with one mistaek in it, and a [link](https://example.org).\n\n"
        ));
        i += 1;
    }
    let (view, mut vcx) = editor(cx, &text);
    assert_eq!(found(&view, &mut vcx).len(), i);
    set_caret(&view, &mut vcx, 0);
    let started = Instant::now();
    view.update_in(&mut vcx, |v, window, cx| {
        v.with_editor(window, cx, |e, _| {
            let _ = e.edit([(0..0, "Teh ".to_owned())], gv_editor::EditKind::Command);
        });
    });
    let on_ui_thread = started.elapsed();
    // Not checked yet: the check waits on the background executor.
    let checked = view.read_with(&vcx, |v, _| {
        v.spell.checked_revision() == v.editor().map(|e| e.text().revision())
    });
    assert!(!checked);
    vcx.executor().advance_clock(DEBOUNCE);
    draw(&mut vcx);
    let words = found(&view, &mut vcx);
    assert_eq!(words.len(), i + 1);
    assert_eq!(words[0], "Teh");
    println!("1 MB: edit + reparse + scheduling on the UI thread: {on_ui_thread:?}");
}

// --- The context menu ---------------------------------------------------------

struct Shell {
    _dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn shell(cx: &mut TestAppContext, text: &str) -> Shell {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("a.md");
    std::fs::write(&file, text).expect("write");
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(HEIGHT)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            let target = Some(OpenTarget::new(&file));
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    for _ in 0..3 {
        draw(&mut vcx);
    }
    vcx.dispatch_action(ToggleEditMode);
    for _ in 0..3 {
        draw(&mut vcx);
    }
    Shell {
        _dir: dir,
        workspace,
        vcx,
    }
}

impl Shell {
    fn editor(&mut self) -> Entity<EditorView> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().editor().cloned())
            .expect("an editor")
    }

    fn text(&mut self) -> String {
        let editor = self.editor();
        text_of(&editor, &mut self.vcx)
    }

    fn has(&mut self, selector: &str) -> bool {
        let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
        self.vcx.debug_bounds(selector).is_some()
    }

    fn suggestions(&mut self) -> Vec<String> {
        let popover = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.context_menu().clone());
        popover.read_with(&self.vcx, |p, _| {
            p.suggestions().iter().map(ToString::to_string).collect()
        })
    }

    fn click(&mut self, selector: &'static str) {
        let at = self.vcx.debug_bounds(selector).expect("drawn").center();
        self.vcx.simulate_click(at, Modifiers::none());
        draw(&mut self.vcx);
    }

    fn right_click(&mut self, at: Point<gpui::Pixels>) {
        self.vcx
            .simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
        self.vcx
            .simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
        draw(&mut self.vcx);
    }
}

#[gpui::test]
fn the_context_menu_offers_suggestions_and_applies_one(cx: &mut TestAppContext) {
    let mut s = shell(cx, "A recieve here and good words.\n");
    let editor = s.editor();
    let at = point_of(&editor, &mut s.vcx, 4);
    s.right_click(at);
    let suggestions = s.suggestions();
    assert_eq!(suggestions.first().map(String::as_str), Some("receive"));
    assert!(s.has("context-menu-suggestion:receive"));
    assert!(!s.has("context-menu-disabled:spell.add_to_dictionary"));
    assert!(!s.has("context-menu-disabled:spell.ignore"));
    s.click("context-menu-suggestion:receive");
    assert_eq!(s.text(), "A receive here and good words.\n");

    // Off a misspelled word: no suggestions, spelling items disabled.
    let at = point_of(&editor, &mut s.vcx, 22);
    s.right_click(at);
    assert!(s.suggestions().is_empty());
    assert!(s.has("context-menu-disabled:spell.ignore"));
    s.vcx.simulate_keystrokes("escape");
    draw(&mut s.vcx);

    // The menu key with the caret in a misspelled word: the first
    // suggestion is highlighted, Enter applies it.
    let editor = s.editor();
    editor.update(&mut s.vcx, |e, cx| {
        e.set_text("Recieve one.\n", cx);
        e.set_cursor(1, cx);
    });
    draw(&mut s.vcx);
    s.vcx.simulate_keystrokes("shift-f10");
    draw(&mut s.vcx);
    assert_eq!(s.suggestions().first().map(String::as_str), Some("Receive"));
    s.vcx.simulate_keystrokes("enter");
    draw(&mut s.vcx);
    assert_eq!(s.text(), "Receive one.\n");
}

#[gpui::test]
fn ignore_from_the_context_menu(cx: &mut TestAppContext) {
    let mut s = shell(cx, "Some qwertyx words and qwertyx again.\n");
    let editor = s.editor();
    assert_eq!(found(&editor, &mut s.vcx), ["qwertyx", "qwertyx"]);
    let at = point_of(&editor, &mut s.vcx, 6);
    s.right_click(at);
    s.click("context-menu-item:spell.ignore");
    assert!(found(&editor, &mut s.vcx).is_empty());
    assert_eq!(s.text(), "Some qwertyx words and qwertyx again.\n");
}
