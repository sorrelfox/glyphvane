// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Images and diagrams you interact with, in headless windows: a click on
//! an image opens it in the image viewer, `data:` images, an image file
//! that changes on disk gets its new size, a diagram's zoom buttons, and
//! links in diagrams.

use std::path::PathBuf;

use gv_render_tree::InlineRole;
use image::{ImageEncoder as _, Rgba, RgbaImage};

use super::*;
use crate::inline::BoxContent;
use crate::reader::items::is_diagram;
use crate::reader::state::{ControlKind, Hover};

fn settle(vcx: &mut VisualTestContext) {
    for _ in 0..4 {
        draw(vcx);
    }
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = RgbaImage::from_pixel(w, h, Rgba([30, 120, 200, 255]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&img, w, h, image::ExtendedColorType::Rgba8)
        .expect("png");
    out
}

/// Standard base64, for `data:` URLs.
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                s.push(char::from(A[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// A folder with `doc.md` and other files.
fn folder(doc: &str, files: &[(&str, Vec<u8>)]) -> (tempfile::TempDir, PathBuf) {
    let dir = temp(&[("doc.md", doc)]);
    for (name, bytes) in files {
        std::fs::write(dir.path().join(name), bytes).expect("write");
    }
    let file = dir.path().join("doc.md");
    (dir, file)
}

/// The image boxes drawn: their window bounds and what they show.
fn image_boxes(st: &State) -> Vec<(Bounds<Pixels>, Option<BoxContent>)> {
    let doc = st.doc.as_ref().expect("loaded");
    let mut out = Vec::new();
    for g in &st.geometry.items {
        let Some(block) = &g.text else {
            continue;
        };
        let Some(content) = doc.tree.item_inline(g.ix) else {
            continue;
        };
        let b = block.borrow();
        let (Some(layout), Some(painted)) = (b.layout(), b.bounds()) else {
            continue;
        };
        for (piece, span) in content.spans.iter().enumerate() {
            if !matches!(span.role, InlineRole::Image(_)) {
                continue;
            }
            let Some(bx) = b
                .paragraph()
                .items()
                .get(piece)
                .and_then(|p| p.inline_box())
            else {
                continue;
            };
            let r = layout.item_rects(piece)[0];
            out.push((
                Bounds::new(
                    painted.origin + point(px(r.x), px(r.y)),
                    size(px(r.width), px(r.height)),
                ),
                b.box_content(bx.id).cloned(),
            ));
        }
    }
    out
}

fn tab_names(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Vec<String> {
    workspace.read_with(vcx, |ws, _| {
        ws.tabs()
            .iter()
            .map(|t| {
                t.path()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect()
    })
}

#[gpui::test]
fn a_click_on_an_image_opens_it_in_the_image_viewer(cx: &mut TestAppContext) {
    let doc = "Text first.\n\n![pic](pic.png)\n\n[![linked](pic.png)](other.md)\n";
    let files = [
        ("pic.png", png(120, 60)),
        ("other.md", b"# Other\n".to_vec()),
    ];
    let (_dir, file) = folder(doc, &files);
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    settle(&mut vcx);
    let (image, linked) = {
        let s = st.borrow();
        let boxes = image_boxes(&s);
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        (boxes[0].0.center(), boxes[1].0.center())
    };
    // The pointer over the image: a hand; an image inside a link is the
    // link's.
    vcx.simulate_mouse_move(image, None, Modifiers::none());
    draw(&mut vcx);
    assert!(
        matches!(&st.borrow().hover, Hover::Target(t) if t.image),
        "{:?}",
        st.borrow().hover
    );
    vcx.simulate_mouse_move(linked, None, Modifiers::none());
    draw(&mut vcx);
    assert!(
        matches!(&st.borrow().hover, Hover::Link(_)),
        "{:?}",
        st.borrow().hover
    );
    // A click opens the image in a tab of its own, in the viewer.
    vcx.simulate_click(image, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(tab_names(&workspace, &mut vcx), ["doc.md", "pic.png"]);
    assert_eq!(file_of(&workspace, &mut vcx), "pic.png");
    let viewer = workspace.read_with(&vcx, |ws, _| ws.active_tab().viewer().is_some());
    assert!(viewer, "the image viewer shows it");
}

#[gpui::test]
fn data_images_show_and_other_data_urls_do_not(cx: &mut TestAppContext) {
    let url = format!("data:image/png;base64,{}", base64(&png(30, 20)));
    let doc = format!(
        "![inline]({url})\n\n![svg](data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=)\n\n![broken](data:image/png;base64,AAAA)\n"
    );
    let (_dir, file) = folder(&doc, &[]);
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    settle(&mut vcx);
    let s = st.borrow();
    let boxes = image_boxes(&s);
    // The PNG at its size, decoded; the broken one a card's box; the SVG
    // only its alt text.
    assert_eq!(boxes.len(), 2, "{boxes:?}");
    let zoom = s.doc.as_ref().expect("loaded").tree.options().styles.zoom();
    let (b, content) = &boxes[0];
    assert!((f32::from(b.size.width) - 30.0 * zoom).abs() < 0.5, "{b:?}");
    assert!(
        matches!(content, Some(BoxContent::Image(_))),
        "decoded: {content:?}"
    );
    assert!(
        (f32::from(boxes[1].0.size.width) - crate::reader::media::CARD_WIDTH * zoom).abs() < 0.5
    );
    let cards: Vec<String> = s
        .cache
        .image_cards
        .values()
        .flatten()
        .map(|c| c.message.clone())
        .collect();
    assert_eq!(cards.len(), 1, "{cards:?}");
    // Data images never offer a click to the viewer.
    drop(s);
    let at = boxes[0].0.center();
    assert!(st.borrow().image_at(at).is_none());
}

#[gpui::test]
fn an_image_that_changes_on_disk_gets_its_new_size(cx: &mut TestAppContext) {
    let doc = "![pic](pic.png)\n";
    let (dir, file) = folder(doc, &[("pic.png", png(40, 20))]);
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    settle(&mut vcx);
    let zoom = st
        .borrow()
        .doc
        .as_ref()
        .expect("loaded")
        .tree
        .options()
        .styles
        .zoom();
    let width = |st: &Rc<RefCell<State>>| f32::from(image_boxes(&st.borrow())[0].0.size.width);
    assert!((width(&st) - 40.0 * zoom).abs() < 0.5);
    // Replaced by a wider picture, with a later time.
    let path = dir.path().join("pic.png");
    std::fs::write(&path, png(80, 20)).expect("write");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|f| f.set_modified(later))
        .expect("touch");
    settle(&mut vcx);
    settle(&mut vcx);
    assert!(
        (width(&st) - 80.0 * zoom).abs() < 0.5,
        "{} for 80 × 20",
        width(&st)
    );
}

/// The first diagram item: its index and the box's window bounds.
fn diagram_box(st: &State) -> (usize, Bounds<Pixels>, f32) {
    let doc = st.doc.as_ref().expect("loaded");
    let ix = doc
        .tree
        .items()
        .iter()
        .position(|i| is_diagram(&i.kind))
        .expect("a diagram");
    let g = st
        .geometry
        .items
        .iter()
        .find(|g| g.ix == ix)
        .expect("drawn");
    let b = g.text.as_ref().expect("its block").borrow();
    let layout = b.layout().expect("laid out");
    let r = layout.item_rects(0)[0];
    let origin = b.bounds().expect("painted").origin;
    let natural = match b.box_content(0) {
        Some(BoxContent::Diagram { diagram, .. }) => diagram.width,
        other => panic!("the diagram box: {other:?}"),
    };
    (
        ix,
        Bounds::new(
            origin + point(px(r.x), px(r.y)),
            size(px(r.width), px(r.height)),
        ),
        natural,
    )
}

fn zoom_control(st: &State, step: i8) -> Option<Point<Pixels>> {
    st.geometry
        .controls
        .iter()
        .find(|c| matches!(c.kind, ControlKind::DiagramZoom { step: s, .. } if s == step))
        .map(|c| c.bounds.center())
}

#[gpui::test]
fn a_diagram_zooms_with_its_buttons(cx: &mut TestAppContext) {
    let doc =
        "# D\n\n```mermaid\nflowchart TD\n  zoom_buttons_a --> zoom_buttons_b\n```\n\nafter\n";
    let (_dir, file) = folder(doc, &[]);
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    settle(&mut vcx);
    let (_, b, natural) = diagram_box(&st.borrow());
    let zoom = st
        .borrow()
        .doc
        .as_ref()
        .expect("loaded")
        .tree
        .options()
        .styles
        .zoom();
    let fitted = f32::from(b.size.width);
    assert!(
        (fitted - natural * zoom).abs() < 0.5,
        "a small diagram at its size"
    );
    // No buttons until the pointer is over it.
    assert!(zoom_control(&st.borrow(), 1).is_none());
    vcx.simulate_mouse_move(b.center(), None, Modifiers::none());
    settle(&mut vcx);
    let plus = zoom_control(&st.borrow(), 1).expect("zoom in");
    assert!(zoom_control(&st.borrow(), 0).is_none(), "no Fit at 100 %");
    vcx.simulate_click(plus, Modifiers::none());
    settle(&mut vcx);
    let (_, b, _) = diagram_box(&st.borrow());
    assert!(
        (f32::from(b.size.width) - natural * zoom * 1.25).abs() < 0.5,
        "{} for {natural}",
        f32::from(b.size.width)
    );
    // Further in, past the column: it keeps its size and scrolls sideways.
    for _ in 0..5 {
        vcx.simulate_mouse_move(b.center(), None, Modifiers::none());
        settle(&mut vcx);
        let plus = zoom_control(&st.borrow(), 1).expect("zoom in");
        vcx.simulate_click(plus, Modifiers::none());
        settle(&mut vcx);
    }
    let (ix, b, _) = diagram_box(&st.borrow());
    assert!(
        (f32::from(b.size.width) - natural * zoom * 4.0).abs() < 0.5,
        "the largest step: {}",
        f32::from(b.size.width)
    );
    // Fit goes back; zoom out makes it smaller than its size.
    vcx.simulate_mouse_move(b.center(), None, Modifiers::none());
    settle(&mut vcx);
    let fit = zoom_control(&st.borrow(), 0).expect("Fit when zoomed");
    vcx.simulate_click(fit, Modifiers::none());
    settle(&mut vcx);
    let (_, b, _) = diagram_box(&st.borrow());
    assert!((f32::from(b.size.width) - fitted).abs() < 0.5);
    vcx.simulate_mouse_move(b.center(), None, Modifiers::none());
    settle(&mut vcx);
    let minus = zoom_control(&st.borrow(), -1).expect("zoom out");
    vcx.simulate_click(minus, Modifiers::none());
    settle(&mut vcx);
    let (ix2, b, _) = diagram_box(&st.borrow());
    assert_eq!(ix, ix2);
    assert!(
        (f32::from(b.size.width) - fitted * 0.75).abs() < 0.5,
        "{} for {fitted}",
        f32::from(b.size.width)
    );
}

#[gpui::test]
fn links_in_diagrams_are_followed(cx: &mut TestAppContext) {
    let doc = "```mermaid\nflowchart LR\n  diagram_link_a[Go] --> diagram_link_b[Other]\n  click diagram_link_a href \"b.md\"\n  class diagram_link_b internal-link;\n```\n";
    let files = [("b.md", b"# B\n".to_vec()), ("Other.md", b"# O\n".to_vec())];
    let (_dir, file) = folder(doc, &files);
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    settle(&mut vcx);
    // Where the links are: their rectangles in the drawn diagram.
    let (go, other) = {
        let s = st.borrow();
        let (_, b, natural) = diagram_box(&s);
        let doc = s.doc.as_ref().expect("loaded");
        let ix = doc
            .tree
            .items()
            .iter()
            .position(|i| is_diagram(&i.kind))
            .expect("diagram");
        let block = s
            .geometry
            .items
            .iter()
            .find(|g| g.ix == ix)
            .and_then(|g| g.text.clone())
            .expect("block");
        let block = block.borrow();
        let Some(BoxContent::Diagram { diagram, .. }) = block.box_content(0) else {
            panic!("rendered");
        };
        assert_eq!(diagram.links.len(), 2, "{:?}", diagram.links);
        let scale = f32::from(b.size.width) / natural;
        let at = |r: gv_diagram::Rect| {
            b.origin
                + point(
                    px((r.x + r.width / 2.0) * scale),
                    px((r.y + r.height / 2.0) * scale),
                )
        };
        (at(diagram.links[0].rect), at(diagram.links[1].rect))
    };
    let hit = st
        .borrow()
        .diagram_link_at(other)
        .expect("the internal link");
    assert_eq!(&*hit.destination, "Other");
    assert_eq!(hit.kind, gv_model::LinkKind::Wikilink);
    vcx.simulate_mouse_move(go, None, Modifiers::none());
    draw(&mut vcx);
    assert!(matches!(&st.borrow().hover, Hover::Target(t) if &*t.destination == "b.md"));
    // A click follows it through the link policy: a note in the same tab.
    vcx.simulate_click(go, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");
}
