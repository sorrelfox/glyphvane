// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The keymap editor in a headless settings window: listing and search,
//! recording a shortcut (clicks and key presses), conflicts and their
//! resolution, removing and resetting, all written to a temporary
//! `keymap.toml` and applied at once.

use std::fs;

use gpui::{Bounds, Entity, Modifiers, Pixels, TestAppContext, VisualTestContext};
use gv_settings::Locations;
use gv_settings::keymap::{CommandId, KeySequence, Platform};

use super::keymap_editor::KeymapEditor;
use super::model::SectionId;
use super::{SettingsWindow, settings_window};
use crate::keymap::Keybindings;
use crate::settings::AppSettings;
use crate::theme::ThemeChoice;

struct Fixture {
    _dir: tempfile::TempDir,
    locations: Locations,
    _view: Entity<SettingsWindow>,
    editor: Entity<KeymapEditor>,
    vcx: VisualTestContext,
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn keys(text: &str) -> KeySequence {
    KeySequence::parse(text, Platform::current()).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn id(text: &'static str) -> CommandId {
    CommandId::new(text).unwrap_or_else(|e| panic!("{e:?}"))
}

/// The settings window on its Keymap section, with a config folder in a
/// temporary folder and no `keymap.toml` yet.
fn fixture(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().expect("temporary folder");
    let locations = Locations::with_root(dir.path().join("app"));
    let settings =
        AppSettings::new(None, Some(ThemeChoice::Light)).with_locations(locations.clone());
    let window = cx.update(|cx| {
        crate::init(cx, settings);
        super::open(cx).expect("the settings window opens")
    });
    let (_, view) = cx
        .update(|cx| settings_window(cx))
        .expect("the settings window is open");
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let editor = vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.select_section(SectionId::Keymap, cx);
            v.keymap_editor(window, cx)
        })
    });
    draw(&mut vcx);
    Fixture {
        _dir: dir,
        locations,
        _view: view,
        editor,
        vcx,
    }
}

impl Fixture {
    fn bounds(&mut self, selector: &str) -> Option<Bounds<Pixels>> {
        let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
        self.vcx.debug_bounds(selector)
    }

    fn click(&mut self, selector: &str) {
        let at = self
            .bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.vcx.simulate_click(at, Modifiers::none());
        draw(&mut self.vcx);
    }

    fn press(&mut self, keys: &str) {
        self.vcx.simulate_keystrokes(keys);
        draw(&mut self.vcx);
    }

    fn labels(&mut self, command: &str) -> Vec<String> {
        self.vcx
            .update(|_, cx| cx.global::<Keybindings>().keymap().key_labels(command))
    }

    fn file(&self) -> String {
        fs::read_to_string(self.locations.keymap_file()).unwrap_or_default()
    }

    fn update<R>(
        &mut self,
        f: impl FnOnce(&mut KeymapEditor, &mut gpui::Window, &mut gpui::Context<'_, KeymapEditor>) -> R,
    ) -> R {
        let editor = self.editor.clone();
        let out = self
            .vcx
            .update(|window, cx| editor.update(cx, |e, cx| f(e, window, cx)));
        draw(&mut self.vcx);
        out
    }
}

#[gpui::test]
fn lists_every_command_and_searches_by_name_and_key(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let all = f.editor.read_with(&f.vcx, |e, cx| e.rows(cx).len());
    let registered = f
        .vcx
        .update(|_, cx| cx.global::<Keybindings>().registry().len());
    assert_eq!(all, registered);
    assert!(f.bounds("keymap-add:app.about").is_some());

    f.update(|e, window, cx| e.set_query("save as", window, cx));
    let rows = f.editor.read_with(&f.vcx, |e, cx| e.rows(cx));
    assert_eq!(
        rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec!["file.save_as"]
    );
    assert_eq!(rows[0].keys, vec![keys("secondary-shift-s")]);

    // By key label, as menus show it.
    let label = keys("secondary-shift-s")
        .label(Platform::current())
        .to_lowercase();
    f.update(|e, window, cx| e.set_query(&label, window, cx));
    let rows = f.editor.read_with(&f.vcx, |e, cx| e.rows(cx));
    assert!(rows.iter().any(|r| r.id.as_str() == "file.save_as"));
    assert!(!rows.iter().any(|r| r.id.as_str() == "app.about"));
}

#[gpui::test]
fn records_a_shortcut_and_writes_it(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.click("keymap-add:app.about");
    assert_eq!(
        f.editor.read_with(&f.vcx, |e, _| e.recording().cloned()),
        Some(id("app.about"))
    );
    // The recorder catches the keys before any binding runs: Mod+, does
    // not open anything, Esc would close the window.
    f.press("secondary-alt-a");
    assert_eq!(
        f.editor.read_with(&f.vcx, |e, _| e.recorded()),
        Some(keys("secondary-alt-a"))
    );
    assert!(
        f.editor
            .read_with(&f.vcx, |e, cx| e.conflicts(cx))
            .is_empty()
    );
    f.click("keymap-save");
    assert!(f.editor.read_with(&f.vcx, |e, _| e.recording().is_none()));
    assert!(
        f.file()
            .contains("[[bind]]\nkeys = \"secondary-alt-a\"\ncommand = \"app.about\"\n"),
        "{}",
        f.file()
    );
    let label = keys("secondary-alt-a").label(Platform::current());
    assert_eq!(f.labels("app.about"), vec![label]);
    // The row shows the new key and a Reset button.
    assert!(f.bounds("keymap-reset:app.about").is_some());

    // A chord, written in this platform's section.
    f.click("keymap-add:app.licenses");
    f.click("keymap-scope-platform");
    f.press("secondary-k");
    f.press("secondary-l");
    assert_eq!(
        f.editor.read_with(&f.vcx, |e, _| e.recorded()),
        Some(keys("secondary-k secondary-l"))
    );
    f.update(|e, _, cx| e.save_recording(false, cx));
    let section = format!("[[{}.bind]]", Platform::current().name());
    assert!(f.file().contains(&section), "{}", f.file());
    assert_eq!(f.labels("app.licenses").len(), 1);
}

#[gpui::test]
fn escape_cancels_recording_without_closing_the_window(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.click("keymap-add:app.about");
    f.press("escape");
    assert!(f.editor.read_with(&f.vcx, |e, _| e.recording().is_none()));
    assert!(f.vcx.update(|_, cx| settings_window(cx)).is_some());
    assert!(f.file().is_empty(), "nothing written");
}

#[gpui::test]
fn conflicts_are_shown_and_resolved(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let save = keys("secondary-s");
    // Keep both: the new binding wins, Save keeps its key elsewhere.
    f.update(|e, window, cx| e.start_recording(id("app.about"), None, window, cx));
    f.press("secondary-s");
    let conflicts = f.editor.read_with(&f.vcx, |e, cx| e.conflicts(cx));
    assert_eq!(
        conflicts,
        vec![("Save".to_owned(), save.label(Platform::current()))]
    );
    assert!(f.bounds("keymap-conflict").is_some());
    assert!(f.bounds("keymap-save").is_none());
    f.click("keymap-keep-both");
    let error = f
        .editor
        .read_with(&f.vcx, |e, _| e.error().map(str::to_owned));
    assert_eq!(error, None);
    assert_eq!(
        f.labels("app.about"),
        vec![save.label(Platform::current())],
        "{}",
        f.file()
    );

    // Replace: Save loses the key.
    f.update(|e, _, cx| e.reset(id("app.about"), cx));
    assert!(f.labels("app.about").is_empty());
    f.update(|e, window, cx| e.start_recording(id("app.about"), None, window, cx));
    f.press("secondary-s");
    f.click("keymap-replace");
    assert_eq!(f.labels("app.about"), vec![save.label(Platform::current())]);
    assert!(f.labels("file.save").is_empty());
    assert!(f.file().contains("unbind"), "{}", f.file());

    // Reset brings Save's key back once nothing shadows it.
    f.update(|e, _, cx| e.reset(id("file.save"), cx));
    assert!(f.labels("file.save").is_empty(), "About still has the key");
    f.update(|e, _, cx| e.reset(id("app.about"), cx));
    assert_eq!(f.labels("file.save"), vec![save.label(Platform::current())]);
    assert!(!f.file().contains("bind"), "{}", f.file());
}

#[gpui::test]
fn removes_and_changes_a_key(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let label = keys("secondary-shift-s").label(Platform::current());
    f.update(|e, window, cx| e.set_query("save as", window, cx));
    // A click on the key records its replacement.
    f.click(&format!("keymap-key:file.save_as:{label}"));
    f.press("secondary-alt-s");
    f.click("keymap-save");
    assert_eq!(
        f.labels("file.save_as"),
        vec![keys("secondary-alt-s").label(Platform::current())]
    );
    // × removes it.
    let new_label = keys("secondary-alt-s").label(Platform::current());
    f.click(&format!("keymap-remove:file.save_as:{new_label}"));
    assert!(f.labels("file.save_as").is_empty());
    assert!(f.editor.read_with(&f.vcx, |e, _| e.error().is_none()));
}

#[gpui::test]
fn comments_in_the_file_are_kept(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let path = f.locations.keymap_file();
    fs::create_dir_all(path.parent().expect("config dir")).expect("config dir");
    let text = "# My keys\nversion = 1\n\n# About\n[[bind]]\nkeys = \"f7\" # handy\ncommand = \"app.about\"\n";
    fs::write(&path, text).expect("written");
    f.vcx.update(|_, cx| crate::keymap::load_file(cx, &path));
    draw(&mut f.vcx);
    assert_eq!(f.labels("app.about"), vec!["F7".to_owned()]);
    f.update(|e, window, cx| e.start_recording(id("app.licenses"), None, window, cx));
    f.press("f8");
    f.click("keymap-save");
    assert!(f.file().starts_with(text), "{}", f.file());
    assert_eq!(f.labels("app.licenses"), vec!["F8".to_owned()]);
}

#[gpui::test]
fn safe_mode_applies_without_saving(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temporary folder");
    let locations = Locations::with_root(dir.path().join("app"));
    let settings = AppSettings::new(None, Some(ThemeChoice::Light))
        .with_locations(locations.clone())
        .with_safe_mode();
    let window = cx.update(|cx| {
        crate::init(cx, settings);
        super::open(cx).expect("opens")
    });
    let (_, view) = cx.update(|cx| settings_window(cx)).expect("open");
    let mut vcx = VisualTestContext::from_window(window, cx);
    let editor = vcx.update(|window, cx| view.update(cx, |v, cx| v.keymap_editor(window, cx)));
    vcx.update(|window, cx| {
        editor.update(cx, |e, cx| {
            e.start_recording(id("app.about"), None, window, cx)
        });
    });
    draw(&mut vcx);
    vcx.simulate_keystrokes("f7");
    vcx.update(|_, cx| editor.update(cx, |e, cx| e.save_recording(false, cx)));
    draw(&mut vcx);
    let labels = vcx.update(|_, cx| cx.global::<Keybindings>().keymap().key_labels("app.about"));
    assert_eq!(labels, vec!["F7".to_owned()]);
    assert!(!locations.keymap_file().exists());
}

/// The keymap editor for assistive technology: a group of named rows,
/// each key a button named by the key and its command, Add, Remove and
/// Reset named by their command; the keyboard reaches Add, Enter starts
/// recording into a named key field, and a conflict is announced.
#[gpui::test]
fn the_keymap_editor_names_its_controls_and_the_keyboard_works_it(cx: &mut TestAppContext) {
    use crate::a11y::testing::{focused, named, with_role};
    use crate::a11y::{Live, Role};

    let mut f = fixture(cx);
    f.update(|e, window, cx| e.set_query("save as", window, cx));
    let title = f
        .editor
        .read_with(&f.vcx, |e, cx| e.rows(cx)[0].title.clone());
    let label = keys("secondary-shift-s").label(Platform::current());
    let vcx = &mut f.vcx;
    assert!(
        with_role(vcx, Role::Group)
            .iter()
            .any(|n| n.label.as_deref() == Some("Keymap")),
        "the editor is a named group"
    );
    let row = with_role(vcx, Role::Group)
        .into_iter()
        .find(|n| n.label.as_deref() == Some(title.as_str()))
        .expect("the command's row");
    assert_eq!(row.description.as_deref(), Some("file.save_as"));
    let key = named(vcx, &format!("{label}: change this key of {title}"));
    assert_eq!(key.role, Some(Role::Button));
    assert!(key.focusable && key.clickable);
    let remove = named(vcx, &format!("Remove {label} from {title}"));
    assert_eq!(remove.role, Some(Role::Button));
    let add_name = format!("Add a key for {title}");
    assert!(named(vcx, &add_name).focusable);
    assert!(
        with_role(vcx, Role::Heading)
            .iter()
            .any(|h| h.level == Some(2)),
        "the category heading"
    );

    // Tab to Add; Enter starts recording into the key field.
    for _ in 0..80 {
        if focused(vcx).flatten().as_deref() == Some(add_name.as_str()) {
            break;
        }
        vcx.simulate_keystrokes("tab");
        draw(vcx);
    }
    assert_eq!(focused(vcx).flatten(), Some(add_name.clone()));
    vcx.simulate_keystrokes("enter");
    vcx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").expect("a key"),
    });
    draw(vcx);
    assert_eq!(
        f.editor.read_with(&f.vcx, |e, _| e.recording().cloned()),
        Some(id("file.save_as"))
    );
    let vcx = &mut f.vcx;
    let field = named(vcx, &format!("New keys for {title}"));
    assert_eq!(field.role, Some(Role::TextInput));
    assert_eq!(field.value.as_deref(), Some("Press the keys…"));
    assert_eq!(field.live, Some(Live::Polite));
    assert_eq!(named(vcx, "Applies to").role, Some(Role::RadioGroup));
    f.press("secondary-s");
    let conflict = with_role(&mut f.vcx, Role::Status)
        .into_iter()
        .find(|n| {
            n.label
                .as_deref()
                .is_some_and(|l| l.starts_with("Also used by: "))
        })
        .expect("the conflict is announced");
    assert_eq!(conflict.live, Some(Live::Polite));
    for name in ["Replace", "Keep both", "Cancel"] {
        assert_eq!(named(&mut f.vcx, name).role, Some(Role::Button), "{name}");
    }
}
