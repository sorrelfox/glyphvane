// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The About window in a headless window: `app.about` and `app.licenses`
//! open it on their pages, Copy debug info fills the clipboard, Escape
//! closes it. The debug info's text and the settings summary.

use gpui::{TestAppContext, VisualTestContext};

use super::*;
use crate::theme::ThemeChoice;

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn opened(cx: &mut TestAppContext, action: &dyn gpui::Action) -> VisualTestContext {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        cx.dispatch_action(action);
    });
    let (window, _) = cx
        .update(|cx| about_window(cx))
        .expect("the About window is open");
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    vcx
}

fn page(vcx: &mut VisualTestContext) -> Option<Page> {
    vcx.update(|_, cx| about_window(cx).map(|(_, view)| view.read(cx).page()))
}

#[gpui::test]
fn about_shows_the_version_and_its_buttons(cx: &mut TestAppContext) {
    let mut vcx = opened(cx, &About);
    assert_eq!(page(&mut vcx), Some(Page::About));
    for selector in [
        "about-name",
        "about-version",
        "about-licenses",
        "about-copy-debug-info",
        "about-close",
    ] {
        assert!(vcx.debug_bounds(selector).is_some(), "{selector}");
    }

    // Third-party licenses switches pages; About comes back.
    let licenses = vcx.debug_bounds("about-licenses").expect("drawn");
    vcx.simulate_click(licenses.center(), gpui::Modifiers::none());
    draw(&mut vcx);
    assert_eq!(page(&mut vcx), Some(Page::Licenses));
    assert!(vcx.debug_bounds("about-notices").is_some());
    let back = vcx.debug_bounds("about-back").expect("drawn");
    vcx.simulate_click(back.center(), gpui::Modifiers::none());
    draw(&mut vcx);
    assert_eq!(page(&mut vcx), Some(Page::About));

    // Copy debug info.
    let copy = vcx.debug_bounds("about-copy-debug-info").expect("drawn");
    vcx.simulate_click(copy.center(), gpui::Modifiers::none());
    draw(&mut vcx);
    let copied = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    let copied = copied.expect("copied");
    assert!(
        copied.starts_with(&format!("Glyphvane {VERSION}\n")),
        "{copied}"
    );

    // Escape closes the window.
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(cx.update(|cx| about_window(cx)).is_none());
}

#[gpui::test]
fn licenses_opens_on_the_notices(cx: &mut TestAppContext) {
    let mut vcx = opened(cx, &Licenses);
    assert_eq!(page(&mut vcx), Some(Page::Licenses));
    // Asking for About brings the same window to its first page.
    vcx.update(|_, cx| cx.dispatch_action(&About));
    draw(&mut vcx);
    assert_eq!(page(&mut vcx), Some(Page::About));
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
}

#[gpui::test]
fn copy_debug_info_works_without_a_window(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        cx.dispatch_action(&CopyDebugInfo);
        let text = cx
            .read_from_clipboard()
            .and_then(|c| c.text())
            .expect("copied");
        assert!(text.contains("Theme: Light"), "{text}");
        assert!(text.contains("Zoom: 100%"), "{text}");
        assert!(text.contains("Profile: no document"), "{text}");
    });
}

#[test]
fn the_notices_are_embedded() {
    assert!(THIRD_PARTY_NOTICES.starts_with("Third-party notices for Glyphvane"));
    assert!(THIRD_PARTY_NOTICES.contains("SIL Open Font License"));
}

#[test]
fn debug_info_reads_as_a_report() {
    let info = DebugInfo {
        version: "1.2.3",
        os: "linux",
        arch: "x86_64",
        display_server: Some("Wayland"),
        gpu: Some("llvmpipe (Mesa 25.0), software renderer".to_owned()),
        theme: "Dark (setting: System)".to_owned(),
        zoom_percent: 120,
        safe_mode: true,
        profile: Some("GfmFile".to_owned()),
        changed_settings: vec!["appearance.zoom_percent".to_owned()],
    };
    assert_eq!(
        info.to_string(),
        "Glyphvane 1.2.3\n\
         OS: linux (x86_64), Wayland\n\
         GPU: llvmpipe (Mesa 25.0), software renderer\n\
         Theme: Dark (setting: System)\n\
         Zoom: 120%\n\
         Safe mode: on\n\
         Profile: GfmFile\n\
         Settings changed: appearance.zoom_percent"
    );
    let bare = DebugInfo {
        display_server: None,
        gpu: None,
        profile: None,
        changed_settings: Vec::new(),
        safe_mode: false,
        ..info
    };
    let text = bare.to_string();
    assert!(text.contains("OS: linux (x86_64)\n"), "{text}");
    assert!(text.contains("GPU: not reported"), "{text}");
    assert!(text.ends_with("Settings: all defaults"), "{text}");
}

#[test]
fn the_settings_summary_names_only_changed_keys() {
    let mut settings = gv_settings::Settings::default();
    assert!(changed_settings(&settings).is_empty());
    settings.appearance.zoom_percent = 130;
    settings.general.viewer_mode = true;
    assert_eq!(
        changed_settings(&settings),
        ["appearance.zoom_percent", "general.viewer_mode"]
    );
}
