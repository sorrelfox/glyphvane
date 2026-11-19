// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Split view in headless GPUI windows (GPUI's no-op text system: 0.6
//! em per character): the two panes and the divider, the preview following
//! the editor's text, scroll sync both ways without oscillation, the caret
//! band and mode switches. The timing run with the platform's real text
//! system is `#[ignore]`d (`cargo test -p gv-ui --release split -- --ignored
//! --nocapture`).

use std::path::Path;
use std::sync::Arc;

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollDelta, ScrollEvent, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_render_tree::StyleTable;

use super::sync::{self, Preview};
use super::{Side, SplitView};
use crate::chrome::WindowChrome;
use crate::editor::{EditorLayout, EditorView};
use crate::metrics::HAIRLINE;
use crate::metrics::size::split;
use crate::reader::{Disposition, LinkRequest, Reader, ReaderEvent, ScrollTarget};
use crate::shell::{OpenTarget, ViewMode, Workspace};
use crate::{AppSettings, ThemeChoice};

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 800.0;

fn temp(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    dir
}

fn open(cx: &mut TestAppContext, file: &Path) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(HEIGHT)));
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
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

/// Frames until the preview has caught up and the follower has moved.
fn settle(vcx: &mut VisualTestContext) {
    for _ in 0..5 {
        draw(vcx);
    }
}

/// `count` paragraphs, a code block every 25, and a last paragraph
/// without a line break after it.
fn long_doc(count: usize) -> String {
    let mut doc = String::new();
    for i in 0..count {
        doc.push_str(&format!("Paragraph {i} with some words.\n\n"));
        if i % 25 == 24 {
            doc.push_str("```\nline one\nline two\nline three\n```\n\n");
        }
    }
    doc.push_str("Last paragraph.");
    doc
}

struct Fixture {
    _dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

impl Fixture {
    fn new(cx: &mut TestAppContext, text: &str) -> Self {
        let dir = temp(&[("note.md", text)]);
        let (window, workspace) = open(cx, &dir.path().join("note.md"));
        let mut vcx = VisualTestContext::from_window(window, cx);
        draw(&mut vcx);
        draw(&mut vcx);
        Self {
            _dir: dir,
            workspace,
            vcx,
        }
    }

    fn mode(&mut self, mode: ViewMode) {
        self.workspace
            .update(&mut self.vcx, |ws, cx| ws.show_mode(mode, cx));
        settle(&mut self.vcx);
    }

    fn split(&mut self) -> Entity<SplitView> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().split().cloned())
            .expect("the tab has a Split view")
    }

    fn editor(&mut self) -> Entity<EditorView> {
        self.split().read_with(&self.vcx, |s, _| s.editor().clone())
    }

    fn preview(&mut self) -> Entity<Reader> {
        self.split().read_with(&self.vcx, |s, _| s.reader().clone())
    }

    fn reader(&mut self) -> Entity<Reader> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().reader().cloned())
            .expect("the tab has a Reading view")
    }

    fn panels(&mut self) -> (bool, bool) {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.panels_open())
    }

    /// The editor's top and the preview's top, both as fractional source
    /// lines.
    fn tops(&mut self) -> (f64, f64) {
        let editor = self.editor();
        let preview = self.preview();
        let e = editor
            .read_with(&self.vcx, |e, _| e.top_position())
            .expect("editor top");
        let p = preview.read_with(&self.vcx, |r, _| {
            let (tree, text) = r.live_doc().expect("following");
            let (ix, spot) = r.top_spot().expect("preview top");
            sync::to_editor(
                &Preview {
                    tree: &tree,
                    text: &text,
                },
                ix,
                spot,
            )
        });
        (e, p)
    }

    fn wheel(&mut self, x: f32, dy: f32) {
        self.vcx.simulate_event(ScrollEvent {
            position: point(px(x), px(HEIGHT / 2.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
            ..ScrollEvent::default()
        });
        draw(&mut self.vcx);
    }

    /// The editor pane's width and the preview pane's left edge.
    fn panes(&mut self) -> (f32, f32) {
        let editor = self.editor();
        let columns = editor
            .update_in(&mut self.vcx, |e, window, _| e.columns(None, window))
            .expect("laid out");
        let preview = self.preview();
        let (bounds, _) = preview.read_with(&self.vcx, |r, _| r.frame_geometry());
        (
            f32::from(columns.bounds.size.width),
            f32::from(bounds.left()),
        )
    }

    fn press(&mut self, x: f32, clicks: usize) {
        self.vcx.simulate_event(MouseDownEvent {
            position: point(px(x), px(HEIGHT / 2.0)),
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: clicks,
            first_mouse: false,
        });
        self.vcx.simulate_event(MouseUpEvent {
            position: point(px(x), px(HEIGHT / 2.0)),
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: clicks,
        });
        settle(&mut self.vcx);
    }

    /// Drags from `from` to `to` (window x), drawing between the moves.
    fn drag(&mut self, from: f32, to: f32) {
        let y = px(HEIGHT / 2.0);
        self.vcx.simulate_event(MouseMoveEvent {
            position: point(px(from), y),
            modifiers: Modifiers::default(),
            pressed_button: None,
        });
        self.vcx.simulate_event(MouseDownEvent {
            position: point(px(from), y),
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        draw(&mut self.vcx);
        for step in 1..=4 {
            let x = from + (to - from) * step as f32 / 4.0;
            self.vcx.simulate_event(MouseMoveEvent {
                position: point(px(x), y),
                modifiers: Modifiers::default(),
                pressed_button: Some(MouseButton::Left),
            });
            draw(&mut self.vcx);
        }
        self.vcx.simulate_event(MouseUpEvent {
            position: point(px(to), y),
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 1,
        });
        settle(&mut self.vcx);
    }

    fn preview_text(&mut self) -> String {
        let preview = self.preview();
        preview
            .read_with(&self.vcx, |r, _| r.plain_text())
            .unwrap_or_default()
    }
}

#[gpui::test]
fn split_shows_both_panes_with_a_divider(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, "# Title\n\nFirst paragraph.\n\n- an item\n");
    f.vcx.simulate_keystrokes("secondary-alt-e");
    settle(&mut f.vcx);
    let mode = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().mode());
    assert_eq!(mode, ViewMode::Split);
    assert_eq!(f.panels(), (false, false), "the panels are not touched");

    // The tab's own Source view, at the pane's edge, with the keyboard.
    let editor = f.editor();
    let tab_editor = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().editor().cloned());
    assert_eq!(tab_editor.as_ref(), Some(&editor));
    assert_eq!(
        editor.read_with(&f.vcx, |e, _| e.layout()),
        EditorLayout::Pane
    );
    let focused = f
        .vcx
        .update(|window, cx| editor.read(cx).focus_handle(cx).is_focused(window));
    assert!(focused, "the editor has the keyboard");

    // Two panes of equal width (to the pixel the window's odd width
    // leaves) with a 1 px line between them.
    let columns = editor
        .update_in(&mut f.vcx, |e, window, _| e.columns(None, window))
        .expect("laid out");
    let preview = f.preview();
    let (bounds, (column_x, column_w)) = preview.read_with(&f.vcx, |r, _| r.frame_geometry());
    let left = columns.bounds;
    assert_eq!(f32::from(left.left()), 0.0);
    assert_eq!(f32::from(bounds.left() - left.right()), HAIRLINE);
    assert_eq!(f32::from(bounds.right()), WIDTH);
    let (a, b) = (f32::from(left.size.width), f32::from(bounds.size.width));
    assert!((a - b).abs() <= 1.0, "{a} and {b}");

    // The editor starts at the pane's left edge and wraps at its width.
    assert_eq!(columns.text_x, split::GUTTER + split::TEXT_PAD_LEFT);
    assert_eq!(columns.gutter_left(), 0.0);
    let wrap = editor.read_with(&f.vcx, |e, _| e.editor().and_then(|e| e.wrap_width()));
    assert_eq!(wrap, Some(columns.text_w));
    assert!(columns.text_right() < a, "clear of the scrollbar");

    // The preview: padding 20 28 0 and the split type scale.
    assert_eq!(column_x, f32::from(bounds.left()) + split::PREVIEW_PAD_X);
    assert_eq!(column_w, (b - 2.0 * split::PREVIEW_PAD_X).floor());
    let first = preview.read_with(&f.vcx, |r, _| r.painted_items())[0];
    assert_eq!(first, (0, split::PREVIEW_PAD_TOP));
    assert_eq!(
        preview.read_with(&f.vcx, |r, _| r.styles()),
        Some(StyleTable::split())
    );
    assert!(f.preview_text().contains("First paragraph."));
    // The tab's Reading view keeps the Preview type.
    let reader = f.reader();
    assert_eq!(
        reader.read_with(&f.vcx, |r, _| r.styles()),
        Some(StyleTable::preview())
    );
}

#[gpui::test]
fn dragging_the_divider_resizes_the_panes(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, &long_doc(300));
    f.mode(ViewMode::Split);
    let split = f.split();
    let (left, line) = f.panes();
    let caret = f.editor().read_with(&f.vcx, |e, _| e.caret());

    // Grabbed 2 px right of the line and moved 240 px left: the line moves
    // by as much, and the press did not reach the panes.
    f.drag(line + 2.0, line + 2.0 - 240.0);
    assert!(!split.read_with(&f.vcx, |s, _| s.is_dragging_divider()));
    let (left2, line2) = f.panes();
    assert!((left2 - (left - 240.0)).abs() <= 1.0, "{left} -> {left2}");
    assert_eq!(line2 - left2, HAIRLINE);
    assert_eq!(f.editor().read_with(&f.vcx, |e, _| e.caret()), caret);
    let ratio = split.read_with(&f.vcx, |s, _| s.ratio());
    assert!((ratio - left2 / WIDTH).abs() < 0.002, "{ratio}");

    // The editor rewraps at its new width.
    let editor = f.editor();
    let columns = editor
        .update_in(&mut f.vcx, |e, window, _| e.columns(None, window))
        .expect("laid out");
    let wrap = editor.read_with(&f.vcx, |e, _| e.editor().and_then(|e| e.wrap_width()));
    assert_eq!(wrap, Some(columns.text_w));

    // Scroll sync still holds in the resized panes.
    for _ in 0..6 {
        f.wheel(100.0, -250.0);
    }
    settle(&mut f.vcx);
    let (e, p) = f.tops();
    assert!(e > 20.0, "the editor scrolled: {e}");
    assert!((e - p).abs() < 0.01, "{e} vs {p}");

    // Away from the line a press is the panes' own.
    let ratio = split.read_with(&f.vcx, |s, _| s.ratio());
    f.drag(line2 + 40.0, line2 + 200.0);
    assert_eq!(split.read_with(&f.vcx, |s, _| s.ratio()), ratio);
}

#[gpui::test]
fn the_divider_keeps_both_panes_usable(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, "# Title\n\nText.\n");
    f.mode(ViewMode::Split);
    let (_, line) = f.panes();

    // Far left and far right: each pane keeps its minimum width.
    f.drag(line, 10.0);
    let (left, _) = f.panes();
    assert_eq!(left, split::MIN_PANE);
    let (_, line) = f.panes();
    f.drag(line, WIDTH - 5.0);
    let (left, line) = f.panes();
    assert_eq!(WIDTH - line, split::MIN_PANE);
    assert_eq!(line - left, HAIRLINE);
}

#[gpui::test]
fn double_clicking_the_divider_centres_it(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, "# Title\n\nText.\n");
    f.mode(ViewMode::Split);
    let split = f.split();
    let (_, line) = f.panes();
    f.drag(line, 400.0);
    let (_, line) = f.panes();
    assert!((line - 401.0).abs() <= 1.0, "{line}");

    f.press(line, 1);
    f.press(line, 2);
    assert_eq!(split.read_with(&f.vcx, |s, _| s.ratio()), 0.5);
    let (left, line) = f.panes();
    assert!((left - WIDTH / 2.0).abs() <= 1.0, "{left}");
    assert_eq!(line - left, HAIRLINE);

    // The split is the tab's: it outlasts a trip to Source and back.
    f.drag(line, 500.0);
    let ratio = split.read_with(&f.vcx, |s, _| s.ratio());
    f.mode(ViewMode::Edit);
    f.mode(ViewMode::Split);
    assert_eq!(f.split().read_with(&f.vcx, |s, _| s.ratio()), ratio);
}

#[gpui::test]
fn typing_in_the_editor_updates_the_preview(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, &long_doc(40));
    f.mode(ViewMode::Split);
    let preview = f.preview();
    let ((incremental, full), _) = preview.read_with(&f.vcx, |r, _| r.follow_stats());
    assert_eq!(
        (incremental, full),
        (0, 1),
        "built once from the editor's tree"
    );

    f.vcx.simulate_keystrokes("secondary-end");
    f.vcx.simulate_input("\n\nAdded *here*.");
    settle(&mut f.vcx);
    assert!(
        f.preview_text().contains("Added here."),
        "{}",
        f.preview_text()
    );
    f.vcx.simulate_input(" More.");
    settle(&mut f.vcx);
    assert!(f.preview_text().contains("Added here. More."));
    let ((incremental, full), _) = preview.read_with(&f.vcx, |r, _| r.follow_stats());
    assert_eq!(full, 1, "updates are incremental");
    assert!(incremental >= 2, "{incremental}");

    // An edit at the top shows too, and the preview's anchor holds.
    f.vcx.simulate_keystrokes("secondary-home");
    f.vcx.simulate_input("# Heading\n\n");
    settle(&mut f.vcx);
    let text = f.preview_text();
    assert!(text.starts_with("Heading"), "{text}");
    // Not saved: the preview shows the editor's text, not the file.
    let on_disk =
        std::fs::read_to_string(f._dir.path().join("note.md")).expect("the file is there");
    assert!(!on_disk.contains("Added"));
}

#[gpui::test]
fn scrolling_either_pane_scrolls_the_other_without_oscillation(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, &long_doc(300));
    f.mode(ViewMode::Split);
    let split = f.split();

    // The wheel over the editor: the preview follows.
    for _ in 0..8 {
        f.wheel(300.0, -250.0);
    }
    settle(&mut f.vcx);
    assert_eq!(split.read_with(&f.vcx, |s, _| s.leader()), Side::Editor);
    let (editor, preview) = f.tops();
    assert!(editor > 50.0, "the editor scrolled: {editor}");
    assert!((editor - preview).abs() < 0.01, "{editor} vs {preview}");
    let still = |f: &mut Fixture| {
        let before = f.tops();
        for _ in 0..5 {
            draw(&mut f.vcx);
        }
        assert_eq!(f.tops(), before, "the panes stay where they are");
    };
    still(&mut f);

    // The wheel over the preview: the editor follows.
    for _ in 0..6 {
        f.wheel(WIDTH - 300.0, -300.0);
    }
    settle(&mut f.vcx);
    assert_eq!(split.read_with(&f.vcx, |s, _| s.leader()), Side::Preview);
    let (editor2, preview2) = f.tops();
    assert!(preview2 > editor + 20.0, "the preview scrolled: {preview2}");
    assert!((editor2 - preview2).abs() < 0.01, "{editor2} vs {preview2}");
    still(&mut f);

    // And back up over the editor.
    for _ in 0..4 {
        f.wheel(300.0, 400.0);
    }
    settle(&mut f.vcx);
    assert_eq!(split.read_with(&f.vcx, |s, _| s.leader()), Side::Editor);
    let (editor3, preview3) = f.tops();
    assert!(editor3 < editor2);
    assert!((editor3 - preview3).abs() < 0.01, "{editor3} vs {preview3}");
    still(&mut f);
}

#[gpui::test]
fn the_caret_band_follows_the_caret(cx: &mut TestAppContext) {
    let doc = long_doc(300);
    let mut f = Fixture::new(cx, &doc);
    f.mode(ViewMode::Split);
    let preview = f.preview();

    // Line 10 is paragraph 5.
    f.vcx
        .simulate_keystrokes("down down down down down down down down down down");
    settle(&mut f.vcx);
    let caret = doc.find("Paragraph 5 ").expect("paragraph 5");
    assert_eq!(preview.read_with(&f.vcx, |r, _| r.band()), Some(caret));
    let painted: Vec<usize> = preview
        .read_with(&f.vcx, |r, _| r.painted_items())
        .into_iter()
        .map(|(start, _)| start)
        .collect();
    assert!(painted.contains(&caret), "the caret's block is on screen");

    // The caret goes to the end: the editor scrolls, and the preview shows
    // the last block whole.
    f.vcx.simulate_keystrokes("secondary-end");
    settle(&mut f.vcx);
    assert_eq!(preview.read_with(&f.vcx, |r, _| r.band()), Some(doc.len()));
    let last = doc.rfind("Last paragraph.").expect("the last block");
    let (items, height) =
        preview.read_with(&f.vcx, |r, _| (r.painted_items(), r.viewport_height()));
    let (_, top) = items
        .iter()
        .copied()
        .find(|(start, _)| *start == last)
        .expect("the last block is painted");
    assert!(top >= 0.0 && top < height, "{top} in {height}");

    // Typing moves the band with the edit once the preview shows it.
    f.vcx.simulate_input(" Typed.");
    settle(&mut f.vcx);
    assert_eq!(
        preview.read_with(&f.vcx, |r, _| r.band()),
        Some(doc.len() + " Typed.".len())
    );
}

#[gpui::test]
fn mode_switches_keep_the_top_line(cx: &mut TestAppContext) {
    let doc = long_doc(200);
    let mut f = Fixture::new(cx, &doc);
    let target = doc.find("Paragraph 100 ").expect("paragraph 100");
    let reader = f.reader();
    reader.update(&mut f.vcx, |r, cx| {
        r.scroll_to(ScrollTarget::Offset(target), cx);
    });
    settle(&mut f.vcx);

    // Preview → Split: both panes start at the paragraph.
    f.vcx.simulate_keystrokes("secondary-alt-e");
    settle(&mut f.vcx);
    let editor = f.editor();
    let preview = f.preview();
    assert_eq!(
        editor.read_with(&f.vcx, |e, _| e.top_offset()),
        Some(target)
    );
    assert_eq!(
        preview.read_with(&f.vcx, |r, _| r.position()),
        Some(ScrollTarget::Offset(target))
    );
    // Split → Edit (toggling split) and back.
    f.vcx.simulate_keystrokes("secondary-alt-e");
    settle(&mut f.vcx);
    assert_eq!(
        editor.read_with(&f.vcx, |e, _| (e.layout(), e.top_offset())),
        (EditorLayout::Column, Some(target))
    );
    f.mode(ViewMode::Split);
    assert_eq!(
        editor.read_with(&f.vcx, |e, _| (e.layout(), e.top_offset())),
        (EditorLayout::Pane, Some(target))
    );
    assert_eq!(
        preview.read_with(&f.vcx, |r, _| r.position()),
        Some(ScrollTarget::Offset(target))
    );
    // Split → Preview.
    f.mode(ViewMode::Preview);
    assert_eq!(
        reader.read_with(&f.vcx, |r, _| r.position()),
        Some(ScrollTarget::Offset(target))
    );
    // Scrolled in the preview pane, Split → Preview keeps the preview's
    // top block.
    f.mode(ViewMode::Split);
    for _ in 0..3 {
        f.wheel(WIDTH - 300.0, -200.0);
    }
    settle(&mut f.vcx);
    let top = match preview.read_with(&f.vcx, |r, _| r.position()) {
        Some(ScrollTarget::Offset(offset)) => offset,
        other => panic!("{other:?}"),
    };
    assert!(top > target);
    f.mode(ViewMode::Preview);
    assert_eq!(
        reader.read_with(&f.vcx, |r, _| r.position()),
        Some(ScrollTarget::Offset(top))
    );
    assert_eq!(f.panels(), (false, false), "the panels are not touched");
}

#[gpui::test]
fn links_in_the_preview_pane_are_followed(cx: &mut TestAppContext) {
    let mut doc = String::from("[To the end](#the-end) and [other](other.md)\n\n");
    doc.push_str(&long_doc(150));
    doc.push_str("\n\n## The end\n\nDone.\n");
    let other = format!(
        "# Other\n\nElsewhere.\n\n{}\n\n## Later\n\nThere.\n",
        long_doc(80)
    );
    let dir = temp(&[("note.md", &doc), ("other.md", &other)]);
    let (window, workspace) = open(cx, &dir.path().join("note.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let mut f = Fixture {
        _dir: dir,
        workspace,
        vcx,
    };
    f.mode(ViewMode::Split);
    let follow = |f: &mut Fixture, destination: &str| {
        let preview = f.preview();
        let request = LinkRequest {
            destination: Arc::from(destination),
            kind: gv_model::LinkKind::Inline,
            disposition: Disposition::InPlace,
            range: None,
            from: None,
        };
        preview.update(&mut f.vcx, |_, cx| {
            cx.emit(ReaderEvent::FollowLink(request))
        });
        settle(&mut f.vcx);
    };

    // A heading in the same document: the preview goes there and the
    // editor follows.
    follow(&mut f, "#the-end");
    let split = f.split();
    assert_eq!(split.read_with(&f.vcx, |s, _| s.leader()), Side::Preview);
    let heading = doc.find("## The end").expect("heading");
    let preview = f.preview();
    let painted: Vec<usize> = preview
        .read_with(&f.vcx, |r, _| r.painted_items())
        .into_iter()
        .map(|(start, _)| start)
        .collect();
    assert!(painted.contains(&heading), "{painted:?}");
    let (editor, top) = f.tops();
    assert!((editor - top).abs() < 0.01, "{editor} vs {top}");
    assert!(editor > 250.0, "the editor followed: {editor}");

    // Another file, at a heading: the tab shows it, still in Split, with
    // both panes there.
    follow(&mut f, "other.md#later");
    let (path, mode) = f.workspace.read_with(&f.vcx, |ws, _| {
        let tab = ws.active_tab();
        (tab.path().map(Path::to_path_buf), tab.mode())
    });
    assert!(path.is_some_and(|p| p.ends_with("other.md")));
    assert_eq!(mode, ViewMode::Split);
    let text = f.preview_text();
    assert!(text.contains("Elsewhere."), "{text}");
    let (editor, top) = f.tops();
    assert!((editor - top).abs() < 0.01, "{editor} vs {top}");
    assert!(editor > 100.0, "at the heading: {editor}");

    // Back: the first file, where its panes were.
    f.workspace
        .update_in(&mut f.vcx, |ws, window, cx| ws.go_back(window, cx));
    settle(&mut f.vcx);
    let painted: Vec<usize> = f
        .preview()
        .read_with(&f.vcx, |r, _| r.painted_items())
        .into_iter()
        .map(|(start, _)| start)
        .collect();
    assert!(painted.contains(&heading), "{painted:?}");
    let (editor, top) = f.tops();
    assert!((editor - top).abs() < 0.01, "{editor} vs {top}");
}

#[gpui::test]
fn an_untitled_document_splits_too(cx: &mut TestAppContext) {
    let mut f = Fixture::new(cx, "# Note\n");
    f.workspace
        .update_in(&mut f.vcx, |ws, window, cx| ws.new_tab(window, cx));
    settle(&mut f.vcx);
    f.mode(ViewMode::Split);
    f.vcx.simulate_input("# Draft\n\nWritten *here*.");
    settle(&mut f.vcx);
    assert_eq!(f.preview_text(), "Draft\nWritten here.");
}

// --- Timing --------------------------------------------------------------------------

mod timing {
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, Instant};

    use gpui::{Keystroke, PlatformTextSystem, TestDispatcher};

    use super::*;

    fn ms(d: Duration) -> f64 {
        d.as_secs_f64() * 1e3
    }

    fn percentiles(samples: &[Duration]) -> String {
        let mut s = samples.to_vec();
        s.sort_unstable();
        let at = |q: f64| ms(s[((s.len() - 1) as f64 * q).round() as usize]);
        format!(
            "p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, max {:.2} ms ({} samples)",
            at(0.5),
            at(0.95),
            at(0.99),
            at(1.0),
            s.len()
        )
    }

    fn platform_text_system() -> Arc<dyn PlatformTextSystem> {
        static TEXT: OnceLock<Arc<dyn PlatformTextSystem>> = OnceLock::new();
        TEXT.get_or_init(|| gpui::platform::current_platform(true).text_system())
            .clone()
    }

    /// About `bytes` of mixed prose, lists, quotes, links and code (as the
    /// editor's timing run).
    fn generated(bytes: usize) -> String {
        let mut s = String::with_capacity(bytes + 256);
        let mut i = 0usize;
        while s.len() < bytes {
            match i % 6 {
                0 => s.push_str(&format!("## Section {i}\n\n")),
                1 => s.push_str(&format!(
                    "Some **strong** and *emphasised* prose with `code` and a [link](notes/n{i}.md), item {i}.\n\n"
                )),
                2 => s.push_str(&format!("- first item {i}\n- second item with [[wiki]]\n\n")),
                3 => s.push_str(&format!("> quoted text {i}\n> more quote\n\n")),
                4 => s.push_str(&format!("```rust\nfn f{i}(x: u32) -> u32 {{\n    x * 2\n}}\n```\n\n")),
                _ => s.push_str(&format!("Plain paragraph number {i} with several words of text in it.\n\n")),
            }
            i += 1;
        }
        s
    }

    /// Typing in the middle of a generated document in Split mode: per
    /// keystroke, the input event to the frame drawn (both panes, the
    /// preview with its last layout), then the preview's update (its build
    /// run on this thread, the frame that shows it and the frame that
    /// syncs it). Then the same keystroke in Edit mode, and what a full
    /// rebuild of the preview would cost instead.
    fn run(name: &str, bytes: usize) {
        let markdown = generated(bytes);
        let dir = temp(&[("doc.md", &markdown)]);
        let dispatcher = TestDispatcher::new(0);
        let mut cx = TestAppContext::build_with_text_system(
            dispatcher.clone(),
            Some("split_timing"),
            platform_text_system(),
        );
        {
            let (window, workspace) = open(&mut cx, &dir.path().join("doc.md"));
            let mut vcx = VisualTestContext::from_window(window, &cx);
            draw(&mut vcx);
            let t = Instant::now();
            workspace.update(&mut vcx, |ws, cx| ws.show_mode(ViewMode::Split, cx));
            settle(&mut vcx);
            let entered = t.elapsed();
            let split = workspace
                .read_with(&vcx, |ws, _| ws.active_tab().split().cloned())
                .expect("split");
            let preview = split.read_with(&vcx, |s, _| s.reader().clone());
            let (_, (seed_model, seed_tree)) = preview.read_with(&vcx, |r, _| r.follow_stats());

            // The caret in the middle of the text, at the end of a plain
            // paragraph, on screen in both panes.
            let middle = markdown.len() / 2;
            let at = markdown[middle..]
                .find("Plain paragraph")
                .map(|k| middle + k)
                .and_then(|k| markdown[k..].find('\n').map(|e| k + e))
                .expect("a plain paragraph");
            let editor = split.read_with(&vcx, |s, _| s.editor().clone());
            editor.update(&mut vcx, |e, cx| e.set_cursor(at, cx));
            settle(&mut vcx);

            let keys = ["a", "b", "c", "space", "x", "y", "z", "space"];
            let (mut typed, mut updates) = (Vec::new(), Vec::new());
            let (mut models, mut trees) = (Vec::new(), Vec::new());
            for k in 0..300 {
                let key = Keystroke::parse(keys[k % keys.len()]).expect("key");
                let t = Instant::now();
                vcx.dispatch_keystroke(window, key);
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                typed.push(t.elapsed());
                let t = Instant::now();
                vcx.run_until_parked();
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                vcx.run_until_parked();
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                updates.push(t.elapsed());
                let (_, (model, tree)) = preview.read_with(&vcx, |r, _| r.follow_stats());
                models.push(model);
                trees.push(tree);
            }
            let ((incremental, full), _) = preview.read_with(&vcx, |r, _| r.follow_stats());
            eprintln!(
                "{name}: Split: typing, input to frame drawn: {}",
                percentiles(&typed[20..])
            );
            eprintln!(
                "{name}: Split: preview update (build on this thread + 2 frames): {}",
                percentiles(&updates[20..])
            );
            eprintln!(
                "{name}: Split: of which model update {}; render tree update {}; {incremental} incremental, {full} from scratch",
                percentiles(&models[20..]),
                percentiles(&trees[20..]),
            );
            eprintln!(
                "{name}: Split entered in {:.1} ms (seed from the editor's tree: model {:.1} ms, render tree {:.1} ms)",
                ms(entered),
                ms(seed_model),
                ms(seed_tree)
            );

            // The same keystrokes in Edit mode, for comparison.
            workspace.update(&mut vcx, |ws, cx| ws.show_mode(ViewMode::Edit, cx));
            settle(&mut vcx);
            let mut edit = Vec::new();
            for k in 0..300 {
                let key = Keystroke::parse(keys[k % keys.len()]).expect("key");
                let t = Instant::now();
                vcx.dispatch_keystroke(window, key);
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                edit.push(t.elapsed());
                vcx.run_until_parked();
            }
            eprintln!(
                "{name}: Edit: typing, input to frame drawn: {}",
                percentiles(&edit[20..])
            );
        }
        // A full rebuild per edit instead (`Reader::open_text`): parse,
        // model and render tree of the whole text.
        let mut full = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            let syntax = gv_syntax::parse_with(&*markdown, gv_syntax::ParseOptions::default());
            let model = gv_model::DocModel::build(&syntax, &*markdown);
            let tree = gv_render_tree::RenderTree::build(
                &syntax,
                &model,
                &*markdown,
                gv_render_tree::RenderOptions::commonmark(),
                gv_render_tree::Sources::default(),
            );
            full.push(t.elapsed());
            drop(tree);
        }
        eprintln!(
            "{name}: a full rebuild instead (parse + model + render tree): {}",
            percentiles(&full)
        );
        cx.run_until_parked();
        cx.update(|cx| cx.quit());
        cx.run_until_parked();
        drop(cx);
        dispatcher.drain_tasks();
    }

    #[test]
    #[ignore = "timing run; see the module docs"]
    fn timing_typing_in_split_in_one_megabyte() {
        run("generated 1 MB", 1 << 20);
    }
}
