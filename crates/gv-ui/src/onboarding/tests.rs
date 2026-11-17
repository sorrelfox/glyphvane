// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The onboarding window in a headless window: shown once, every choice
//! saved for all vaults, Open folder through the (fake) dialog, the
//! default-app offer, and Get started / Escape ending it for good.

use std::fs;
use std::path::PathBuf;

use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use gpui::{App, Modifiers, TestAppContext, VisualTestContext};
use gv_settings::{AppState, LayerKind, Locations, SettingsFile};

use super::*;
use crate::app_state::StateStore;
use crate::files::open_dialog::{OpenAnswer, OpenPathDialog};

struct FolderFake(PathBuf);

impl OpenPathDialog for FolderFake {
    fn ask(&self, prompt: OpenPrompt, _: &mut App) -> LocalBoxFuture<'static, OpenAnswer> {
        assert_eq!(prompt, OpenPrompt::Folder);
        let path = self.0.clone();
        async move { Ok(Some(vec![path])) }.boxed_local()
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    locations: Locations,
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn setup(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().expect("temporary folder");
    let locations = Locations::with_root(dir.path().join("app"));
    fs::create_dir_all(locations.config_dir()).expect("config dir");
    fs::write(locations.settings_file(), "version = 1\n").expect("settings");
    let vault = dir.path().join("Notes");
    fs::create_dir_all(&vault).expect("vault");
    fs::write(vault.join("a.md"), "# A\n").expect("note");
    let file = SettingsFile::load(locations.settings_file(), LayerKind::Global).expect("loads");
    cx.update(|cx| {
        crate::init(
            cx,
            AppSettings::new(Some(file), None).with_locations(locations.clone()),
        );
        crate::app_state::install(cx);
        open_dialog::set_dialog(cx, FolderFake(vault));
    });
    Fixture {
        _dir: dir,
        locations,
    }
}

fn opened(cx: &mut TestAppContext) -> VisualTestContext {
    let window = cx
        .update(show_if_first_run)
        .expect("the first run shows the onboarding");
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    vcx
}

fn click(vcx: &mut VisualTestContext, selector: &'static str) {
    let bounds = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    vcx.simulate_click(bounds.center(), Modifiers::none());
    draw(vcx);
}

#[gpui::test]
fn choices_are_saved_and_the_first_run_ends(cx: &mut TestAppContext) {
    let f = setup(cx);
    let mut vcx = opened(cx);
    for selector in [
        "onboarding-title",
        "onboarding-open-folder",
        "onboarding-switch-general.check_for_updates",
        "onboarding-switch-privacy_and_security.crash_reports",
        "onboarding-done",
    ] {
        assert!(vcx.debug_bounds(selector).is_some(), "{selector}");
    }
    click(&mut vcx, "onboarding-switch-general.check_for_updates");
    click(
        &mut vcx,
        "onboarding-switch-privacy_and_security.crash_reports",
    );
    vcx.update(|_, cx| {
        let view = onboarding_window(cx).expect("open").1;
        view.update(cx, |this, cx| {
            this.choose("appearance.theme", Value::String("dark".into()), cx);
            this.choose(
                "profiles.default_profile",
                Value::String("obsidian".into()),
                cx,
            );
            this.choose(
                "privacy_and_security.remote_content",
                Value::String("block".into()),
                cx,
            );
        });
    });
    let text = fs::read_to_string(f.locations.settings_file()).expect("read");
    for line in [
        "check_for_updates = true",
        "crash_reports = true",
        "theme = \"dark\"",
        "default_profile = \"obsidian\"",
        "remote_content = \"block\"",
    ] {
        assert!(text.contains(line), "{line} in {text}");
    }
    assert!(!AppState::load(&f.locations.app_state_file()).onboarding_done);
    let done = vcx.debug_bounds("onboarding-done").expect("drawn");
    vcx.simulate_click(done.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(cx.update(|cx| onboarding_window(cx).is_none()));
    assert!(AppState::load(&f.locations.app_state_file()).onboarding_done);
    // Never again.
    cx.update(|cx| {
        crate::app_state::set_store(cx, StateStore::load(Some(f.locations.app_state_file())));
        assert!(!should_show(cx));
        assert!(show_if_first_run(cx).is_none());
    });
}

#[gpui::test]
fn escape_ends_it_too(cx: &mut TestAppContext) {
    let f = setup(cx);
    let mut vcx = opened(cx);
    vcx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.update(|cx| onboarding_window(cx).is_none()));
    assert!(AppState::load(&f.locations.app_state_file()).onboarding_done);
}

#[gpui::test]
fn open_folder_opens_a_vault_window(cx: &mut TestAppContext) {
    let _f = setup(cx);
    let mut vcx = opened(cx);
    let before = cx.update(|cx| cx.windows().len());
    click(&mut vcx, "onboarding-open-folder");
    cx.run_until_parked();
    let after = cx.update(|cx| cx.windows().len());
    assert_eq!(after, before + 1, "a main window opened");
}

#[gpui::test]
fn the_default_app_offer_answers(cx: &mut TestAppContext) {
    let _f = setup(cx);
    let mut vcx = opened(cx);
    if default_app::offer() == Offer::Explain {
        return;
    }
    assert!(vcx.debug_bounds("onboarding-default-app").is_some());
    // The row is below the fold of the test window: run its button's work.
    cx.update(|cx| {
        let view = onboarding_window(cx).expect("open").1;
        view.update(cx, |this, cx| this.make_default_app(cx));
    });
    cx.run_until_parked();
    let status = cx.update(|cx| {
        onboarding_window(cx)
            .expect("open")
            .1
            .read(cx)
            .default_app_status()
            .map(str::to_owned)
    });
    assert!(status.is_some());
}

#[gpui::test]
fn safe_mode_never_shows_it(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, None).with_safe_mode());
        assert!(!should_show(cx));
    });
}
