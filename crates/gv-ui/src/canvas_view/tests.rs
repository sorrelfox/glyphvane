// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Canvas view in headless GPUI windows: loading and drawing a fixture
//! canvas, culling, pan and zoom, keyboard navigation, opening cards,
//! problems, reduced motion, the minimap, `view.zoom_to_fit`, and what
//! assistive technology is told.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

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
