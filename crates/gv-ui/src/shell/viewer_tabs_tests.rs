// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Files that are not notes in tabs, in headless GPUI windows: text and
//! code files and images in the built-in viewer, sniffing files of unknown
//! kinds, and links to them.

use std::path::Path;
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_model::LinkKind;

use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::file_viewer::{FileViewer, ViewerStatus};
use crate::files::notice::Topic;
use crate::reader::Disposition;
use crate::reader::link_info::LinkAt;
use crate::{AppSettings, ThemeChoice};

fn temp(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, bytes) in files {
        std::fs::write(dir.path().join(name), bytes).expect("write");
    }
    dir
}

fn open(cx: &mut TestAppContext, file: &Path) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    })
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn viewer(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Entity<FileViewer> {
    workspace
        .read_with(vcx, |ws, _| ws.active_tab().viewer().cloned())
        .expect("the active tab has a viewer")
}

fn file_of(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> String {
    workspace.read_with(vcx, |ws, _| {
        ws.active_tab()
            .path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// Waits (in real time: the sniff and the load run on other threads)
/// until `done` holds.
fn until(
    vcx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(vcx) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        draw(vcx);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn png(path: &Path) {
    image::RgbaImage::from_pixel(40, 30, image::Rgba([200, 10, 10, 255]))
        .save(path)
        .expect("png");
}

#[gpui::test]
fn a_code_file_opens_in_the_text_viewer_read_only(cx: &mut TestAppContext) {
    let dir = temp(&[("config.json", b"{\"a\": [1, 2]}\n")]);
    let (window, workspace) = open(cx, &dir.path().join("config.json"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let viewer = viewer(&workspace, &mut vcx);
    until(&mut vcx, "the text", |vcx| {
        viewer.read_with(vcx, |v, cx| v.text_ready(cx))
    });
    viewer.read_with(&vcx, |v, cx| {
        assert!(matches!(
            v.status(),
            ViewerStatus::Text {
                language: Some(_),
                ..
            }
        ));
        let text = v
            .reader()
            .and_then(|r| r.read(cx).plain_text())
            .expect("text");
        assert!(text.contains("\"a\": [1, 2]"), "{text}");
    });
    workspace.read_with(&vcx, |ws, cx| {
        assert!(ws.active_tab().reader().is_none());
        assert!(!ws.mode_available(ViewMode::Edit, cx));
        assert!(!ws.mode_available(ViewMode::Split, cx));
    });
    assert!(vcx.debug_bounds("file-viewer").is_some());
    // The keyboard goes to the viewer's Reading view.
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| ws.focus_document(window, cx));
        assert!(viewer.read(cx).focus_handle(cx).is_focused(window));
    });

    // A change on disk is read again.
    std::fs::write(dir.path().join("config.json"), "{\"b\": true}\n").expect("write");
    vcx.update(|_, cx| workspace.update(cx, |ws, cx| ws.reload_viewer(ws.active, cx)));
    until(&mut vcx, "the new text", |vcx| {
        viewer.read_with(vcx, |v, cx| {
            v.reader()
                .and_then(|r| r.read(cx).plain_text())
                .is_some_and(|t| t.contains("\"b\": true"))
        })
    });
}

#[gpui::test]
fn an_image_link_opens_the_image_viewer_and_back_returns(cx: &mut TestAppContext) {
    let dir = temp(&[("note.md", b"# Note\n\n[pic](pic.png)\n")]);
    png(&dir.path().join("pic.png"));
    let note = dir.path().join("note.md");
    let (window, workspace) = open(cx, &note);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| {
            let link = LinkAt {
                file: note.clone(),
                kind: LinkKind::Inline,
                image: false,
                destination: "pic.png".into(),
                range: None,
                construct: None,
                profile: gv_vault::ProfileId::CommonMark,
            };
            ws.follow_link_at(ws.active, link, Disposition::InPlace, false, window, cx);
        });
    });
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "pic.png");
    let viewer = viewer(&workspace, &mut vcx);
    until(&mut vcx, "the image", |vcx| {
        viewer.read_with(vcx, |v, _| matches!(v.status(), ViewerStatus::Image(_)))
    });
    assert!(vcx.debug_bounds("image-viewer-body").is_some());
    // No question about the default app.
    let topic = workspace.read_with(&vcx, |ws, cx| ws.active_notice(cx).map(|(n, _)| n.topic));
    assert_eq!(topic, None);

    vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.go_back(window, cx)));
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "note.md");
    workspace.read_with(&vcx, |ws, _| assert!(ws.active_tab().reader().is_some()));
    assert!(vcx.debug_bounds("file-viewer").is_none());
}

#[gpui::test]
fn files_of_unknown_kinds_are_sniffed(cx: &mut TestAppContext) {
    let dir = temp(&[
        ("note.md", b"# Note\n"),
        ("notes.weird", b"plain words\n"),
        ("blob.weird", &[0, 1, 2, 3]),
        ("archive.zip", b"PK\x03\x04"),
    ]);
    let (window, workspace) = open(cx, &dir.path().join("note.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);

    // Text: a tab with the text viewer.
    let text = dir.path().join("notes.weird");
    vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.open_any_file(text, window, cx)));
    until(&mut vcx, "the text tab", |vcx| {
        file_of(&workspace, vcx) == "notes.weird"
    });
    let viewer = viewer(&workspace, &mut vcx);
    until(&mut vcx, "the text", |vcx| {
        viewer.read_with(vcx, |v, cx| v.text_ready(cx))
    });

    // Binary: the question about the default app, no tab.
    let tabs = workspace.read_with(&vcx, |ws, _| ws.tabs.len());
    let blob = dir.path().join("blob.weird");
    vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.open_any_file(blob, window, cx)));
    until(&mut vcx, "the question", |vcx| {
        workspace.read_with(vcx, |ws, cx| {
            ws.active_notice(cx)
                .is_some_and(|(n, _)| n.topic == Topic::Confirm)
        })
    });
    assert_eq!(workspace.read_with(&vcx, |ws, _| ws.tabs.len()), tabs);
    vcx.update(|_, cx| workspace.update(cx, |ws, _| ws.tabs[ws.active].notice = None));

    // A known binary kind is asked about at once.
    let zip = dir.path().join("archive.zip");
    vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.open_any_file(zip, window, cx)));
    let topic = workspace.read_with(&vcx, |ws, cx| ws.active_notice(cx).map(|(n, _)| n.topic));
    assert_eq!(topic, Some(Topic::Confirm));
}

#[gpui::test]
fn the_viewer_offers_the_default_app_for_what_it_cannot_show(cx: &mut TestAppContext) {
    let dir = temp(&[("broken.png", b"not an image")]);
    let (window, workspace) = open(cx, &dir.path().join("broken.png"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let viewer = viewer(&workspace, &mut vcx);
    until(&mut vcx, "the message", |vcx| {
        viewer.read_with(vcx, |v, _| matches!(v.status(), ViewerStatus::NotShown(_)))
    });
    let path = dir.path().join("broken.png");
    viewer.update(&mut vcx, |_, cx| {
        cx.emit(crate::file_viewer::FileViewerEvent::OpenExternally(path));
    });
    draw(&mut vcx);
    let topic = workspace.read_with(&vcx, |ws, cx| ws.active_notice(cx).map(|(n, _)| n.topic));
    assert_eq!(topic, Some(Topic::Confirm));
}
