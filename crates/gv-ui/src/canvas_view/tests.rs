// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Canvas view in headless GPUI windows: loading and drawing a fixture
//! canvas, culling, pan and zoom, keyboard navigation, opening cards,
//! problems, reduced motion, the minimap, `view.zoom_to_fit`, what
//! assistive technology is told, PDF first pages, group backgrounds, the
//! problems chip by keyboard, and HTML and live blocks in cards.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gpui::AppContext as _;
use gpui::{
    AnyWindowHandle, Bounds, Entity, Modifiers, MouseButton, Pixels, Point, ScrollDelta,
    ScrollEvent, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_canvas::{FitOptions, ItemId, Transform};
use gv_vault::FragmentKind;

use super::cards::Preview;
use super::{CanvasEvent, CanvasStatus, CanvasView};
use crate::a11y::{Role, testing};
use crate::reader::Disposition;
use crate::{AppSettings, ThemeChoice};

const WIDTH: f32 = 1000.0;
const HEIGHT: f32 = 700.0;

/// A 1×1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0xd, 0xa, 0x1a, 0xa, 0x0, 0x0, 0x0, 0xd, 0x49, 0x48, 0x44, 0x52, 0x0,
    0x0, 0x0, 0x1, 0x0, 0x0, 0x0, 0x1, 0x8, 0x4, 0x0, 0x0, 0x0, 0xb5, 0x1c, 0xc, 0x2, 0x0, 0x0,
    0x0, 0xb, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0x0, 0x0, 0x0, 0x6, 0x0, 0x2,
    0x30, 0x81, 0xd0, 0x2f, 0x0, 0x0, 0x0, 0x0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

const FIXTURE: &str = r##"{
  "nodes": [
    {"id":"g","type":"group","x":-40,"y":-60,"width":1200,"height":520,"label":"Group A","color":"4"},
    {"id":"t1","type":"text","x":0,"y":0,"width":300,"height":160,"text":"# Hello\n\nSome **bold** text.\n\n- one\n- two\n\n```\ncode\n```"},
    {"id":"f1","type":"file","x":400,"y":0,"width":300,"height":200,"file":"notes/Note.md","subpath":"#Second"},
    {"id":"l1","type":"link","x":800,"y":0,"width":300,"height":120,"url":"https://example.com/page"},
    {"id":"im","type":"file","x":0,"y":250,"width":200,"height":150,"file":"img.png"},
    {"id":"pdf","type":"file","x":400,"y":250,"width":200,"height":150,"file":"doc.pdf"},
    {"id":"cv","type":"file","x":800,"y":250,"width":200,"height":150,"file":"sub.canvas","color":"#336699"},
    {"id":"far","type":"text","x":6000,"y":6000,"width":200,"height":100,"text":"far away"},
    {"id":"bad","type":"text","x":"oops","y":0,"width":10,"height":10,"text":"x"},
    {"id":"odd","type":"text","x":6000,"y":6300,"width":200,"height":100,"text":"odd","color":"zzz"}
  ],
  "edges": [
    {"id":"e1","fromNode":"t1","toNode":"f1","label":"refers"},
    {"id":"e2","fromNode":"f1","toNode":"l1","color":"1","fromEnd":"arrow"},
    {"id":"e3","fromNode":"t1","toNode":"im"}
  ]
}"##;

const NOTE: &str =
    "# First\n\nnot shown\n\n# Second\n\nin the second section\n\n# Third\n\nlater\n";

const SUB: &str =
    r#"{"nodes":[{"id":"a","type":"text","x":0,"y":0,"width":100,"height":50,"text":"inner"}]}"#;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("notes")).expect("folder");
    std::fs::write(root.join("board.canvas"), FIXTURE).expect("write");
    std::fs::write(root.join("notes/Note.md"), NOTE).expect("write");
    std::fs::write(root.join("img.png"), PNG).expect("write");
    std::fs::write(root.join("doc.pdf"), b"%PDF-1.4\n").expect("write");
    std::fs::write(root.join("sub.canvas"), SUB).expect("write");
    std::fs::write(root.join("broken.canvas"), "[1, 2]").expect("write");
    dir
}

fn open(
    cx: &mut TestAppContext,
    file: &Path,
    root: &Path,
) -> (AnyWindowHandle, Entity<CanvasView>) {
    let (file, root) = (file.to_path_buf(), root.to_path_buf());
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        cx.set_reduce_motion(true);
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(HEIGHT)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |_, cx| CanvasView::open(file, Some(root), cx))
            .expect("test window opens")
    })
}

fn draw(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

/// Opens the fixture and draws it until it is laid out and its previews
/// are in.
fn shown(
    cx: &mut TestAppContext,
    name: &str,
) -> (tempfile::TempDir, Entity<CanvasView>, VisualTestContext) {
    let dir = fixture();
    let (window, view) = open(cx, &dir.path().join(name), dir.path());
    let mut vcx = VisualTestContext::from_window(window, cx);
    for _ in 0..3 {
        draw(&mut vcx);
    }
    vcx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
    });
    (dir, view, vcx)
}

/// Shows the world's origin at the view's top-left at zoom 1, offset by
/// (`x`, `y`) px.
fn at_origin(view: &Entity<CanvasView>, vcx: &mut VisualTestContext, x: f64, y: f64) {
    view.update(vcx, |v, cx| {
        v.set_transform(
            Transform {
                zoom: 1.0,
                pan: gv_canvas::Point::new(x, y),
            },
            cx,
        );
    });
    for _ in 0..3 {
        draw(vcx);
    }
}

/// The window point of world point (`x`, `y`).
fn window_point(
    view: &Entity<CanvasView>,
    vcx: &mut VisualTestContext,
    x: f64,
    y: f64,
) -> Point<Pixels> {
    view.read_with(vcx, |v, _| {
        let s = v.transform().world_to_screen(gv_canvas::Point::new(x, y));
        let o = v.frame.get().origin;
        point(o.x + px(s.x as f32), o.y + px(s.y as f32))
    })
}

/// Cmd on macOS, Ctrl elsewhere.
fn mod_key() -> Modifiers {
    if cfg!(target_os = "macos") {
        Modifiers::command()
    } else {
        Modifiers::control()
    }
}

fn node(view: &Entity<CanvasView>, vcx: &mut VisualTestContext, id: &str) -> usize {
    view.read_with(vcx, |v, _| {
        v.canvas()
            .expect("loaded")
            .nodes()
            .iter()
            .position(|n| n.id == id)
            .expect("node")
    })
}

fn events(view: &Entity<CanvasView>, vcx: &mut VisualTestContext) -> Rc<RefCell<Vec<CanvasEvent>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    vcx.update(|_, cx| {
        cx.subscribe(view, move |_, e: &CanvasEvent, _| {
            sink.borrow_mut().push(e.clone())
        })
        .detach();
    });
    seen
}

#[gpui::test]
fn renders_the_fixture_fitted_then_in_full_detail(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    let (status, t, viewport, fit, nodes) = view.read_with(&vcx, |v, _| {
        let doc = v.doc.clone().expect("loaded");
        (
            v.status().clone(),
            v.transform(),
            v.viewport(),
            doc.layout.fit(v.viewport(), &FitOptions::default()),
            doc.canvas.nodes().len(),
        )
    });
    assert_eq!(status, CanvasStatus::Ready);
    assert_eq!(
        (viewport.width as f32, viewport.height as f32),
        (WIDTH, HEIGHT)
    );
    // Opened fitted to the whole canvas, far nodes included: zoomed out to
    // blocks, nothing built.
    assert_eq!(t, fit);
    assert!(t.zoom < 0.3, "{t:?}");
    assert_eq!(nodes, 9, "the node with a bad x is left out");
    view.read_with(&vcx, |v, _| {
        assert_eq!(v.nodes_drawn, 8, "every card, as a block");
        assert_eq!(v.cards.builds, 0, "no content at block detail");
        assert_eq!(v.painted.get(), 4, "a group and three edges painted");
    });

    // At 100 %: the top of the board, in full detail.
    at_origin(&view, &mut vcx, 60.0, 80.0);
    let f1 = node(&view, &mut vcx, "f1");
    let im = node(&view, &mut vcx, "im");
    let pdf = node(&view, &mut vcx, "pdf");
    let cv = node(&view, &mut vcx, "cv");
    view.read_with(&vcx, |v, _| {
        assert_eq!(
            v.nodes_drawn, 6,
            "the six cards of the board, not the far ones"
        );
        assert!(v.cards.builds >= 2, "the text card and the note were built");
        match v.cards.preview(f1) {
            Some(Preview::Markdown(p)) => {
                assert!(p.text.contains("in the second section"), "{:?}", p.text);
                assert!(!p.text.contains("not shown") && !p.text.contains("later"));
            }
            other => panic!("note preview: {other:?}"),
        }
        assert!(matches!(v.cards.preview(im), Some(Preview::Image(_))));
        assert!(matches!(v.cards.preview(pdf), Some(Preview::Other(e)) if e.as_ref() == "PDF"));
        assert!(
            matches!(v.cards.preview(cv), Some(Preview::Canvas(d)) if d.canvas.nodes().len() == 1)
        );
    });
    // The text card's paragraphs were laid out and painted.
    let t1 = node(&view, &mut vcx, "t1");
    view.read_with(&vcx, |v, _| {
        let doc = v.doc.clone().expect("loaded");
        let parsed = v.cards.parsed.get(&t1).expect("parsed");
        assert!(parsed.text.starts_with("# Hello"));
        let built = v.cards.built.get(&t1).expect("built");
        let painted = built
            .blocks
            .iter()
            .filter(|b| matches!(b, super::Block::Text { block, .. } if block.borrow().bounds().is_some()))
            .count();
        assert!(painted >= 3, "heading, paragraph and list items painted: {painted}");
        assert_eq!(doc.canvas.edges().len(), 3);
    });
    assert!(vcx.debug_bounds("canvas-zoom").is_some());
}

#[gpui::test]
fn culls_items_outside_the_viewport(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    at_origin(&view, &mut vcx, 60.0, 80.0);
    let (visible, total) = view.read_with(&vcx, |v, _| {
        let doc = v.doc.clone().expect("loaded");
        (v.visible_items(), doc.layout.paint_order().len())
    });
    assert_eq!(total, 12, "9 nodes and 3 edges");
    let far = node(&view, &mut vcx, "far");
    assert!(!visible.contains(&ItemId::Node(far)));
    assert_eq!(
        visible.len(),
        10,
        "the group, the edges and six cards, not the two far ones: {visible:?}"
    );
    // Panned to the far cards: only they are drawn.
    at_origin(&view, &mut vcx, -5900.0, -5950.0);
    view.read_with(&vcx, |v, _| {
        assert_eq!(v.nodes_drawn, 2);
        assert_eq!(v.painted.get(), 0, "no group or edge there");
    });
}

#[gpui::test]
fn drag_and_wheel_pan_and_ctrl_wheel_zooms_at_the_pointer(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    at_origin(&view, &mut vcx, 60.0, 80.0);
    let none = Modifiers::none();
    // Drag on empty space (left of the minimap).
    let from = point(px(600.0), px(600.0));
    vcx.simulate_mouse_down(from, MouseButton::Left, none);
    vcx.simulate_mouse_move(point(px(650.0), px(630.0)), MouseButton::Left, none);
    vcx.simulate_mouse_move(point(px(680.0), px(650.0)), MouseButton::Left, none);
    vcx.simulate_mouse_up(point(px(680.0), px(650.0)), MouseButton::Left, none);
    let t = view.read_with(&vcx, |v, _| v.transform());
    assert_eq!((t.pan.x, t.pan.y, t.zoom), (140.0, 130.0, 1.0));

    // The wheel pans.
    vcx.simulate_event(ScrollEvent {
        position: point(px(500.0), px(300.0)),
        delta: ScrollDelta::Pixels(point(px(-10.0), px(-40.0))),
        ..ScrollEvent::default()
    });
    let t = view.read_with(&vcx, |v, _| v.transform());
    assert_eq!((t.pan.x, t.pan.y), (130.0, 90.0));

    // Ctrl-wheel zooms, keeping the world point under the pointer still.
    let at = point(px(400.0), px(250.0));
    let before = view.read_with(&vcx, |v, _| {
        v.transform()
            .screen_to_world(gv_canvas::Point::new(400.0, 250.0))
    });
    let zoom_key = mod_key();
    vcx.simulate_event(ScrollEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(0.0), px(100.0))),
        modifiers: zoom_key,
        ..ScrollEvent::default()
    });
    let t = view.read_with(&vcx, |v, _| v.transform());
    assert!(t.zoom > 1.1, "zoomed in: {t:?}");
    let after = t.screen_to_world(gv_canvas::Point::new(400.0, 250.0));
    assert!((after.x - before.x).abs() < 1e-6 && (after.y - before.y).abs() < 1e-6);

    // A pinch zooms too.
    vcx.simulate_event(gpui::PinchEvent {
        position: at,
        delta: -0.5,
        modifiers: none,
        phase: gpui::TouchPhase::Moved,
    });
    let z = view.read_with(&vcx, |v, _| v.transform().zoom);
    assert!((z - t.zoom * 0.5).abs() < 1e-6, "{z}");
}

#[gpui::test]
fn keyboard_walks_cards_and_enter_opens_the_file(cx: &mut TestAppContext) {
    let (dir, view, mut vcx) = shown(cx, "board.canvas");
    let seen = events(&view, &mut vcx);
    let order = view.read_with(&vcx, |v, _| {
        v.doc
            .clone()
            .expect("loaded")
            .layout
            .reading_order()
            .to_vec()
    });
    vcx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), Some(order[0]));
    vcx.simulate_keystrokes("tab tab");
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), Some(order[2]));
    vcx.simulate_keystrokes("shift-tab");
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), Some(order[1]));

    // From the text card, right is the note, down the image.
    let (t1, f1, im) = (
        node(&view, &mut vcx, "t1"),
        node(&view, &mut vcx, "f1"),
        node(&view, &mut vcx, "im"),
    );
    view.update(&mut vcx, |v, cx| v.select(Some(t1), cx));
    vcx.simulate_keystrokes("down");
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), Some(im));
    vcx.simulate_keystrokes("up right");
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), Some(f1));

    vcx.simulate_keystrokes("enter");
    let events = seen.borrow().clone();
    let [
        CanvasEvent::OpenFile {
            path,
            fragment,
            disposition,
        },
    ] = events.as_slice()
    else {
        panic!("one open: {events:?}");
    };
    assert_eq!(path, &dir.path().join("notes").join("Note.md"));
    assert_eq!(*disposition, Disposition::InPlace);
    let fragment = fragment.as_ref().expect("the subpath");
    assert_eq!(
        fragment.kind,
        FragmentKind::Heading(vec!["Second".to_owned()])
    );

    vcx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), None);
}

#[gpui::test]
fn clicks_open_file_and_link_cards(cx: &mut TestAppContext) {
    let (dir, view, mut vcx) = shown(cx, "board.canvas");
    at_origin(&view, &mut vcx, 60.0, 80.0);
    let seen = events(&view, &mut vcx);

    // A text card is only selected.
    let t1 = node(&view, &mut vcx, "t1");
    let at = window_point(&view, &mut vcx, 150.0, 80.0);
    vcx.simulate_click(at, Modifiers::none());
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), Some(t1));
    assert!(seen.borrow().is_empty());

    // The PDF card opens its file; Mod-click asks for a new tab.
    let at = window_point(&view, &mut vcx, 500.0, 300.0);
    vcx.simulate_click(at, mod_key());
    // The link card follows its URL.
    let at = window_point(&view, &mut vcx, 900.0, 50.0);
    vcx.simulate_click(at, Modifiers::none());
    // Middle click: a background tab.
    let at = window_point(&view, &mut vcx, 100.0, 300.0);
    vcx.simulate_mouse_down(at, MouseButton::Middle, Modifiers::none());
    vcx.simulate_mouse_up(at, MouseButton::Middle, Modifiers::none());
    // Empty space (below the group) clears the selection.
    let at = window_point(&view, &mut vcx, 350.0, 500.0);
    vcx.simulate_click(at, Modifiers::none());

    let events = seen.borrow().clone();
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(
        events[0],
        CanvasEvent::OpenFile {
            path: dir.path().join("doc.pdf"),
            fragment: None,
            disposition: Disposition::NewTab,
        }
    );
    match &events[1] {
        CanvasEvent::FollowLink(r) => {
            assert_eq!(r.destination.as_ref(), "https://example.com/page");
            assert_eq!(r.disposition, Disposition::InPlace);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        &events[2],
        CanvasEvent::OpenFile { path, disposition: Disposition::Background, .. }
            if path == &dir.path().join("img.png")
    ));
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), None);
}

#[gpui::test]
fn fit_shortcut_and_zoom_commands(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    at_origin(&view, &mut vcx, 60.0, 80.0);
    vcx.simulate_keystrokes("home");
    let (t, fit) = view.read_with(&vcx, |v, _| {
        let doc = v.doc.clone().expect("loaded");
        (
            v.transform(),
            doc.layout.fit(v.viewport(), &FitOptions::default()),
        )
    });
    assert_eq!(t, fit);

    vcx.dispatch_action(crate::keymap::actions::view::ZoomReset);
    assert!((view.read_with(&vcx, |v, _| v.transform().zoom) - 1.0).abs() < 1e-9);
    vcx.dispatch_action(crate::keymap::actions::view::ZoomIn);
    assert!((view.read_with(&vcx, |v, _| v.transform().zoom) - 1.25).abs() < 1e-9);
    vcx.dispatch_action(crate::keymap::actions::view::ZoomOut);
    assert!((view.read_with(&vcx, |v, _| v.transform().zoom) - 1.0).abs() < 1e-9);

    let before = view.read_with(&vcx, |v, _| v.transform());
    vcx.simulate_keystrokes("shift-left");
    let after = view.read_with(&vcx, |v, _| v.transform());
    assert!((after.pan.x - before.pan.x - f64::from(WIDTH) * 0.1).abs() < 1e-6);
}

#[gpui::test]
fn reduced_motion_jumps_and_otherwise_animates(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    at_origin(&view, &mut vcx, 60.0, 80.0);
    let far = node(&view, &mut vcx, "far");
    // Reduced motion (the tests' default here): the view jumps.
    view.update(&mut vcx, |v, cx| v.select(Some(far), cx));
    view.read_with(&vcx, |v, _| {
        assert!(v.animation.is_none());
        let s = v
            .transform
            .world_rect_to_screen(&v.doc.as_ref().expect("loaded").layout.node_rect(far));
        assert!(s.x >= 0.0 && s.right() <= f64::from(WIDTH), "{s:?}");
    });
    // With motion: an animation towards the same kind of target.
    vcx.update(|_, cx| cx.set_reduce_motion(false));
    at_origin(&view, &mut vcx, 60.0, 80.0);
    view.update(&mut vcx, |v, cx| v.select(Some(far), cx));
    let (animating, target) = view.read_with(&vcx, |v, _| (v.animation.is_some(), v.transform()));
    assert!(animating);
    assert_ne!(target, view.read_with(&vcx, |v, _| v.transform));
    // A press ends it where it was heading.
    let at = point(px(10.0), px(10.0));
    vcx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    view.read_with(&vcx, |v, _| {
        assert!(v.animation.is_none());
        assert_eq!(v.transform, target);
    });
    // Reduced motion turned on while the view moves: the next frame lands.
    at_origin(&view, &mut vcx, 60.0, 80.0);
    view.update(&mut vcx, |v, cx| v.select(Some(far), cx));
    assert!(view.read_with(&vcx, |v, _| v.animation.is_some()));
    let target = view.read_with(&vcx, |v, _| v.transform());
    vcx.update(|window, cx| {
        cx.set_reduce_motion(true);
        window.draw(cx).clear(cx);
    });
    view.read_with(&vcx, |v, _| {
        assert!(v.animation.is_none(), "no motion once reduced");
        assert_eq!(v.transform, target);
    });
}

#[gpui::test]
fn problems_are_counted_and_listed(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    let (issues, odd_issues) = view.read_with(&vcx, |v, _| {
        let doc = v.doc.clone().expect("loaded");
        let odd = doc
            .canvas
            .nodes()
            .iter()
            .position(|n| n.id == "odd")
            .expect("odd");
        (doc.canvas.issues().len(), doc.node_issues[odd].len())
    });
    assert!(issues >= 2, "the bad x and the bad colour");
    assert_eq!(odd_issues, 1, "the bad colour is the odd card's warning");
    let chip = vcx
        .debug_bounds("canvas-issues")
        .expect("the problems chip");
    vcx.simulate_click(chip.center(), Modifiers::none());
    draw(&mut vcx);
    assert!(view.read_with(&vcx, |v, _| v.issues_open));
    assert!(vcx.debug_bounds("canvas-issue-list").is_some());
    // The click did not reach the canvas.
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), None);
}

#[gpui::test]
fn a_file_that_is_not_a_canvas_says_why(cx: &mut TestAppContext) {
    let (_dir, view, vcx) = shown(cx, "broken.canvas");
    let status = view.read_with(&vcx, |v, _| v.status().clone());
    assert!(matches!(status, CanvasStatus::Failed(why) if why.contains("object")));
}

#[gpui::test]
fn reloading_the_same_file_keeps_the_view(cx: &mut TestAppContext) {
    let (dir, view, mut vcx) = shown(cx, "board.canvas");
    at_origin(&view, &mut vcx, 60.0, 80.0);
    let t1 = node(&view, &mut vcx, "t1");
    view.update(&mut vcx, |v, cx| v.select(Some(t1), cx));
    let path = dir.path().join("board.canvas");
    view.update(&mut vcx, |v, cx| v.load(path, cx));
    draw(&mut vcx);
    view.read_with(&vcx, |v, _| {
        assert_eq!(v.status(), &CanvasStatus::Ready);
        assert_eq!(v.transform().zoom, 1.0);
        assert_eq!(v.selected(), Some(t1));
    });
    // Another file is shown whole.
    let other = dir.path().join("sub.canvas");
    view.update(&mut vcx, |v, cx| v.load(other, cx));
    draw(&mut vcx);
    draw(&mut vcx);
    view.read_with(&vcx, |v, _| {
        assert_eq!(v.selected(), None);
        assert_eq!(v.canvas().map(|c| c.nodes().len()), Some(1));
        assert_eq!(v.transform().zoom, 1.0, "a small canvas is fitted at 100 %");
        let doc = v.doc.clone().expect("loaded");
        assert_eq!(
            v.transform(),
            doc.layout.fit(v.viewport(), &FitOptions::default())
        );
    });
}

/// The window point of world point (`x`, `y`) on the minimap of the last
/// frame.
fn map_point(
    view: &Entity<CanvasView>,
    vcx: &mut VisualTestContext,
    x: f64,
    y: f64,
) -> Point<Pixels> {
    view.read_with(vcx, |v, _| {
        let map = v.minimap_layout().expect("the minimap is shown");
        let p = map.map.world_to_screen(gv_canvas::Point::new(x, y));
        let o = v.minimap.bounds.get().origin;
        point(o.x + px(p.x as f32), o.y + px(p.y as f32))
    })
}

/// The world point at the centre of the view.
fn view_centre(view: &Entity<CanvasView>, vcx: &mut VisualTestContext) -> gv_canvas::Point {
    view.read_with(vcx, |v, _| {
        v.transform().screen_to_world(gv_canvas::Point::new(
            f64::from(WIDTH) / 2.0,
            f64::from(HEIGHT) / 2.0,
        ))
    })
}

fn near(a: gv_canvas::Point, b: gv_canvas::Point, tolerance: f64) -> bool {
    (a.x - b.x).abs() <= tolerance && (a.y - b.y).abs() <= tolerance
}

#[gpui::test]
fn the_minimap_shows_when_the_canvas_leaves_the_view(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    // Fitted: the whole canvas is in view, no map.
    assert!(!view.read_with(&vcx, |v, _| v.minimap_shown()));
    assert!(vcx.debug_bounds("canvas-minimap").is_none());

    at_origin(&view, &mut vcx, 60.0, 80.0);
    assert!(view.read_with(&vcx, |v, _| v.minimap_shown()));
    let b = vcx.debug_bounds("canvas-minimap").expect("drawn");
    assert!(f32::from(b.right()) <= WIDTH && f32::from(b.left()) > WIDTH / 2.0);
    assert!(f32::from(b.bottom()) <= HEIGHT && f32::from(b.top()) > HEIGHT / 2.0);
    // The viewport rectangle is on the map, the far cards too.
    let map = view.read_with(&vcx, |v, _| v.minimap_layout().expect("map"));
    let far = node(&view, &mut vcx, "far");
    let far_rect = view.read_with(&vcx, |v, _| {
        v.doc.as_ref().expect("loaded").layout.node_rect(far)
    });
    let on_map = map.rect(&far_rect);
    let frame = gv_canvas::Rect::new(0.0, 0.0, 200.0, 140.0).inflate(1e-6);
    assert!(frame.contains_rect(&on_map) && frame.contains_rect(&map.viewport));

    // A click there centres the view on it (reduced motion: at once),
    // selecting nothing and opening nothing.
    let seen = events(&view, &mut vcx);
    let c = far_rect.center();
    let at = map_point(&view, &mut vcx, c.x, c.y);
    vcx.simulate_click(at, Modifiers::none());
    draw(&mut vcx);
    let tolerance = 2.0 / map.map.zoom;
    assert!(near(view_centre(&view, &mut vcx), c, tolerance));
    view.read_with(&vcx, |v, _| {
        assert!(v.animation.is_none());
        assert_eq!(v.selected(), None);
        assert_eq!(v.transform().zoom, 1.0, "the zoom is kept");
    });
    assert!(seen.borrow().is_empty());

    // A drag moves the view along.
    let from = map_point(&view, &mut vcx, 500.0, 200.0);
    let to = map_point(&view, &mut vcx, 300.0, 100.0);
    let none = Modifiers::none();
    vcx.simulate_mouse_down(from, MouseButton::Left, none);
    let target = view.read_with(&vcx, |v, _| {
        v.minimap_layout()
            .expect("map")
            .to_world(gv_canvas::Point::new(
                f64::from(f32::from(to.x - v.minimap.bounds.get().origin.x)),
                f64::from(f32::from(to.y - v.minimap.bounds.get().origin.y)),
            ))
    });
    vcx.simulate_mouse_move(to, MouseButton::Left, none);
    vcx.simulate_mouse_up(to, MouseButton::Left, none);
    draw(&mut vcx);
    assert!(near(view_centre(&view, &mut vcx), target, 1e-6));

    // Fitting brings everything in view: the map goes.
    vcx.dispatch_action(crate::keymap::actions::view::ZoomToFit);
    draw(&mut vcx);
    assert!(!view.read_with(&vcx, |v, _| v.minimap_shown()));
}

#[gpui::test]
fn a_minimap_click_animates_unless_motion_is_reduced(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    vcx.update(|_, cx| cx.set_reduce_motion(false));
    at_origin(&view, &mut vcx, 60.0, 80.0);
    let at = map_point(&view, &mut vcx, 6100.0, 6150.0);
    vcx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    view.read_with(&vcx, |v, _| {
        assert!(v.animation.is_some(), "animated");
        assert_ne!(v.transform, v.transform());
    });
    vcx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
}

#[gpui::test]
fn a_canvas_that_fits_has_no_minimap(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "sub.canvas");
    at_origin(&view, &mut vcx, 0.0, 0.0);
    assert!(!view.read_with(&vcx, |v, _| v.minimap_shown()));
    at_origin(&view, &mut vcx, 300.0, 300.0);
    assert!(!view.read_with(&vcx, |v, _| v.minimap_shown()));
}

#[gpui::test]
fn zoom_to_fit_is_a_command(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    at_origin(&view, &mut vcx, 60.0, 80.0);
    vcx.dispatch_action(crate::keymap::actions::view::ZoomToFit);
    let (t, fit) = view.read_with(&vcx, |v, _| {
        let doc = v.doc.clone().expect("loaded");
        (
            v.transform(),
            doc.layout.fit(v.viewport(), &FitOptions::default()),
        )
    });
    assert_eq!(t, fit);
    // Shift+1 runs it too.
    at_origin(&view, &mut vcx, 60.0, 80.0);
    vcx.simulate_keystrokes("shift-1");
    assert_eq!(view.read_with(&vcx, |v, _| v.transform()), fit);
}

#[gpui::test]
fn cards_are_a_list_in_reading_order(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    // The canvas is a named group that takes the keyboard.
    let root = testing::named(&mut vcx, "Canvas board");
    assert_eq!(root.role, Some(Role::Group));
    assert!(root.focusable);
    let list = testing::named(&mut vcx, "8 cards, 3 connections");
    assert_eq!(list.role, Some(Role::List));
    let items: Vec<testing::Node> = testing::with_role(&mut vcx, Role::ListItem);
    let names: Vec<&str> = items.iter().filter_map(|n| n.label.as_deref()).collect();
    assert_eq!(
        names,
        [
            "Text card: Hello",
            "File card: Note.md#Second",
            "Link card: https://example.com/page",
            "File card: img.png",
            "File card: doc.pdf",
            "File card: sub.canvas",
            "Text card: far away",
            "Text card: odd",
        ]
    );
    for (i, item) in items.iter().enumerate() {
        assert_eq!(item.position, Some((i + 1, 8)));
        assert_eq!(item.selected, Some(false));
        assert!(item.clickable, "a click action opens the card");
    }
    // Edges and groups are described on the cards.
    let desc = |name: &str, vcx: &mut VisualTestContext| {
        testing::named(vcx, name).description.unwrap_or_default()
    };
    assert_eq!(
        desc("Text card: Hello", &mut vcx),
        "In group Group A. Links to Note.md#Second (refers), img.png"
    );
    assert_eq!(
        desc("File card: Note.md#Second", &mut vcx),
        "In group Group A. Links to https://example.com/page. Linked from Hello (refers)"
    );
    assert!(desc("Text card: odd", &mut vcx).starts_with("Problems: `color`"));
    assert_eq!(desc("Text card: far away", &mut vcx), "");

    // The selected card is reported as it moves.
    vcx.simulate_keystrokes("tab");
    draw(&mut vcx);
    assert_eq!(
        testing::named(&mut vcx, "Text card: Hello").selected,
        Some(true)
    );
    vcx.simulate_keystrokes("tab");
    draw(&mut vcx);
    assert_eq!(
        testing::named(&mut vcx, "Text card: Hello").selected,
        Some(false)
    );
    assert_eq!(
        testing::named(&mut vcx, "File card: Note.md#Second").selected,
        Some(true)
    );
    assert_eq!(
        view.read_with(&vcx, |v, _| v.selected()),
        Some(node(&view, &mut vcx, "f1"))
    );
}

#[gpui::test]
fn problems_zoom_and_minimap_are_named(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    at_origin(&view, &mut vcx, 60.0, 80.0);
    let n = view.read_with(&vcx, |v, _| v.canvas().expect("loaded").issues().len());
    let chip_name = format!("{n} problems");
    let chip = testing::named(&mut vcx, &chip_name);
    assert_eq!(chip.role, Some(Role::Button));
    assert_eq!(chip.expanded, Some(false));
    let b = vcx.debug_bounds("canvas-issues").expect("chip");
    vcx.simulate_click(b.center(), Modifiers::none());
    draw(&mut vcx);
    assert_eq!(testing::named(&mut vcx, &chip_name).expanded, Some(true));
    let problems = testing::named(&mut vcx, "Problems");
    assert_eq!(problems.role, Some(Role::List));
    let zoom = testing::named(&mut vcx, "Zoom 100%");
    assert_eq!(zoom.role, Some(Role::Status));
    assert!(zoom.live.is_some());
    assert_eq!(testing::named(&mut vcx, "Minimap").role, Some(Role::Image));
}

// ----- PDF first pages, group backgrounds, the problems chip by keyboard -----------

/// A PDF engine that draws a 4 × 6 page (or fails), counting the pages
/// asked for.
struct FakePdf {
    pages: Arc<AtomicUsize>,
    fail: bool,
}

impl crate::export::PdfEngine for FakePdf {
    fn render(
        &self,
        _: &gv_export::TypstDocument,
        _: u32,
        _: &AtomicBool,
    ) -> Result<Vec<u8>, crate::export::PdfError> {
        Err(crate::export::PdfError::Failed("not here".into()))
    }

    fn draws_pages(&self) -> bool {
        true
    }

    fn page(
        &self,
        pdf: &[u8],
        page: u32,
        max_width: u32,
        max_height: u32,
        _: &AtomicBool,
    ) -> Result<crate::export::PageRaster, crate::export::PdfError> {
        self.pages.fetch_add(1, Ordering::SeqCst);
        assert_eq!(page, 0, "the first page");
        assert!(pdf.starts_with(b"%PDF") && max_width > 0 && max_height > 0);
        if self.fail {
            return Err(crate::export::PdfError::Failed("broken".into()));
        }
        Ok(crate::export::PageRaster {
            width: 4,
            height: 6,
            rgba: vec![200; 4 * 6 * 4],
            pages: 3,
        })
    }
}

fn install_pdf(vcx: &mut VisualTestContext, fail: bool) -> Arc<AtomicUsize> {
    let pages = Arc::new(AtomicUsize::new(0));
    let engine = FakePdf {
        pages: pages.clone(),
        fail,
    };
    vcx.update(|_, cx| crate::export::set_pdf_engine(cx, Some(Arc::new(engine))));
    pages
}

#[gpui::test]
fn pdf_cards_show_their_first_page(cx: &mut TestAppContext) {
    let (dir, view, mut vcx) = shown(cx, "board.canvas");
    let pages = install_pdf(&mut vcx, false);
    at_origin(&view, &mut vcx, 60.0, 80.0);
    let pdf = node(&view, &mut vcx, "pdf");
    view.read_with(&vcx, |v, _| {
        assert!(
            matches!(v.cards.preview(pdf), Some(Preview::Image(i)) if i.size(0).width == gpui::DevicePixels(4)),
            "{:?}",
            v.cards.preview(pdf)
        );
    });
    assert_eq!(pages.load(Ordering::SeqCst), 1);
    // Read again (a change on disk elsewhere): the page comes from the
    // cache.
    view.update(&mut vcx, |v, cx| {
        v.load(dir.path().join("board.canvas"), cx)
    });
    for _ in 0..4 {
        draw(&mut vcx);
    }
    view.read_with(&vcx, |v, _| {
        assert!(matches!(v.cards.preview(pdf), Some(Preview::Image(_))));
    });
    assert_eq!(pages.load(Ordering::SeqCst), 1, "drawn once");
}

#[gpui::test]
fn a_pdf_that_cannot_be_drawn_is_a_file_card(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    let pages = install_pdf(&mut vcx, true);
    at_origin(&view, &mut vcx, 60.0, 80.0);
    let pdf = node(&view, &mut vcx, "pdf");
    view.read_with(&vcx, |v, _| {
        assert!(matches!(v.cards.preview(pdf), Some(Preview::Other(e)) if e.as_ref() == "PDF"));
    });
    assert_eq!(pages.load(Ordering::SeqCst), 1);
}

const BACKGROUNDS: &str = r#"{
  "nodes": [
    {"id":"tiled","type":"group","x":0,"y":0,"width":300,"height":200,"label":"Tiled","background":"img.png","backgroundStyle":"repeat"},
    {"id":"cover","type":"group","x":400,"y":0,"width":300,"height":200,"background":"img.png"},
    {"id":"remote","type":"group","x":0,"y":300,"width":300,"height":200,"background":"https://example.com/bg.png"},
    {"id":"gone","type":"group","x":400,"y":300,"width":300,"height":200,"background":"missing.png","backgroundStyle":"ratio"},
    {"id":"t","type":"text","x":20,"y":20,"width":100,"height":60,"text":"in the group"}
  ]
}"#;

#[gpui::test]
fn group_backgrounds_are_painted(cx: &mut TestAppContext) {
    let dir = fixture();
    std::fs::write(dir.path().join("bg.canvas"), BACKGROUNDS).expect("write");
    let (window, view) = open(cx, &dir.path().join("bg.canvas"), dir.path());
    let mut vcx = VisualTestContext::from_window(window, cx);
    for _ in 0..4 {
        draw(&mut vcx);
    }
    at_origin(&view, &mut vcx, 20.0, 20.0);
    let ids = ["tiled", "cover", "remote", "gone"].map(|id| node(&view, &mut vcx, id));
    view.read_with(&vcx, |v, _| {
        use super::cards::Background;
        assert!(matches!(v.cards.background(ids[0]), Some(Background::Ready(b)) if b.width == 1.0));
        assert!(matches!(
            v.cards.background(ids[1]),
            Some(Background::Ready(_))
        ));
        // A remote background is never fetched; a missing file is skipped.
        assert!(matches!(v.cards.background(ids[2]), Some(Background::None)));
        assert!(matches!(v.cards.background(ids[3]), Some(Background::None)));
        assert_eq!(v.backgrounds_painted.get(), 2);
        assert_eq!(v.painted.get(), 4, "every group is still drawn");
    });
}

/// Presses and releases `key` (buttons act on the release).
fn press(vcx: &mut VisualTestContext, key: &'static str) {
    vcx.simulate_keystrokes(key);
    vcx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse(key).expect("a key"),
    });
    draw(vcx);
}

#[gpui::test]
fn tab_reaches_the_problems_chip_after_the_last_card(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    let (order, issues) = view.read_with(&vcx, |v, _| {
        let doc = v.doc.clone().expect("loaded");
        (
            doc.layout.reading_order().to_vec(),
            doc.canvas.issues().len(),
        )
    });
    let chip = format!("{issues} problems");
    let chip_focused = |vcx: &mut VisualTestContext| {
        let handle = view.read_with(vcx, |v, _| v.issues_focus.clone());
        vcx.update(|window, _| handle.is_focused(window))
    };
    for _ in 0..order.len() {
        vcx.simulate_keystrokes("tab");
    }
    assert_eq!(
        view.read_with(&vcx, |v, _| v.selected()),
        order.last().copied()
    );
    // Past the last card: the chip, and no card selected.
    vcx.simulate_keystrokes("tab");
    draw(&mut vcx);
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), None);
    assert!(chip_focused(&mut vcx));
    assert_eq!(
        testing::focused(&mut vcx).flatten().as_deref(),
        Some(chip.as_str())
    );
    // Enter and Space work it as a button, opening nothing else.
    let seen = events(&view, &mut vcx);
    press(&mut vcx, "enter");
    assert!(view.read_with(&vcx, |v, _| v.issues_open));
    press(&mut vcx, "space");
    assert!(!view.read_with(&vcx, |v, _| v.issues_open));
    assert!(seen.borrow().is_empty());
    // Shift+Tab goes back to the last card, Tab to the chip again.
    vcx.simulate_keystrokes("shift-tab");
    assert!(!chip_focused(&mut vcx));
    assert_eq!(
        view.read_with(&vcx, |v, _| v.selected()),
        order.last().copied()
    );
    vcx.simulate_keystrokes("tab");
    assert!(chip_focused(&mut vcx));
    // Tab past the chip is the window's: nothing wraps round to the first
    // card (the focus goes to the window's next stop, here the canvas
    // itself, with no card selected).
    vcx.simulate_keystrokes("tab");
    draw(&mut vcx);
    assert!(!chip_focused(&mut vcx));
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), None);
}

/// A window with a stop before and after a canvas, moving the focus as
/// the app's window root does with the Tab keys the canvas leaves alone.
struct Around {
    before: gpui::FocusHandle,
    canvas: Entity<CanvasView>,
    after: gpui::FocusHandle,
}

impl gpui::Render for Around {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        cx: &mut gpui::Context<'_, Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{InteractiveElement as _, ParentElement as _, Styled as _};
        gpui::div()
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|_, _: &super::NextCard, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &super::PreviousCard, window, cx| window.focus_prev(cx)))
            .child(gpui::div().h(px(20.0)).track_focus(&self.before))
            .child(gpui::div().flex_1().child(self.canvas.clone()))
            .child(gpui::div().h(px(20.0)).track_focus(&self.after))
    }
}

#[gpui::test]
fn tab_leaves_the_canvas_past_its_last_stop(cx: &mut TestAppContext) {
    let dir = fixture();
    let (file, root) = (dir.path().join("board.canvas"), dir.path().to_path_buf());
    let (window, around) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        cx.set_reduce_motion(true);
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(HEIGHT)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |_, cx| {
            cx.new(|cx| Around {
                before: cx.focus_handle().tab_index(0).tab_stop(true),
                canvas: CanvasView::open(file, Some(root), cx),
                after: cx.focus_handle().tab_index(0).tab_stop(true),
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    for _ in 0..3 {
        draw(&mut vcx);
    }
    let (view, before, after) = around.read_with(&vcx, |a, _| {
        (a.canvas.clone(), a.before.clone(), a.after.clone())
    });
    let focused = |vcx: &mut VisualTestContext, h: &gpui::FocusHandle| {
        let h = h.clone();
        vcx.update(|window, _| h.is_focused(window))
    };
    let canvas_focus = view.read_with(&vcx, |v, _| v.focus.clone());
    let order = view.read_with(&vcx, |v, _| {
        v.doc
            .clone()
            .expect("loaded")
            .layout
            .reading_order()
            .to_vec()
    });
    let f = canvas_focus.clone();
    vcx.update(|window, cx| window.focus(&f, cx));

    // The cards, the problems chip, then on out of the canvas.
    for _ in 0..order.len() + 1 {
        vcx.simulate_keystrokes("tab");
        draw(&mut vcx);
    }
    assert!(!focused(&mut vcx, &after));
    vcx.simulate_keystrokes("tab");
    draw(&mut vcx);
    assert!(focused(&mut vcx, &after), "Tab leaves the canvas");
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), None);

    // Back in from behind: the last card; Shift+Tab before the first card
    // leaves the other way.
    let f = canvas_focus.clone();
    vcx.update(|window, cx| window.focus(&f, cx));
    vcx.simulate_keystrokes("shift-tab");
    assert_eq!(
        view.read_with(&vcx, |v, _| v.selected()),
        order.last().copied()
    );
    view.update(&mut vcx, |v, cx| v.select(Some(order[0]), cx));
    draw(&mut vcx);
    vcx.simulate_keystrokes("shift-tab");
    draw(&mut vcx);
    assert!(focused(&mut vcx, &before), "Shift+Tab leaves the canvas");
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), None);
    // Coming back with Tab starts at the first card again.
    let f = canvas_focus.clone();
    vcx.update(|window, cx| window.focus(&f, cx));
    vcx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(&vcx, |v, _| v.selected()), Some(order[0]));
}

// ----- the PDF queue, file paths, the minimap drag ---------------------------------

const PDFS: &str = r#"{
  "nodes": [
    {"id":"a","type":"file","x":0,"y":0,"width":200,"height":150,"file":"a.pdf"},
    {"id":"b","type":"file","x":250,"y":0,"width":200,"height":150,"file":"b.pdf"},
    {"id":"c","type":"file","x":0,"y":200,"width":200,"height":150,"file":"c.pdf"},
    {"id":"d","type":"file","x":250,"y":200,"width":200,"height":150,"file":"d.pdf"}
  ]
}"#;

/// The PDF board opened with a page-drawing engine, drawn until its
/// pages are in.
fn pdf_board(
    cx: &mut TestAppContext,
) -> (
    tempfile::TempDir,
    Entity<CanvasView>,
    VisualTestContext,
    Arc<AtomicUsize>,
) {
    let dir = fixture();
    for name in ["a", "b", "c", "d"] {
        std::fs::write(dir.path().join(format!("{name}.pdf")), b"%PDF-1.4\n").expect("write");
    }
    std::fs::write(dir.path().join("pdfs.canvas"), PDFS).expect("write");
    let (window, view) = open(cx, &dir.path().join("pdfs.canvas"), dir.path());
    let mut vcx = VisualTestContext::from_window(window, cx);
    let pages = install_pdf(&mut vcx, false);
    for _ in 0..4 {
        draw(&mut vcx);
    }
    (dir, view, vcx, pages)
}

fn all_previews(view: &Entity<CanvasView>, vcx: &mut VisualTestContext, ok: fn(&Preview) -> bool) {
    view.read_with(vcx, |v, _| {
        assert!(!v.pdf_busy && v.pdf_queue.is_empty());
        for n in 0..4 {
            let p = v.cards.preview(n);
            assert!(p.is_some_and(ok), "{n}: {p:?}");
        }
    });
}

#[gpui::test]
fn pdf_cards_are_drawn_one_at_a_time(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx, pages) = pdf_board(cx);
    all_previews(&view, &mut vcx, |p| matches!(p, Preview::Image(_)));
    assert_eq!(pages.load(Ordering::SeqCst), 4);
    // Asked for again: one page goes to the background executor, the
    // others wait in the queue.
    view.update(&mut vcx, |v, cx| {
        v.cards.clear();
        v.load_previews(vec![0, 1, 2, 3], cx);
        assert!(v.pdf_busy);
        assert_eq!(v.pdf_queue.len(), 3);
    });
    for _ in 0..4 {
        draw(&mut vcx);
    }
    all_previews(&view, &mut vcx, |p| matches!(p, Preview::Image(_)));
}

#[gpui::test]
fn queued_pdf_pages_are_dropped_when_no_longer_wanted(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx, _pages) = pdf_board(cx);
    let elsewhere = tempfile::tempdir().expect("dir");
    view.update(&mut vcx, |v, cx| {
        v.cards.clear();
        v.load_previews(vec![0, 1, 2, 3], cx);
        let old = v.pdf_cancel.clone();
        // Another vault: what was asked for is cancelled, the queue
        // dropped.
        v.set_vault_root(Some(elsewhere.path().to_path_buf()), cx);
        assert!(old.load(Ordering::SeqCst));
        assert!(v.pdf_queue.is_empty());
        // The job in flight still ends before the next one starts.
        assert!(v.pdf_busy);
    });
    for _ in 0..4 {
        draw(&mut vcx);
    }
    // The cards are looked up again in the other vault, where they are
    // missing; the cancelled job left nothing behind.
    all_previews(&view, &mut vcx, |p| matches!(p, Preview::Missing(_)));
    // A closed view cancels its pages too.
    let flag = vcx.update(|_, cx| {
        let other = cx.new(CanvasView::new);
        let flag = other.read(cx).pdf_cancel.clone();
        drop(other);
        flag
    });
    vcx.run_until_parked();
    assert!(flag.load(Ordering::SeqCst));
}

#[gpui::test]
fn file_cards_outside_the_vault_are_not_read_or_opened(cx: &mut TestAppContext) {
    let outer = fixture();
    let vault = outer.path().join("vault");
    std::fs::create_dir_all(&vault).expect("folder");
    std::fs::write(outer.path().join("secret.md"), "# Secret\n").expect("write");
    let board = r#"{
  "nodes": [
    {"id":"up","type":"file","x":0,"y":0,"width":200,"height":150,"file":"../secret.md"},
    {"id":"unc","type":"file","x":250,"y":0,"width":200,"height":150,"file":"\\\\server\\share\\x.md"},
    {"id":"drive","type":"file","x":0,"y":200,"width":200,"height":150,"file":"C:/x.md"},
    {"id":"bg","type":"group","x":-50,"y":-50,"width":600,"height":500,"background":"../img.png"}
  ]
}"#;
    std::fs::write(vault.join("b.canvas"), board).expect("write");
    let (window, view) = open(cx, &vault.join("b.canvas"), &vault);
    let mut vcx = VisualTestContext::from_window(window, cx);
    for _ in 0..4 {
        draw(&mut vcx);
    }
    let seen = events(&view, &mut vcx);
    for id in ["up", "unc", "drive"] {
        let n = node(&view, &mut vcx, id);
        view.read_with(&vcx, |v, _| {
            assert!(
                matches!(v.cards.preview(n), Some(Preview::Missing(why)) if why.contains("outside")),
                "{id}: {:?}",
                v.cards.preview(n)
            );
        });
        view.update(&mut vcx, |v, cx| v.open_node(n, Disposition::InPlace, cx));
    }
    assert!(seen.borrow().is_empty(), "{:?}", seen.borrow());
    let bg = node(&view, &mut vcx, "bg");
    view.read_with(&vcx, |v, _| {
        assert!(matches!(
            v.cards.background(bg),
            Some(super::cards::Background::None)
        ));
    });
}

#[gpui::test]
fn a_minimap_drag_holds_the_map_still_and_ends_on_any_release(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    at_origin(&view, &mut vcx, 60.0, 80.0);
    let map = view.read_with(&vcx, |v, _| v.minimap_layout().expect("map"));
    let origin = view.read_with(&vcx, |v, _| v.minimap.bounds.get().origin);
    let on_map = |x: f64, y: f64| {
        let p = map.map.world_to_screen(gv_canvas::Point::new(x, y));
        point(origin.x + px(p.x as f32), origin.y + px(p.y as f32))
    };
    let none = Modifiers::none();
    vcx.simulate_mouse_down(on_map(500.0, 200.0), MouseButton::Left, none);
    draw(&mut vcx);
    // Each step lands where the map of the press says, frames between.
    let tolerance = 2.0 / map.map.zoom;
    for (x, y) in [(300.0, 100.0), (3000.0, 3000.0), (6000.0, 6100.0)] {
        vcx.simulate_mouse_move(on_map(x, y), MouseButton::Left, none);
        draw(&mut vcx);
        let c = view_centre(&view, &mut vcx);
        assert!(
            near(c, gv_canvas::Point::new(x, y), tolerance),
            "{c:?} for {x}, {y}"
        );
        let now = view.read_with(&vcx, |v, _| v.minimap_layout().expect("map"));
        assert_eq!(now.world, map.world, "the map holds still");
    }
    // Released off the map: the drag is over all the same.
    vcx.simulate_mouse_up(point(px(100.0), px(100.0)), MouseButton::Left, none);
    draw(&mut vcx);
    view.read_with(&vcx, |v, _| {
        assert!(!v.minimap.dragging && v.minimap.frozen.is_none());
    });
    // A later pan that crosses the map only pans.
    let before = view.read_with(&vcx, |v, _| v.transform());
    let b = vcx.debug_bounds("canvas-minimap").expect("drawn");
    let start = point(b.left() - px(100.0), b.center().y);
    vcx.simulate_mouse_down(start, MouseButton::Left, none);
    vcx.simulate_mouse_move(b.center(), MouseButton::Left, none);
    draw(&mut vcx);
    vcx.simulate_mouse_move(
        b.center() + point(px(20.0), px(10.0)),
        MouseButton::Left,
        none,
    );
    draw(&mut vcx);
    vcx.simulate_mouse_up(b.center(), MouseButton::Left, none);
    let after = view.read_with(&vcx, |v, _| v.transform());
    let moved = b.center() + point(px(20.0), px(10.0)) - start;
    let expected = before.panned(f64::from(f32::from(moved.x)), f64::from(f32::from(moved.y)));
    assert!(
        (after.pan.x - expected.pan.x).abs() < 1e-3 && (after.pan.y - expected.pan.y).abs() < 1e-3,
        "{after:?} vs {expected:?}"
    );
}

#[gpui::test]
fn a_new_view_size_keeps_the_centre(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, "board.canvas");
    let before = view_centre(&view, &mut vcx);
    // The sidebar closes: the view gets wider (and the window shorter).
    let (w, h) = (WIDTH + 240.0, HEIGHT - 100.0);
    vcx.simulate_resize(size(px(w), px(h)));
    for _ in 0..3 {
        draw(&mut vcx);
    }
    let after = view.read_with(&vcx, |v, _| {
        v.transform().screen_to_world(gv_canvas::Point::new(
            f64::from(w) / 2.0,
            f64::from(h) / 2.0,
        ))
    });
    assert!(near(before, after, 0.5), "{before:?} -> {after:?}");
}

/// Draws a frame after the background work, every job outside it: a
/// highlighting or diagram job run while the frame is drawn fails the
/// test.
fn draw_guarded(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let _frame = crate::reader::code_highlight::tests::FrameGuard::new();
        window.draw(cx).clear(cx);
    });
}

#[gpui::test]
fn card_code_is_highlighted_and_diagrams_drawn_off_the_frame(cx: &mut TestAppContext) {
    use super::cards::Block;
    use crate::inline::BoxContent;
    use crate::reader::{code_highlight, diagram};

    let board = r#"{"nodes":[{"id":"c","type":"text","x":0,"y":0,"width":160,"height":400,
        "text":"```rust\nfn canvas_card_code() -> u8 { 7 }\n```\n\n```mermaid\nflowchart LR\n  CanvasCardA --> CanvasCardB --> CanvasCardC\n```"}]}"#;
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("code.canvas"), board).expect("write");
    let (window, view) = open(cx, &dir.path().join("code.canvas"), dir.path());
    let mut vcx = VisualTestContext::from_window(window, cx);
    let jobs = (
        code_highlight::tests::jobs_run(),
        diagram::tests::jobs_run(),
    );
    for _ in 0..2 {
        draw_guarded(&mut vcx);
    }
    view.update(&mut vcx, |v, cx| {
        v.set_transform(
            Transform {
                zoom: 1.0,
                pan: gv_canvas::Point::new(20.0, 20.0),
            },
            cx,
        );
    });
    for _ in 0..4 {
        draw_guarded(&mut vcx);
    }
    assert_eq!(
        (
            code_highlight::tests::jobs_run(),
            diagram::tests::jobs_run()
        ),
        (jobs.0 + 1, jobs.1 + 1),
        "one job each, off the frame"
    );
    let c = node(&view, &mut vcx, "c");
    view.read_with(&vcx, |v, _| {
        let built = v.cards.built.get(&c).expect("the card was built");
        let keyword = code_highlight::token_of(gv_highlight::TokenClass::Keyword).role();
        let code = built
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Code { block, .. } => Some(block.clone()),
                _ => None,
            })
            .expect("the code block");
        assert!(
            code.borrow()
                .paragraph()
                .items()
                .iter()
                .any(|i| i.style().is_some_and(|s| s.color == keyword)),
            "highlighted with the Reading view's tokens"
        );
        let drawn = built
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Diagram { block, .. } => Some(block.clone()),
                _ => None,
            })
            .expect("the diagram is drawn");
        let drawn = drawn.borrow();
        assert!(matches!(
            drawn.box_content(0),
            Some(BoxContent::Diagram { .. })
        ));
        assert!(drawn.bounds().is_some(), "laid out and painted");
        // Scaled down to the card's width (less its padding).
        let widest = drawn
            .paragraph()
            .items()
            .iter()
            .filter_map(|i| i.inline_box())
            .map(|b| b.width)
            .fold(0.0, f32::max);
        assert!(drawn.widest_box() > 160.0, "{}", drawn.widest_box());
        assert!(widest > 0.0 && widest < 160.0 - 24.0, "{widest}");
    });
}

#[gpui::test]
fn card_html_and_live_blocks_are_drawn_without_jobs(cx: &mut TestAppContext) {
    use super::cards::Block;
    use crate::reader::{code_highlight, diagram};

    let board = r#"{"nodes":[
        {"id":"t","type":"text","x":0,"y":0,"width":240,"height":300,
         "text":"<div>\n<i>canvas html</i>\n</div>\n\n```base\nviews:\n  - type: table\n```"},
        {"id":"f","type":"file","x":300,"y":0,"width":240,"height":300,"file":"q.md"}]}"#;
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("live.canvas"), board).expect("write");
    std::fs::write(
        dir.path().join("q.md"),
        "<details>\n<summary>More</summary>\n\nInside.\n\n</details>\n\n```query\nfoo\n```\n",
    )
    .expect("write");
    let (window, view) = open(cx, &dir.path().join("live.canvas"), dir.path());
    let mut vcx = VisualTestContext::from_window(window, cx);
    view.update(&mut vcx, |v, cx| {
        v.set_transform(
            Transform {
                zoom: 1.0,
                pan: gv_canvas::Point::new(20.0, 20.0),
            },
            cx,
        );
    });
    let jobs = (
        code_highlight::tests::jobs_run(),
        diagram::tests::jobs_run(),
    );
    for _ in 0..4 {
        draw_guarded(&mut vcx);
    }
    assert_eq!(
        (
            code_highlight::tests::jobs_run(),
            diagram::tests::jobs_run()
        ),
        jobs,
        "nothing to run"
    );
    let (t, f) = (node(&view, &mut vcx, "t"), node(&view, &mut vcx, "f"));
    view.read_with(&vcx, |v, _| {
        let card = v.cards.built.get(&t).expect("the text card was built");
        let html = card
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Code { block, label, .. } => Some((block.clone(), label.clone())),
                _ => None,
            })
            .expect("the HTML block");
        assert_eq!(html.1.as_deref(), Some("html"));
        assert_eq!(
            html.0.borrow().paragraph().text(),
            "<div>\n<i>canvas html</i>\n</div>"
        );
        assert!(html.0.borrow().bounds().is_some(), "laid out and painted");
        assert!(
            card.blocks
                .iter()
                .any(|b| matches!(b, Block::Live { title, .. } if title.as_ref() == "Base"))
        );
        let note = v.cards.built.get(&f).expect("the note preview was built");
        assert!(
            note.blocks
                .iter()
                .any(|b| matches!(b, Block::Live { title, .. } if title.as_ref() == "Query: foo"))
        );
        assert!(note.blocks.iter().any(|b| matches!(
            b,
            Block::Text {
                marker: Some(super::Marker::Disclosure { open: false }),
                ..
            }
        )));
        assert!(
            note.blocks.iter().all(|b| !matches!(
                b,
                Block::Text { block, .. } if block.borrow().paragraph().text() == "Inside."
            )),
            "a closed <details> hides its content"
        );
    });
}
