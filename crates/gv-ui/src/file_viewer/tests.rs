// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The built-in viewers: what each file is shown as, and the viewers in a
//! headless window.

use std::path::Path;

use gpui::{
    AnyWindowHandle, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};

use super::{
    FileViewer, ImageZoom, ViewerKind, ViewerStatus, fenced, kind, language_info, looks_like_text,
    sniff_text,
};
use crate::{AppSettings, ThemeChoice};

#[test]
fn files_are_told_apart_by_name() {
    assert_eq!(kind(Path::new("a/photo.JPG")), Some(ViewerKind::Image));
    assert_eq!(kind(Path::new("a/diagram.svg")), Some(ViewerKind::Image));
    assert_eq!(kind(Path::new("main.rs")), Some(ViewerKind::Text));
    assert_eq!(kind(Path::new("data.json")), Some(ViewerKind::Text));
    assert_eq!(kind(Path::new("Makefile")), Some(ViewerKind::Text));
    assert_eq!(kind(Path::new("LICENSE")), Some(ViewerKind::Text));
    assert_eq!(kind(Path::new(".gitignore")), Some(ViewerKind::Text));
    assert_eq!(kind(Path::new("archive.zip")), None);
    assert_eq!(kind(Path::new("notes")), None);
}

#[test]
fn text_is_sniffed_from_the_first_bytes() {
    assert!(looks_like_text(b"plain words\n"));
    assert!(looks_like_text("café".as_bytes()));
    // A character cut at the end of the sample is still text.
    assert!(looks_like_text(&"é".as_bytes()[..1]));
    assert!(!looks_like_text(b"\x89PNG\r\n\x1a\n\0\0"));
    assert!(!looks_like_text(&[0xff, 0xfe, b'a', b'b']));

    let dir = tempfile::tempdir().expect("temp dir");
    let script = dir.path().join("configure");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").expect("write");
    let blob = dir.path().join("blob.bin");
    std::fs::write(&blob, [0u8, 1, 2, 3]).expect("write");
    assert!(sniff_text(&script));
    assert!(!sniff_text(&blob));
    assert!(!sniff_text(&dir.path().join("missing")));
}

#[test]
fn the_language_follows_the_extension() {
    assert_eq!(language_info(Path::new("main.rs")).as_deref(), Some("rs"));
    assert_eq!(language_info(Path::new("x.h")).as_deref(), Some("c"));
    assert_eq!(
        language_info(Path::new("Makefile")).as_deref(),
        Some("makefile")
    );
    assert_eq!(language_info(Path::new("notes.txt")), None);
}

#[test]
fn the_fence_is_longer_than_any_backtick_run() {
    assert_eq!(fenced("a\n", Some("rs")), "```rs\na\n```\n");
    assert_eq!(fenced("no newline", None), "```\nno newline\n```\n");
    let inner = "````\nfour\n````";
    let out = fenced(inner, None);
    assert!(out.starts_with("`````\n"));
    assert!(out.ends_with("\n`````\n"));
}

fn open(cx: &mut TestAppContext, file: &Path) -> (AnyWindowHandle, Entity<FileViewer>) {
    let file = file.to_path_buf();
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        cx.set_reduce_motion(true);
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(800.0), px(600.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |_, cx| FileViewer::open(file, None, cx))
            .expect("test window opens")
    })
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

#[gpui::test]
fn a_code_file_shows_as_one_highlighted_code_block(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() {\n    println!(\"```\");\n}\n").expect("write");
    let (window, viewer) = open(cx, &file);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    viewer.read_with(&vcx, |v, cx| {
        assert!(matches!(
            v.status(),
            ViewerStatus::Text { language: Some(l), .. } if l.as_ref() == "Rust"
        ));
        assert!(v.text_ready(cx));
        let text = v
            .reader()
            .and_then(|r| r.read(cx).plain_text())
            .expect("the text");
        assert!(text.contains("fn main()"), "{text}");
        assert!(text.contains("println!(\"```\");"), "{text}");
    });
}

#[gpui::test]
fn a_binary_file_is_not_shown(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("data.bin");
    std::fs::write(&file, [0u8, 159, 146, 150]).expect("write");
    let (window, viewer) = open(cx, &file);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    viewer.read_with(&vcx, |v, _| {
        assert!(
            matches!(v.status(), ViewerStatus::NotShown(why) if why.contains("does not show")),
            "{:?}",
            v.status()
        );
    });
    assert!(
        vcx.debug_bounds("file-viewer-open-externally").is_some(),
        "the open-with-default-app button"
    );
}

#[gpui::test]
fn an_image_fits_and_zooms(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("big.png");
    image::RgbaImage::from_pixel(1600, 1200, image::Rgba([10, 20, 30, 255]))
        .save(&file)
        .expect("png");
    let (window, viewer) = open(cx, &file);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let scale = vcx.update(|window, _| window.scale_factor());
    viewer.read_with(&vcx, |v, _| {
        let ViewerStatus::Image(shown) = v.status() else {
            panic!("an image: {:?}", v.status());
        };
        assert_eq!((shown.probe.width, shown.probe.height), (1600, 1200));
        assert_eq!(v.zoom(), ImageZoom::Fit);
        // Fit: the whole image in the window's points, never enlarged.
        let z = v.effective_scale(scale);
        assert!(z < 1.0 && z > 0.1, "{z}");
        assert!(1600.0 * z / scale <= 800.0 && 1200.0 * z / scale <= 600.0);
    });
    // The image's box fits the body.
    let body = vcx.debug_bounds("image-viewer-body").expect("the body");
    assert!(f32::from(body.size.width) <= 800.0);

    // 100 %, then one step up and two down.
    vcx.update(|window, cx| {
        viewer.update(cx, |v, cx| {
            v.set_zoom(ImageZoom::Scale(1.0), cx);
            v.zoom_by_step(true, window, cx);
        });
    });
    viewer.read_with(&vcx, |v, _| assert_eq!(v.zoom(), ImageZoom::Scale(1.5)));
    vcx.update(|window, cx| {
        viewer.update(cx, |v, cx| {
            v.zoom_by_step(false, window, cx);
            v.zoom_by_step(false, window, cx);
        });
    });
    viewer.read_with(&vcx, |v, _| assert_eq!(v.zoom(), ImageZoom::Scale(0.75)));
    // Keys: 0 fits again.
    vcx.update(|window, cx| {
        viewer.update(cx, |v, cx| v.focus.focus(window, cx));
    });
    draw(&mut vcx);
    vcx.simulate_keystrokes("0");
    viewer.read_with(&vcx, |v, _| assert_eq!(v.zoom(), ImageZoom::Fit));
    vcx.simulate_keystrokes("1");
    viewer.read_with(&vcx, |v, _| assert_eq!(v.zoom(), ImageZoom::Scale(1.0)));
}

#[gpui::test]
fn an_image_that_does_not_decode_says_why(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("broken.png");
    std::fs::write(&file, b"not a png at all").expect("write");
    let (window, viewer) = open(cx, &file);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    viewer.read_with(&vcx, |v, _| {
        assert!(
            matches!(v.status(), ViewerStatus::NotShown(why) if why.starts_with("The image cannot be shown")),
            "{:?}",
            v.status()
        );
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

/// The image viewer for assistive technology: a named group with a
/// toolbar of named buttons (Fit and 100 % say whether they are on), the
/// zoom as a polite status, the picture as an image named by the file;
/// Tab reaches every button and Space presses the focused one.
#[gpui::test]
fn the_image_viewer_names_its_controls_and_the_keyboard_reaches_them(cx: &mut TestAppContext) {
    use crate::a11y::testing::{focused, named, with_role};
    use crate::a11y::{Live, Role, Toggled};

    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("photo.png");
    image::RgbaImage::from_pixel(1600, 1200, image::Rgba([10, 20, 30, 255]))
        .save(&file)
        .expect("png");
    let (window, viewer) = open(cx, &file);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);

    assert_eq!(
        named(&mut vcx, "Image viewer: photo.png").role,
        Some(Role::Group)
    );
    assert_eq!(named(&mut vcx, "Image").role, Some(Role::Toolbar));
    let picture = named(&mut vcx, "photo.png");
    assert_eq!(picture.role, Some(Role::Image));
    assert_eq!(
        picture.description.as_deref(),
        Some("PNG, 1600 by 1200 pixels")
    );
    let zoom = named(&mut vcx, "Zoom");
    assert_eq!(zoom.role, Some(Role::Status));
    assert_eq!(zoom.live, Some(Live::Polite));
    assert!(zoom.value.as_deref().is_some_and(|v| v.ends_with(" %")));
    assert_eq!(named(&mut vcx, "Fit").toggled, Some(Toggled::True));
    assert_eq!(named(&mut vcx, "Actual size").toggled, Some(Toggled::False));
    let buttons: Vec<String> = with_role(&mut vcx, Role::Button)
        .into_iter()
        .inspect(|b| assert!(b.focusable && b.clickable, "{b:?}"))
        .filter_map(|b| b.label)
        .collect();
    assert_eq!(buttons, ["Fit", "Actual size", "Zoom out", "Zoom in"]);

    // From the viewer (as a click on it leaves it), Tab reaches the
    // buttons in order; Space presses 100 %.
    vcx.update(|window, cx| {
        viewer.update(cx, |v, cx| v.focus.focus(window, cx));
    });
    draw(&mut vcx);
    let mut seen = Vec::new();
    for _ in 0..8 {
        vcx.simulate_keystrokes("tab");
        draw(&mut vcx);
        if let Some(Some(name)) = focused(&mut vcx) {
            if seen.contains(&name) {
                break;
            }
            seen.push(name);
        }
    }
    assert_eq!(seen, buttons);
    while focused(&mut vcx).flatten().as_deref() != Some("Actual size") {
        vcx.simulate_keystrokes("tab");
        draw(&mut vcx);
    }
    press(&mut vcx, "space");
    viewer.read_with(&vcx, |v, _| assert_eq!(v.zoom(), ImageZoom::Scale(1.0)));
    assert_eq!(named(&mut vcx, "Actual size").toggled, Some(Toggled::True));
}

/// A file the viewer does not show says why in a status, with a named
/// button the keyboard reaches.
#[gpui::test]
fn a_file_not_shown_is_announced(cx: &mut TestAppContext) {
    use crate::a11y::testing::{named, with_role};
    use crate::a11y::{Live, Role};

    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("data.bin");
    std::fs::write(&file, [0u8, 159, 146, 150]).expect("write");
    let (window, _viewer) = open(cx, &file);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    assert_eq!(named(&mut vcx, "data.bin").role, Some(Role::Group));
    let status = with_role(&mut vcx, Role::Status);
    assert_eq!(status.len(), 1);
    assert!(
        status[0]
            .label
            .as_deref()
            .is_some_and(|l| l.contains("does not show"))
    );
    assert_eq!(status[0].live, Some(Live::Polite));
    let open = named(&mut vcx, "Open with default app");
    assert_eq!(open.role, Some(Role::Button));
    assert!(open.focusable);
}
