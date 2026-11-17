// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The cheatsheet in a headless window: opened by its command, grouped by
//! category, filtered, copied as Markdown, following keymap changes.

use gpui::{Modifiers, TestAppContext, VisualTestContext};
use gv_settings::keymap::{Category, KeySequence, Platform, UserKeymap};

use super::{cheatsheet_window, groups, markdown};
use crate::keymap::action_for;
use crate::settings::AppSettings;
use crate::theme::ThemeChoice;

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn label(keys: &str) -> String {
    KeySequence::parse(keys, Platform::current())
        .map(|k| k.label(Platform::current()))
        .unwrap_or_default()
}

#[gpui::test]
fn the_command_opens_one_window_grouped_by_category(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let action = action_for("app.keymap_cheatsheet").expect("an action");
        assert!(cx.is_action_available(action.as_ref()));
        cx.dispatch_action(action.as_ref());
    });
    cx.run_until_parked();
    let (window, view) = cx
        .update(|cx| cheatsheet_window(cx))
        .expect("the command opens the cheatsheet");
    // Again: the same window comes forward.
    cx.update(|cx| super::open(cx).expect("opens"));
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);

    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let shown = view.read_with(&vcx, |v, cx| v.groups(cx));
    let categories: Vec<Category> = shown.iter().map(|(c, _)| *c).collect();
    let mut sorted = categories.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(categories, sorted, "each group once, in category order");
    assert!(shown.iter().all(|(_, entries)| !entries.is_empty()));
    assert!(vcx.debug_bounds("cheatsheet-row:file.save").is_some());

    // Filter by title or key.
    vcx.update(|window, cx| view.update(cx, |v, cx| v.set_query("save as", window, cx)));
    draw(&mut vcx);
    let shown = view.read_with(&vcx, |v, cx| v.groups(cx));
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].1[0].command.as_str(), "file.save_as");
    assert!(vcx.debug_bounds("cheatsheet-row:file.save").is_none());

    // Copy as Markdown.
    let at = vcx.debug_bounds("cheatsheet-copy").expect("drawn").center();
    vcx.simulate_click(at, Modifiers::none());
    draw(&mut vcx);
    let copied = vcx
        .update(|_, cx| cx.read_from_clipboard())
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert!(
        copied.starts_with("# Keyboard shortcuts\n\n## File\n"),
        "{copied}"
    );
    assert!(
        copied.contains(&format!("| Save as | `{}` |", label("secondary-shift-s"))),
        "{copied}"
    );

    // Esc clears the filter, then closes the window.
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(view.read_with(&vcx, |v, _| v.query().is_empty()));
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(cx.update(|cx| cheatsheet_window(cx)).is_none());
}

#[gpui::test]
fn it_follows_keymap_changes(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        super::open(cx).expect("opens");
    });
    let (window, view) = cx.update(|cx| cheatsheet_window(cx)).expect("open");
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    assert!(vcx.debug_bounds("cheatsheet-row:app.about").is_none());
    vcx.update(|_, cx| {
        let user = UserKeymap::parse(
            "version = 1\n[[bind]]\nkeys = \"f7\"\ncommand = \"app.about\"\n",
            None,
            Platform::current(),
        );
        crate::keymap::apply(cx, &user);
    });
    draw(&mut vcx);
    assert!(vcx.debug_bounds("cheatsheet-row:app.about").is_some());
    let entries = view.read_with(&vcx, |v, cx| v.groups(cx));
    let about = entries
        .iter()
        .flat_map(|(_, e)| e)
        .find(|e| e.command.as_str() == "app.about")
        .expect("listed");
    assert_eq!(about.keys, vec!["F7".to_owned()]);
}

#[test]
fn markdown_escapes_pipes() {
    let entries = vec![gv_settings::keymap::CheatsheetEntry {
        category: Category::Editing,
        command: gv_settings::keymap::CommandId::new("editor.x").expect("id"),
        title: "A | B".to_owned(),
        keys: vec!["Ctrl+\\".to_owned(), "Ctrl+|".to_owned()],
    }];
    let text = markdown(&groups(entries, ""));
    assert_eq!(
        text,
        "# Keyboard shortcuts\n\n## Editing\n\n| Command | Keys |\n|---|---|\n| A \\| B | `Ctrl+\\`, `Ctrl+\\|` |\n"
    );
}
