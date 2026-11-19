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
fn the_flatpak_explains_the_default_app_and_offers_no_update_check(cx: &mut TestAppContext) {
    let _flatpak = crate::updates::ForceFlatpak::new(true);
    let _f = setup(cx);
    let mut vcx = opened(cx);
    assert!(
        vcx.debug_bounds("onboarding-switch-general.check_for_updates")
            .is_none()
    );
    assert!(
        vcx.debug_bounds("onboarding-switch-privacy_and_security.crash_reports")
            .is_some()
    );
    if cfg!(target_os = "linux") {
        // No button that would run xdg-mime; the row explains instead.
        assert_eq!(default_app::offer(), Offer::Explain);
        assert!(vcx.debug_bounds("onboarding-default-app").is_none());
    }
}

#[gpui::test]
fn outside_the_flatpak_the_update_check_is_offered(cx: &mut TestAppContext) {
    let _flatpak = crate::updates::ForceFlatpak::new(false);
    let _f = setup(cx);
    let mut vcx = opened(cx);
    assert_eq!(
        vcx.debug_bounds("onboarding-switch-general.check_for_updates")
            .is_some(),
        crate::updates::enabled_by_build()
    );
}

#[gpui::test]
fn safe_mode_never_shows_it(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, None).with_safe_mode());
        assert!(!should_show(cx));
    });
}

/// Presses and releases `key` (GPUI clicks a focused control on the
/// release of Enter or Space).
fn press(vcx: &mut VisualTestContext, key: &'static str) {
    vcx.simulate_keystrokes(key);
    vcx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse(key).expect("a key"),
    });
    draw(vcx);
}

/// The window for assistive technology: a dialog with its title as the
/// first heading, a heading per group, every row a named group, the
/// switches and the choices named by their rows; Tab reaches every
/// control and Space works the focused one.
#[gpui::test]
fn the_onboarding_names_its_parts_and_the_keyboard_reaches_them(cx: &mut TestAppContext) {
    use crate::a11y::testing::{focused, named, nodes, with_role};
    use crate::a11y::{Role, Toggled};

    let _f = setup(cx);
    let mut vcx = opened(cx);
    let vcx = &mut vcx;
    let headings: Vec<_> = with_role(vcx, Role::Heading)
        .into_iter()
        .map(|n| (n.label.unwrap_or_default(), n.level))
        .collect();
    assert_eq!(headings[0], ("Welcome to Glyphvane".to_owned(), Some(1)));
    assert!(
        headings.contains(&("Look".to_owned(), Some(2))),
        "{headings:?}"
    );
    assert_eq!(headings.len(), 6, "{headings:?}");
    // The window and its title share a name.
    let dialog: Vec<_> = nodes(vcx)
        .into_iter()
        .filter(|n| n.role == Some(Role::Dialog))
        .collect();
    assert_eq!(dialog.len(), 1);
    assert_eq!(dialog[0].label.as_deref(), Some("Welcome to Glyphvane"));

    let theme = with_role(vcx, Role::RadioGroup)
        .into_iter()
        .find(|n| n.label.as_deref() == Some("Theme"))
        .expect("the theme choice");
    assert!(theme.label.is_some());
    let updates = with_role(vcx, Role::Switch)
        .into_iter()
        .find(|n| n.label.as_deref() == Some("Check for updates"))
        .expect("the update switch");
    assert_eq!(updates.toggled, Some(Toggled::False));
    assert!(updates.focusable);
    let rows: Vec<_> = with_role(vcx, Role::Group)
        .into_iter()
        .filter_map(|n| n.label)
        .collect();
    assert!(rows.iter().any(|r| r == "Save crash reports"), "{rows:?}");
    for label in ["Open folder…", "Get started"] {
        let b = named(vcx, label);
        assert_eq!(b.role, Some(Role::Button), "{label}");
        assert!(b.focusable && b.clickable, "{label}");
    }

    // Tab reaches every control and comes back round.
    let stops = nodes(vcx)
        .into_iter()
        .filter(|n| n.focusable && !n.disabled)
        .count();
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..stops * 2 + 4 {
        vcx.simulate_keystrokes("tab");
        draw(vcx);
        let Some(Some(label)) = focused(vcx) else {
            continue;
        };
        if seen.first() == Some(&label) {
            break;
        }
        seen.push(label);
    }
    // A choice's segments are reached inside its group.
    for label in ["Open folder…", "Theme", "Check for updates", "Get started"] {
        assert!(
            seen.iter().any(|s| s == label),
            "Tab missed {label}: {seen:?}"
        );
    }
    while focused(vcx).flatten().as_deref() != Some("Check for updates") {
        vcx.simulate_keystrokes("tab");
        draw(vcx);
    }
    press(vcx, "space");
    let updates = with_role(vcx, Role::Switch)
        .into_iter()
        .find(|n| n.label.as_deref() == Some("Check for updates"))
        .expect("the update switch");
    assert_eq!(updates.toggled, Some(Toggled::True));
}
