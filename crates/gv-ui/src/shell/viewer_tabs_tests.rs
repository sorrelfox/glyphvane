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
        // Text is edited as plain text; there is no Split view.
        assert!(ws.mode_available(ViewMode::Edit, cx));
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
    // An image has no Source view.
    workspace.read_with(&vcx, |ws, cx| {
        assert!(!ws.mode_available(ViewMode::Edit, cx));
        assert!(!ws.mode_available(ViewMode::Split, cx));
    });
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

#[gpui::test]
fn edit_switches_a_text_file_to_the_editor_and_back(cx: &mut TestAppContext) {
    // CRLF line endings and no line ending at the end.
    let original = b"{\r\n  \"a\": 1\r\n}";
    let dir = temp(&[("data.json", original)]);
    let file = dir.path().join("data.json");
    let (window, workspace) = open(cx, &file);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let viewer = viewer(&workspace, &mut vcx);
    until(&mut vcx, "the text", |vcx| {
        viewer.read_with(vcx, |v, cx| v.text_ready(cx))
    });

    // Mod+E: the Source view, in plain-text mode, with the keyboard.
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| ws.focus_document(window, cx));
    });
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    let editor = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("the tab has an editor");
    assert_eq!(
        workspace.read_with(&vcx, |ws, _| ws.active_tab().mode()),
        ViewMode::Edit
    );
    until(&mut vcx, "the editor", |vcx| {
        editor.read_with(vcx, |e, _| {
            *e.status() == crate::editor::EditorStatus::Ready
        })
    });
    assert!(editor.read_with(&vcx, |e, _| e.is_plain() && !e.is_read_only()));
    assert!(vcx.debug_bounds("file-viewer").is_none());
    let focused = vcx.update(|window, cx| editor.read(cx).focus_handle(cx).is_focused(window));
    assert!(focused, "the editor has the keyboard");

    // Edit and save: the rest of the file is kept byte for byte.
    let end_of_a = "{\r\n  \"a\": 1".len();
    editor.update(&mut vcx, |e, cx| e.set_cursor(end_of_a, cx));
    draw(&mut vcx);
    vcx.simulate_keystrokes("enter");
    vcx.simulate_input("\"b\": 2");
    draw(&mut vcx);
    editor.update(&mut vcx, |e, cx| e.save(cx));
    let expected = b"{\r\n  \"a\": 1\r\n  \"b\": 2\r\n}";
    until(&mut vcx, "the save", |_| {
        std::fs::read(&file).is_ok_and(|bytes| bytes == expected)
    });

    // Mod+E again: the viewer shows the edited text.
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    assert_eq!(
        workspace.read_with(&vcx, |ws, _| ws.active_tab().mode()),
        ViewMode::Preview
    );
    assert!(vcx.debug_bounds("file-viewer").is_some());
    until(&mut vcx, "the edited text", |vcx| {
        viewer.read_with(vcx, |v, cx| {
            v.reader()
                .and_then(|r| r.read(cx).plain_text())
                .is_some_and(|t| t.contains("\"b\": 2"))
        })
    });
    // Split stays unavailable; its command does nothing.
    vcx.dispatch_action(crate::keymap::actions::view::ToggleSplit);
    draw(&mut vcx);
    assert_eq!(
        workspace.read_with(&vcx, |ws, _| ws.active_tab().mode()),
        ViewMode::Preview
    );
}

/// Switching a text file between the viewer and the Source view keeps the
/// line at the top, both ways, also after an edit.
#[gpui::test]
fn the_line_at_the_top_stays_between_the_viewer_and_edit(cx: &mut TestAppContext) {
    let text: String = (0..400).map(|i| format!("line {i}\n")).collect();
    let dir = temp(&[("long.txt", text.as_bytes())]);
    let (window, workspace) = open(cx, &dir.path().join("long.txt"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let viewer = viewer(&workspace, &mut vcx);
    until(&mut vcx, "the text", |vcx| {
        viewer.read_with(vcx, |v, cx| v.text_ready(cx))
    });
    let viewer_top = |vcx: &mut VisualTestContext| {
        viewer
            .read_with(vcx, |v, cx| v.text_top(cx))
            .expect("the viewer's top line")
    };
    let near = |what: &str, got: f64, want: f64| {
        assert!((got - want).abs() < 1.5, "{what}: {got}, not {want}");
    };
    // The viewer, scrolled into its second chunk of lines.
    viewer.update(&mut vcx, |v, cx| v.scroll_text_to(120.0, cx));
    draw(&mut vcx);
    near("viewer", viewer_top(&mut vcx), 120.0);

    // Edit: the editor shows the same line at its top.
    workspace.update(&mut vcx, |ws, cx| ws.set_mode(ViewMode::Edit, cx));
    let editor = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("the tab has an editor");
    until(&mut vcx, "the editor", |vcx| {
        editor.read_with(vcx, |e, _| {
            *e.status() == crate::editor::EditorStatus::Ready
        })
    });
    draw(&mut vcx);
    let top = editor
        .read_with(&vcx, |e, _| e.top_position())
        .expect("top");
    near("editor", top, 120.0);

    // Scrolled in the editor and back to Preview: the viewer follows.
    editor.update(&mut vcx, |e, cx| e.scroll_to_line(250, cx));
    draw(&mut vcx);
    workspace.update(&mut vcx, |ws, cx| ws.set_mode(ViewMode::Preview, cx));
    draw(&mut vcx);
    near("viewer after Edit", viewer_top(&mut vcx), 250.0);

    // Preview to Edit again (the editor is laid out): the viewer's line.
    viewer.update(&mut vcx, |v, cx| v.scroll_text_to(42.5, cx));
    draw(&mut vcx);
    workspace.update(&mut vcx, |ws, cx| ws.set_mode(ViewMode::Edit, cx));
    draw(&mut vcx);
    let top = editor
        .read_with(&vcx, |e, _| e.top_position())
        .expect("top");
    near("editor again", top, 42.5);

    // An edit: the viewer is given the new text and still shows the
    // editor's line at the top.
    editor.update(&mut vcx, |e, cx| e.set_cursor(0, cx));
    draw(&mut vcx);
    vcx.simulate_input("new ");
    editor.update(&mut vcx, |e, cx| e.scroll_to_line(300, cx));
    draw(&mut vcx);
    workspace.update(&mut vcx, |ws, cx| ws.set_mode(ViewMode::Preview, cx));
    until(&mut vcx, "the edited text", |vcx| {
        viewer.read_with(vcx, |v, cx| {
            v.reader()
                .and_then(|r| r.read(cx).plain_text())
                .is_some_and(|t| t.contains("new line 0"))
        })
    });
    draw(&mut vcx);
    near("viewer after the edit", viewer_top(&mut vcx), 300.0);
}

/// A viewer tab takes the keyboard as a note's tab does: in a new window
/// and when opened from a note that has it.
#[gpui::test]
fn a_viewer_tab_takes_the_keyboard(cx: &mut TestAppContext) {
    let dir = temp(&[("a.md", b"# A\n"), ("config.json", b"{}\n")]);
    let (window, workspace) = open(cx, &dir.path().join("config.json"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let focused = |workspace: &Entity<Workspace>, vcx: &mut VisualTestContext| {
        let viewer = viewer(workspace, vcx);
        vcx.update(|window, cx| {
            viewer
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        })
    };
    let shown = |workspace: &Entity<Workspace>, vcx: &mut VisualTestContext| {
        let viewer = viewer(workspace, vcx);
        until(vcx, "the text", |vcx| {
            viewer.read_with(vcx, |v, cx| v.text_ready(cx))
        });
    };
    shown(&workspace, &mut vcx);
    assert!(focused(&workspace, &mut vcx), "a new window's viewer");

    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let json = dir.path().join("config.json");
    workspace.update_in(&mut vcx, |ws, window, cx| {
        ws.open_tab(OpenTarget::new(json), window, cx)
    });
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "config.json");
    shown(&workspace, &mut vcx);
    assert!(focused(&workspace, &mut vcx), "a viewer opened from a note");
}

/// A link to a PDF opens the PDF viewer in a tab at the page its
/// `#page=N` names; another such link to the same PDF moves that tab to
/// its page. A PDF has no Source view.
#[gpui::test]
fn a_pdf_link_opens_the_pdf_viewer_at_its_page(cx: &mut TestAppContext) {
    use std::sync::Arc;

    use crate::file_viewer::pdf_tests::FakePdf;

    let dir = temp(&[
        ("note.md", b"# Note\n\n[p](paper.pdf#page=3)\n"),
        ("paper.pdf", b"%PDF-1.7\n%%EOF\n"),
    ]);
    let note = dir.path().join("note.md");
    let (window, workspace) = open(cx, &note);
    let mut vcx = VisualTestContext::from_window(window, cx);
    let engine = Arc::new(FakePdf::pages(&[(612.0, 792.0); 6]));
    vcx.update(|_, cx| {
        crate::export::set_pdf_engine(cx, Some(engine as Arc<dyn crate::export::PdfEngine>));
    });
    draw(&mut vcx);
    let follow = |destination: &str, vcx: &mut VisualTestContext| {
        let link = LinkAt {
            file: note.clone(),
            kind: LinkKind::Inline,
            image: false,
            destination: destination.into(),
            range: None,
            construct: None,
            profile: gv_vault::ProfileId::CommonMark,
        };
        vcx.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.follow_link_at(ws.active, link, Disposition::InPlace, false, window, cx);
            });
        });
        draw(vcx);
    };
    follow("paper.pdf#page=3", &mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "paper.pdf");
    let viewer = viewer(&workspace, &mut vcx);
    until(&mut vcx, "the PDF", |vcx| {
        viewer.read_with(vcx, |v, _| v.pdf().is_some())
    });
    let page = |vcx: &mut VisualTestContext| {
        viewer.read_with(vcx, |v, cx| v.pdf().map(|p| p.read(cx).current_page()))
    };
    until(&mut vcx, "page 3", |vcx| page(vcx) == Some(2));
    workspace.read_with(&vcx, |ws, cx| {
        assert!(!ws.mode_available(ViewMode::Edit, cx));
        assert!(!ws.mode_available(ViewMode::Split, cx));
        assert!(ws.active_notice(cx).is_none(), "no question");
    });
    // The same PDF, another page: the same tab moves there.
    follow("paper.pdf#page=5", &mut vcx);
    until(&mut vcx, "page 5", |vcx| page(vcx) == Some(4));
    assert_eq!(
        workspace.read_with(&vcx, |ws, _| ws.tabs.len()),
        1,
        "the same tab"
    );
}

/// Back and Forward bring a PDF back where it was left (page, offset,
/// zoom), not at its start nor at the page a link named; a jump within
/// the PDF goes back too.
#[gpui::test]
fn back_returns_to_the_pdf_where_it_was_left(cx: &mut TestAppContext) {
    use std::sync::Arc;

    use crate::file_viewer::PdfZoom;
    use crate::file_viewer::pdf_tests::FakePdf;

    let dir = temp(&[
        ("note.md", b"# Note\n\n[p](paper.pdf)\n"),
        ("other.md", b"# Other\n"),
        ("paper.pdf", b"%PDF-1.7\n%%EOF\n"),
        ("second.pdf", b"%PDF-1.7\n%%EOF\n"),
    ]);
    let note = dir.path().join("note.md");
    let (window, workspace) = open(cx, &note);
    let mut vcx = VisualTestContext::from_window(window, cx);
    let engine = Arc::new(FakePdf::pages(&[(612.0, 792.0); 8]));
    vcx.update(|_, cx| {
        crate::export::set_pdf_engine(cx, Some(engine as Arc<dyn crate::export::PdfEngine>));
    });
    draw(&mut vcx);
    let follow = |destination: &str, vcx: &mut VisualTestContext| {
        let link = LinkAt {
            file: note.clone(),
            kind: LinkKind::Inline,
            image: false,
            destination: destination.into(),
            range: None,
            construct: None,
            profile: gv_vault::ProfileId::CommonMark,
        };
        vcx.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.follow_link_at(ws.active, link, Disposition::InPlace, false, window, cx);
            });
        });
        draw(vcx);
    };
    let back = |vcx: &mut VisualTestContext| {
        vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.go_back(window, cx)));
        draw(vcx);
    };
    let forward = |vcx: &mut VisualTestContext| {
        vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.go_forward(window, cx)));
        draw(vcx);
    };
    // The PDF shown now, once read: its page and zoom.
    let shown = |vcx: &mut VisualTestContext| {
        let viewer = workspace.read_with(vcx, |ws, _| ws.active_tab().viewer().cloned())?;
        viewer.read_with(vcx, |v, cx| {
            let pdf = v.pdf()?.read(cx);
            Some((
                v.path().file_name()?.to_string_lossy().into_owned(),
                pdf.current_page(),
                pdf.zoom(),
            ))
        })
    };

    follow("paper.pdf", &mut vcx);
    until(&mut vcx, "the PDF", |vcx| shown(vcx).is_some());
    let pdf = workspace
        .read_with(&vcx, |ws, cx| {
            ws.active_tab()
                .viewer()
                .and_then(|v| v.read(cx).pdf().cloned())
        })
        .expect("the PDF viewer");
    pdf.update(&mut vcx, |p, cx| {
        p.set_zoom(PdfZoom::Scale(1.5), cx);
        p.go_to(4, cx);
    });
    draw(&mut vcx);
    let left = pdf.read_with(&vcx, |p, _| p.place());
    assert_eq!(left.page, 4);

    let at_page_5 = Some(("paper.pdf".to_owned(), 4, PdfZoom::Scale(1.5)));
    follow("other.md", &mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "other.md");
    back(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "paper.pdf");
    until(&mut vcx, "page 5 again", |vcx| shown(vcx) == at_page_5);
    let place = workspace
        .read_with(&vcx, |ws, cx| {
            ws.active_tab()
                .viewer()
                .and_then(|v| v.read(cx).pdf_place(cx))
        })
        .expect("a place");
    assert_eq!(place.page, left.page);
    assert!(
        (place.into.unwrap_or(0.0) - left.into.unwrap_or(0.0)).abs() < 0.5,
        "{place:?} vs {left:?}"
    );

    // Forward to the note and Back again: still page 5.
    forward(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "other.md");
    back(&mut vcx);
    until(&mut vcx, "page 5 once more", |vcx| shown(vcx) == at_page_5);

    // Another PDF in the same tab, then Back: the first one's place.
    follow("second.pdf", &mut vcx);
    until(&mut vcx, "the second PDF", |vcx| {
        shown(vcx).is_some_and(|(name, page, _)| name == "second.pdf" && page == 0)
    });
    back(&mut vcx);
    until(&mut vcx, "the first PDF at page 5", |vcx| {
        shown(vcx) == at_page_5
    });

    // A jump within the PDF (a link to a page): Back returns where it was.
    follow("paper.pdf#page=2", &mut vcx);
    until(&mut vcx, "page 2", |vcx| {
        shown(vcx).is_some_and(|(_, page, _)| page == 1)
    });
    back(&mut vcx);
    until(&mut vcx, "page 5 after the jump", |vcx| {
        shown(vcx) == at_page_5
    });
    forward(&mut vcx);
    until(&mut vcx, "page 2 again", |vcx| {
        shown(vcx).is_some_and(|(_, page, _)| page == 1)
    });
}
