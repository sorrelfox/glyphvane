// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The update checker with a fake transport (no network): opt-in, the
//! daily schedule, the notice and its buttons, and Check for updates with
//! and without the opt-in.

use std::sync::{Arc, Mutex};

use gpui::{
    Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    VisualTestContext, Window, div,
};
use gv_settings::{LayerKind, Locations, SettingsFile};

use super::github::{HttpResponse, Transport};
use super::*;

/// Answers every request with the same release and counts the requests.
struct Fake {
    tag: Mutex<String>,
    requests: Mutex<usize>,
}

impl Fake {
    fn new(tag: &str) -> Arc<Self> {
        Arc::new(Self {
            tag: Mutex::new(tag.to_owned()),
            requests: Mutex::new(0),
        })
    }

    fn requests(&self) -> usize {
        *self.requests.lock().expect("lock")
    }
}

impl Transport for Fake {
    fn get(&self, url: &str, _: Option<&str>) -> Result<HttpResponse, String> {
        assert_eq!(url, github::LATEST_RELEASE_URL);
        *self.requests.lock().expect("lock") += 1;
        let tag = self.tag.lock().expect("lock").clone();
        Ok(HttpResponse {
            status: 200,
            etag: None,
            body: format!(
                r#"{{"tag_name":"{tag}","html_url":"https://github.com/sorrelfox/glyphvane/releases/tag/{tag}","body":"- Faster."}}"#
            )
            .into_bytes(),
        })
    }
}

/// A window that shows the application notice, as a main window does.
struct Host;

impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = crate::Appearance::theme(cx);
        div().size_full().children(notices::render(theme, cx))
    }
}

const NEWER: &str = "v99.0.0";

fn setup(
    cx: &mut TestAppContext,
    opted_in: bool,
    fake: &Arc<Fake>,
) -> (tempfile::TempDir, Locations, VisualTestContext) {
    let dir = tempfile::tempdir().expect("temporary folder");
    let locations = Locations::with_root(dir.path());
    std::fs::create_dir_all(locations.config_dir()).expect("config dir");
    std::fs::write(
        locations.settings_file(),
        format!("version = 1\n[general]\ncheck_for_updates = {opted_in}\n"),
    )
    .expect("settings");
    let file = SettingsFile::load(locations.settings_file(), LayerKind::Global).expect("loads");
    cx.update(|cx| {
        crate::init(
            cx,
            AppSettings::new(Some(file), None).with_locations(locations.clone()),
        );
        crate::app_state::install(cx);
        set_transport(cx, fake.clone());
        set_clock(cx, 1_000_000);
    });
    let window = cx.add_window(|_, _| Host);
    let vcx = VisualTestContext::from_window(window.into(), cx);
    (dir, locations, vcx)
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

#[gpui::test]
fn nothing_is_checked_without_the_opt_in(cx: &mut TestAppContext) {
    let fake = Fake::new(NEWER);
    let (_dir, _, mut vcx) = setup(cx, false, &fake);
    cx.update(|cx| {
        assert!(!automatic(cx));
        tick(cx);
    });
    draw(&mut vcx);
    assert_eq!(fake.requests(), 0);
    assert!(cx.update(|cx| notices::current(cx).is_none()));
}

#[gpui::test]
fn a_daily_check_shows_the_notice(cx: &mut TestAppContext) {
    let fake = Fake::new(NEWER);
    let (_dir, locations, mut vcx) = setup(cx, true, &fake);
    cx.update(tick);
    draw(&mut vcx);
    assert_eq!(fake.requests(), 1);
    let notice = cx.update(|cx| notices::current(cx)).expect("the notice");
    assert_eq!(notice.topic, AppTopic::Update);
    assert_eq!(notice.title.as_ref(), "Glyphvane 99.0.0 is available");
    assert!(vcx.debug_bounds("app-notice").is_some());
    assert!(vcx.debug_bounds("app-notice-action-0").is_some());
    let state = gv_settings::AppState::load(&locations.app_state_file());
    assert_eq!(state.updates.last_attempt, Some(1_000_000));
    assert_eq!(state.updates.latest.map(|r| r.tag).as_deref(), Some(NEWER));
    // Not again within the day.
    cx.update(|cx| {
        set_clock(cx, 1_000_000 + github::INTERVAL - 1);
        tick(cx);
    });
    draw(&mut vcx);
    assert_eq!(fake.requests(), 1);
    cx.update(|cx| {
        set_clock(cx, 1_000_000 + github::INTERVAL);
        tick(cx);
    });
    draw(&mut vcx);
    assert_eq!(fake.requests(), 2);
}

#[gpui::test]
fn what_is_new_and_skip_this_version(cx: &mut TestAppContext) {
    let fake = Fake::new(NEWER);
    let (_dir, locations, mut vcx) = setup(cx, true, &fake);
    cx.update(tick);
    draw(&mut vcx);
    // What's new: the notes in a dialog, then the release page.
    let bounds = vcx.debug_bounds("app-notice-action-0").expect("drawn");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::none());
    draw(&mut vcx);
    let (message, detail) = cx.pending_prompt().expect("a dialog");
    assert_eq!(message, "What's new in Glyphvane 99.0.0");
    assert!(detail.contains("Faster"));
    cx.simulate_prompt_answer("Open release page");
    draw(&mut vcx);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/sorrelfox/glyphvane/releases/tag/v99.0.0")
    );
    // Skip this version: the notice goes and stays away.
    let bounds = vcx.debug_bounds("app-notice-action-1").expect("drawn");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::none());
    draw(&mut vcx);
    assert!(cx.update(|cx| notices::current(cx).is_none()));
    let state = gv_settings::AppState::load(&locations.app_state_file());
    assert_eq!(state.updates.skipped_version.as_deref(), Some(NEWER));
    cx.update(|cx| {
        set_clock(cx, 1_000_000 + github::INTERVAL);
        tick(cx);
    });
    draw(&mut vcx);
    assert_eq!(fake.requests(), 2);
    assert!(cx.update(|cx| notices::current(cx).is_none()));
    // A still newer release is announced again.
    *fake.tag.lock().expect("lock") = "v100.0.0".to_owned();
    cx.update(|cx| {
        set_clock(cx, 1_000_000 + 2 * github::INTERVAL);
        tick(cx);
    });
    draw(&mut vcx);
    assert!(cx.update(|cx| notices::current(cx).is_some()));
}

#[gpui::test]
fn check_for_updates_asks_first_without_the_opt_in(cx: &mut TestAppContext) {
    let fake = Fake::new(NEWER);
    let (_dir, locations, mut vcx) = setup(cx, false, &fake);
    cx.update(|cx| cx.dispatch_action(&actions::app::CheckForUpdates));
    draw(&mut vcx);
    let (message, detail) = cx.pending_prompt().expect("a question");
    assert_eq!(message, "Check for updates?");
    assert!(detail.contains("api.github.com"));
    assert_eq!(fake.requests(), 0, "nothing before the answer");
    cx.simulate_prompt_answer("Cancel");
    draw(&mut vcx);
    assert_eq!(fake.requests(), 0);

    // Check once: one request, the answer in a dialog, the opt-in unchanged.
    cx.update(|cx| cx.dispatch_action(&actions::app::CheckForUpdates));
    draw(&mut vcx);
    cx.simulate_prompt_answer("Check once");
    draw(&mut vcx);
    assert_eq!(fake.requests(), 1);
    let (message, _) = cx.pending_prompt().expect("the answer");
    assert_eq!(message, "Glyphvane 99.0.0 is available");
    cx.simulate_prompt_answer("Later");
    draw(&mut vcx);
    assert!(!cx.update(|cx| automatic(cx)));

    // Check daily turns the opt-in on.
    cx.update(|cx| cx.dispatch_action(&actions::app::CheckForUpdates));
    draw(&mut vcx);
    cx.simulate_prompt_answer("Check daily");
    draw(&mut vcx);
    assert_eq!(fake.requests(), 2);
    cx.simulate_prompt_answer("Later");
    assert!(cx.update(|cx| automatic(cx)));
    let text = std::fs::read_to_string(locations.settings_file()).expect("read");
    assert!(text.contains("check_for_updates = true"), "{text}");
}

#[gpui::test]
fn check_for_updates_says_when_it_is_up_to_date(cx: &mut TestAppContext) {
    let fake = Fake::new("v0.0.1");
    let (_dir, _, mut vcx) = setup(cx, true, &fake);
    cx.update(|cx| cx.dispatch_action(&actions::app::CheckForUpdates));
    draw(&mut vcx);
    assert_eq!(fake.requests(), 1, "opted in: no question first");
    let (message, _) = cx.pending_prompt().expect("the answer");
    assert_eq!(message, "Glyphvane is up to date");
    cx.simulate_prompt_answer("OK");
    assert!(cx.update(|cx| notices::current(cx).is_none()));
}

#[gpui::test]
fn safe_mode_checks_nothing_on_its_own(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let settings = AppSettings::new(None, None).with_safe_mode();
        crate::init(cx, settings);
        assert!(!automatic(cx));
    });
}

#[test]
fn long_notes_are_cut_for_the_dialog() {
    assert_eq!(notes_excerpt("  "), "The release page has the details.");
    assert_eq!(notes_excerpt("- One"), "- One");
    let long = "x".repeat(2000);
    let cut = notes_excerpt(&long);
    assert!(cut.starts_with(&"x".repeat(1500)));
    assert!(cut.ends_with("The release page has the rest."));
}

#[test]
fn the_build_allows_the_check_by_default() {
    assert!(enabled_by_build());
}

#[test]
fn a_flatpak_sandbox_is_recognised_by_its_info_file() {
    let root = tempfile::tempdir().expect("a temporary folder");
    assert!(!flatpak_info_in(root.path()));
    std::fs::write(root.path().join(".flatpak-info"), "[Application]\n").expect("written");
    assert!(flatpak_info_in(root.path()));
}

/// A long detail wraps in its own column, clear of the buttons and inside
/// the window, at every width.
#[gpui::test]
fn a_long_detail_wraps_clear_of_the_buttons(cx: &mut TestAppContext) {
    use gpui::{AppContext as _, Bounds, WindowBounds, WindowOptions, point, px, size};
    let fake = Fake::new(NEWER);
    let (_dir, _, _) = setup(cx, false, &fake);
    let noop: notices::Handler = std::rc::Rc::new(|_, _| {});
    let button = |label: &'static str| notices::AppButton {
        label: label.into(),
        run: noop.clone(),
        dismisses: false,
    };
    let notice = notices::AppNotice {
        topic: notices::AppTopic::Update,
        kind: NoticeKind::Information,
        title: "Glyphvane 99.0.0 is available.".into(),
        detail: Some(
            "This release changes how notes load remote images and media, how links that \
             leave the application are confirmed, and how CSS snippets are applied to the \
             notes of every vault."
                .into(),
        ),
        buttons: vec![
            button("What's new"),
            button("Skip this version"),
            button("Remind me later"),
        ],
        on_dismiss: None,
    };
    cx.update(|cx| notices::show(cx, notice));
    for width in [1200.0, 800.0, 500.0] {
        let (window, _) = cx.update(|cx| {
            let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(width), px(400.0)));
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            };
            gpui::open_window(options, cx, |_, cx| cx.new(|_| Host)).expect("test window opens")
        });
        let mut vcx = VisualTestContext::from_window(window, cx);
        draw(&mut vcx);
        let ids = [
            "app-notice-action-0",
            "app-notice-action-1",
            "app-notice-action-2",
            "app-notice-dismiss",
        ];
        crate::files::notice::assert_clear_of_buttons(&mut vcx, width, "app-notice-detail", &ids);
    }
}
