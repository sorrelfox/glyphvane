// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Settings export and import in a headless settings window: the palette
//! commands, the dialogs (replaced by fakes), the preview, leaving changes
//! out, Apply and Cancel, and the files written.

use std::fs;
use std::path::PathBuf;

use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use gpui::{App, Entity, Modifiers, TestAppContext, VisualTestContext};
use gv_settings::{LayerKind, Locations, SettingsFile};

use super::model::SectionId;
use super::{SettingsWindow, settings_window};
use crate::files::open_dialog::{self, OpenAnswer, OpenPathDialog, OpenPrompt};
use crate::files::save_as::{self, SaveAnswer, SavePathDialog, SavePrompt};
use crate::keymap::actions;
use crate::settings::AppSettings;

const GLOBAL_TEXT: &str =
    "# Mine\nversion = 1\n\n[appearance]\ntheme = \"dark\"\nzoom_percent = 110\n";

struct OpenFake(PathBuf);

impl OpenPathDialog for OpenFake {
    fn ask(&self, prompt: OpenPrompt, _: &mut App) -> LocalBoxFuture<'static, OpenAnswer> {
        assert_eq!(prompt, OpenPrompt::Files);
        let path = self.0.clone();
        async move { Ok(Some(vec![path])) }.boxed_local()
    }
}

struct SaveFake(PathBuf);

impl SavePathDialog for SaveFake {
    fn ask(&self, prompt: &SavePrompt, _: &mut App) -> LocalBoxFuture<'static, SaveAnswer> {
        assert_eq!(prompt.suggested_name, gv_settings::EXPORT_FILE_NAME);
        let path = self.0.clone();
        async move { Ok(Some(path)) }.boxed_local()
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    locations: Locations,
    view: Entity<SettingsWindow>,
    vcx: VisualTestContext,
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

/// The settings window opened by `action`, with fake dialogs.
fn fixture(cx: &mut TestAppContext, action: &dyn gpui::Action) -> Fixture {
    let dir = tempfile::tempdir().expect("temporary folder");
    let locations = Locations::with_root(dir.path().join("app"));
    fs::create_dir_all(locations.config_dir()).expect("config dir");
    fs::write(locations.settings_file(), GLOBAL_TEXT).expect("settings written");
    let import = dir.path().join("import.toml");
    let export = dir.path().join("export.toml");
    let file = SettingsFile::load(locations.settings_file(), LayerKind::Global).expect("loads");
    cx.update(|cx| {
        crate::init(
            cx,
            AppSettings::new(Some(file), None).with_locations(locations.clone()),
        );
        open_dialog::set_dialog(cx, OpenFake(import));
        save_as::set_dialog(cx, SaveFake(export));
        cx.dispatch_action(action);
    });
    let (window, view) = cx
        .update(|cx| settings_window(cx))
        .expect("the command opens the settings window");
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    Fixture {
        dir,
        locations,
        view,
        vcx,
    }
}

fn click(vcx: &mut VisualTestContext, selector: &'static str) {
    let bounds = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    vcx.simulate_click(bounds.center(), Modifiers::none());
    draw(vcx);
}

#[gpui::test]
fn export_writes_the_valid_settings(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &actions::settings::Export);
    let path = f.dir.path().join("export.toml");
    let text = fs::read_to_string(&path).expect("the export is written");
    assert!(text.contains("theme = \"dark\""), "{text}");
    assert!(text.contains("zoom_percent = 110"), "{text}");
    assert!(!text.contains("# Mine"));
    let section = f.view.read_with(&f.vcx, |v, _| v.section());
    assert_eq!(section, SectionId::About);
    let status = f
        .view
        .read_with(&f.vcx, |v, _| v.transfer_status().map(str::to_owned));
    assert!(status.is_some_and(|s| s.starts_with("Settings exported to")));
    assert!(f.vcx.debug_bounds("settings-transfer-status").is_some());
    // The About section's button exports again.
    fs::remove_file(&path).expect("removed");
    click(&mut f.vcx, "settings-export");
    assert!(path.exists());
}

#[gpui::test]
fn import_previews_then_applies_the_chosen_changes(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &actions::app::Settings);
    let import = f.dir.path().join("import.toml");
    fs::write(
        &import,
        "version = 1\n[appearance]\ntheme = \"light\"\nzoom_percent = 110\n\
         [privacy_and_security]\nremote_content = \"allow\"\n\
         [editor]\nlarger_headings = true\nnope = 1\n",
    )
    .expect("written");
    f.vcx.update(|window, cx| {
        window.dispatch_action(Box::new(actions::settings::Import), cx);
    });
    draw(&mut f.vcx);
    let pending = f
        .view
        .read_with(&f.vcx, |v, _| v.pending_import().cloned())
        .expect("a preview");
    let keys: Vec<_> = pending.preview.changes.iter().map(|c| c.key).collect();
    assert_eq!(
        keys,
        [
            "appearance.theme",
            "editor.larger_headings",
            "privacy_and_security.remote_content"
        ]
    );
    assert_eq!(pending.preview.unchanged, 1);
    // The less strict security value starts left out.
    assert!(
        !pending
            .chosen
            .contains("privacy_and_security.remote_content")
    );
    assert_eq!(pending.chosen.len(), 2);
    assert!(f.vcx.debug_bounds("settings-import-apply").is_some());
    // Leave the heading change out too.
    click(&mut f.vcx, "settings-import-switch-editor.larger_headings");
    click(&mut f.vcx, "settings-import-apply");
    assert!(
        f.view
            .read_with(&f.vcx, |v, _| v.pending_import().is_none())
    );
    let text = fs::read_to_string(f.locations.settings_file()).expect("read");
    assert!(text.starts_with("# Mine\n"), "comments kept: {text}");
    assert!(text.contains("theme = \"light\""), "{text}");
    assert!(!text.contains("larger_headings"), "{text}");
    assert!(!text.contains("remote_content"), "{text}");
    let theme = f.vcx.update(|_, cx| {
        cx.global::<AppSettings>()
            .settings()
            .appearance
            .theme
            .as_str()
    });
    assert_eq!(theme, "light", "applied at once");
    let status = f
        .view
        .read_with(&f.vcx, |v, _| v.transfer_status().map(str::to_owned));
    assert_eq!(status.as_deref(), Some("Imported 1 setting."));
}

#[gpui::test]
fn cancel_and_unreadable_files_change_nothing(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &actions::app::Settings);
    let import = f.dir.path().join("import.toml");
    fs::write(&import, "version = 1\n[appearance]\ntheme = \"light\"\n").expect("written");
    f.view
        .update_in(&mut f.vcx, |v, _, cx| v.import_from(&import, cx));
    draw(&mut f.vcx);
    click(&mut f.vcx, "settings-import-cancel");
    assert!(
        f.view
            .read_with(&f.vcx, |v, _| v.pending_import().is_none())
    );
    let text = fs::read_to_string(f.locations.settings_file()).expect("read");
    assert_eq!(text, GLOBAL_TEXT);

    fs::write(&import, "[appearance\n").expect("written");
    f.view
        .update_in(&mut f.vcx, |v, _, cx| v.import_from(&import, cx));
    let (pending, status) = f.view.read_with(&f.vcx, |v, _| {
        (
            v.pending_import().is_some(),
            v.transfer_status().map(str::to_owned),
        )
    });
    assert!(!pending);
    assert!(status.is_some_and(|s| s.starts_with("Cannot import")));

    // A file that matches the settings has nothing to apply.
    fs::write(&import, GLOBAL_TEXT).expect("written");
    f.view
        .update_in(&mut f.vcx, |v, _, cx| v.import_from(&import, cx));
    draw(&mut f.vcx);
    assert!(f.vcx.debug_bounds("settings-import-nothing").is_some());
}
