// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Diagrams and images in the Reading view, in headless windows: jobs run
//! on the background executor (a diagram, image or highlighting job that
//! runs inside a frame fails the test), diagrams repaint in a new theme
//! without being laid out again, fallback cards and *Render anyway*,
//! images fitted to their box, animations, image cards, EXIF orientation,
//! and `<picture>` and `#gh-*-mode-only` by the theme.
//!
//! Each test uses diagram sources of its own: the diagram engine's cache
//! is shared by the whole process.

use std::fmt::Write as _;
use std::path::PathBuf;

use gpui::{Hsla, RenderImage};
use gv_render_tree::{ItemKind, NodeKind};
use image::{ImageEncoder as _, Rgba, RgbaImage};

use super::*;
use crate::inline::{BoxContent, Palette, Token};
use crate::reader::diagram::tests::{jobs_run as diagram_jobs, pending_draws};
use crate::reader::items::is_diagram;
use crate::reader::media::tests::jobs_run as image_jobs;
use crate::reader::state::ControlKind;
use crate::theme::{PreferredLight, ThemeChoice, ThemeId};

fn settle(vcx: &mut VisualTestContext) {
    for _ in 0..4 {
        draw(vcx);
    }
}

fn png(w: u32, h: u32, exif: Option<Vec<u8>>) -> Vec<u8> {
    let img = RgbaImage::from_pixel(w, h, Rgba([200, 40, 40, 255]));
    let mut out = Vec::new();
    let mut enc = image::codecs::png::PngEncoder::new(&mut out);
    if let Some(e) = exif {
        enc.set_exif_metadata(e).expect("exif");
    }
    enc.write_image(&img, w, h, image::ExtendedColorType::Rgba8)
        .expect("png");
    out
}

fn gif(frames: usize, w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
        enc.set_repeat(image::codecs::gif::Repeat::Infinite)
            .expect("repeat");
        for i in 0..frames {
            let c = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]][i % 3];
            let f = image::Frame::from_parts(
                RgbaImage::from_pixel(w, h, Rgba(c)),
                0,
                0,
                image::Delay::from_numer_denom_ms(80, 1),
            );
            enc.encode_frame(f).expect("frame");
        }
    }
    out
}

/// A TIFF block with one IFD entry: Orientation = `o`.
fn exif_orientation(o: u16) -> Vec<u8> {
    let mut e = b"MM\0\x2a\0\0\0\x08".to_vec();
    e.extend_from_slice(&1u16.to_be_bytes());
    e.extend_from_slice(&0x0112u16.to_be_bytes());
    e.extend_from_slice(&3u16.to_be_bytes());
    e.extend_from_slice(&1u32.to_be_bytes());
    e.extend_from_slice(&o.to_be_bytes());
    e.extend_from_slice(&[0, 0]);
    e.extend_from_slice(&0u32.to_be_bytes());
    e
}

/// A folder with `doc.md` and binary files.
fn folder(doc: &str, files: &[(&str, Vec<u8>)]) -> (tempfile::TempDir, PathBuf) {
    let dir = temp(&[("doc.md", doc)]);
    for (name, bytes) in files {
        std::fs::write(dir.path().join(name), bytes).expect("write");
    }
    let file = dir.path().join("doc.md");
    (dir, file)
}

/// The first diagram item's id and cached block.
fn diagram_block(st: &State) -> (gv_render_tree::ItemId, Option<BoxContent>) {
    let doc = st.doc.as_ref().expect("loaded");
    let item = doc
        .tree
        .items()
        .iter()
        .find(|i| is_diagram(&i.kind))
        .expect("a diagram item");
    let content = st
        .cache
        .cached(item.id)
        .and_then(|b| b.borrow().box_content(0).cloned());
    (item.id, content)
}

fn set_theme(vcx: &mut VisualTestContext, choice: ThemeChoice) {
    vcx.update(|_, cx| {
        crate::appearance::Appearance::set_preference(cx, choice, PreferredLight::Light);
    });
}

fn colour(id: ThemeId, t: Token) -> Hsla {
    Palette::of(id).color(t)
}

#[gpui::test]
fn a_diagram_renders_off_the_frame_and_repaints_in_a_new_theme(cx: &mut TestAppContext) {
    let doc = "# D\n\n```mermaid\nflowchart TD\n  repaint_in_theme --> B\n```\n";
    let (_dir, file) = folder(doc, &[]);
    let (window, workspace) = open(cx, &file);
    let (jobs, pending) = (diagram_jobs(), pending_draws());
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    settle(&mut vcx);
    // Drawn pending once or more, rendered by one job off the frame.
    assert!(pending_draws() > pending, "drawn pending first");
    assert_eq!(diagram_jobs(), jobs + 1);
    let st = state(&reader, &mut vcx);
    let (id, content) = diagram_block(&st.borrow());
    let Some(BoxContent::Diagram { diagram, .. }) = content else {
        panic!("the diagram box: {content:?}");
    };
    assert!(
        diagram.family.starts_with("flowchart"),
        "{}",
        diagram.family
    );
    assert!(!diagram.items.is_empty());
    let built = st.borrow().cache.built;
    let _ = crate::inline::diagram::tests::take_painted();
    draw(&mut vcx);
    let light = crate::inline::diagram::tests::take_painted();
    assert!(
        light.contains(&colour(ThemeId::Light, Token::AccentTint)),
        "node fills in the light accent tint"
    );
    // Dark: the same block repaints in the dark tokens, nothing is laid
    // out again and no job runs.
    set_theme(&mut vcx, ThemeChoice::Dark);
    settle(&mut vcx);
    let dark = crate::inline::diagram::tests::take_painted();
    assert!(dark.contains(&colour(ThemeId::Dark, Token::AccentTint)));
    assert!(!dark.contains(&colour(ThemeId::Light, Token::AccentTint)));
    assert_eq!(st.borrow().cache.built, built, "no block made again");
    assert!(st.borrow().cache.cached(id).is_some());
    assert_eq!(diagram_jobs(), jobs + 1);
}

#[gpui::test]
fn a_bad_diagram_shows_its_card_above_the_source(cx: &mut TestAppContext) {
    let src = "sequenceDiagram\n  Alice->>Bob: card_above_source\n  Bob-->>: \n  ??? !!!";
    let doc = format!("```mermaid\n{src}\n```\n\n```mermaid\nnot_a_diagram card_note\n```\n");
    let (_dir, file) = folder(&doc, &[]);
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    settle(&mut vcx);
    let st = state(&reader, &mut vcx);
    let at = {
        let s = st.borrow();
        let tree = &s.doc.as_ref().expect("loaded").tree;
        let ids: Vec<_> = tree
            .items()
            .iter()
            .filter(|i| is_diagram(&i.kind))
            .map(|i| i.id)
            .collect();
        assert_eq!(ids.len(), 2);
        // The error card, then the note: both above their source.
        let card = s.cache.diagram_frames.get(&ids[0]).expect("a frame");
        let card = card.as_ref().expect("a card");
        assert_eq!(card.title(), "Diagram error");
        assert!(!card.render_anyway);
        assert!(
            s.cache
                .diagram_frames
                .get(&ids[1])
                .expect("a frame")
                .is_none()
        );
        for (id, first) in [
            (ids[0], "Diagram error: "),
            (ids[1], "Mermaid: diagram type"),
        ] {
            let block = s.cache.cached(id).expect("laid out");
            let text = block.borrow().paragraph().text().to_owned();
            assert!(text.starts_with(first), "{text:?}");
            assert!(text.contains("\n\n"), "the source below: {text:?}");
        }
        s.geometry
            .controls
            .iter()
            .find(|c| matches!(c.kind, ControlKind::CopySource { .. }))
            .expect("Copy source")
            .bounds
            .center()
    };
    vcx.simulate_click(at, Modifiers::none());
    let copied = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(copied.as_deref(), Some(src));
}

#[gpui::test]
fn render_anyway_renders_a_diagram_over_the_caps(cx: &mut TestAppContext) {
    let mut src = String::from("flowchart LR\n");
    let mut i = 0;
    while src.len() <= 100 << 10 {
        let _ = writeln!(src, "  %% render anyway padding line {i}");
        i += 1;
    }
    src.push_str("  render_anyway_ui --> B");
    let doc = format!("```mermaid\n{src}\n```\n");
    let (_dir, file) = folder(&doc, &[]);
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    settle(&mut vcx);
    let st = state(&reader, &mut vcx);
    let at = {
        let s = st.borrow();
        let (id, _) = diagram_block(&s);
        let card = s.cache.diagram_frames.get(&id).cloned().flatten();
        let card = card.expect("a card");
        assert_eq!(card.title(), "Diagram too large to render");
        assert!(card.render_anyway);
        s.geometry
            .controls
            .iter()
            .find(|c| matches!(c.kind, ControlKind::RenderAnyway { .. }))
            .expect("Render anyway")
            .bounds
            .center()
    };
    vcx.simulate_click(at, Modifiers::none());
    settle(&mut vcx);
    let s = st.borrow();
    let (id, content) = diagram_block(&s);
    assert!(!s.cache.diagram_frames.contains_key(&id), "no card now");
    assert!(
        matches!(content, Some(BoxContent::Diagram { .. })),
        "{content:?}"
    );
}

/// The image box contents of the items laid out, in order.
fn images(st: &State) -> Vec<(f32, f32, Option<std::sync::Arc<RenderImage>>)> {
    let doc = st.doc.as_ref().expect("loaded");
    let mut out = Vec::new();
    for item in doc.tree.items() {
        let Some(block) = st.cache.cached(item.id) else {
            continue;
        };
        let b = block.borrow();
        for piece in b.paragraph().items() {
            let Some(bx) = piece.inline_box() else {
                continue;
            };
            if bx.kind != gv_inline::BoxKind::Image {
                continue;
            }
            let image = match b.box_content(bx.id) {
                Some(BoxContent::Image(gpui::ImageSource::Render(i))) => Some(i.clone()),
                _ => None,
            };
            out.push((bx.width, bx.ascent + bx.descent, image));
        }
    }
    out
}

#[gpui::test]
fn images_fit_their_box_animate_and_follow_exif(cx: &mut TestAppContext) {
    let doc = "![big](big.png)\n\n![anim](anim.gif)\n\n![turned](turned.png)\n";
    let files = [
        ("big.png", png(2000, 1000, None)),
        ("anim.gif", gif(3, 16, 8)),
        ("turned.png", png(40, 20, Some(exif_orientation(6)))),
    ];
    let (_dir, file) = folder(doc, &files);
    let jobs = image_jobs();
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    settle(&mut vcx);
    assert_eq!(
        image_jobs(),
        jobs + 3,
        "one decode per image, off the frame"
    );
    let st = state(&reader, &mut vcx);
    let s = st.borrow();
    let scale = s.cache.view.scale;
    let max = s
        .doc
        .as_ref()
        .expect("loaded")
        .tree
        .options()
        .metrics
        .image_max_width;
    let shown = images(&s);
    assert_eq!(shown.len(), 3);
    // The wide image: its box is the column's image width, and it is
    // decoded at the box's device pixels.
    let (w, h, img) = &shown[0];
    assert!(
        (w - max).abs() < 0.01 && (h - max / 2.0).abs() < 0.01,
        "{w}x{h}"
    );
    let img = img.as_ref().expect("decoded");
    let size = img.size(0);
    assert_eq!(
        (i32::from(size.width), i32::from(size.height)),
        (
            (max * scale).ceil() as i32,
            (max / 2.0 * scale).ceil() as i32
        )
    );
    // The animation keeps its frames (motion is not reduced).
    let anim = shown[1].2.as_ref().expect("decoded");
    assert_eq!(anim.frame_count(), 3);
    assert_eq!(crate::inline::animation_frame(anim, 0), 0);
    assert_eq!(crate::inline::animation_frame(anim, 85), 1);
    assert_eq!(crate::inline::animation_frame(anim, 170), 2);
    // EXIF orientation 6 turns 40 × 20 into 20 × 40.
    let zoom = s.doc.as_ref().expect("loaded").tree.options().styles.zoom();
    let (w, h, _) = shown[2];
    assert!(
        (w - 20.0 * zoom).abs() < 0.01 && (h - 40.0 * zoom).abs() < 0.01,
        "{w}x{h}"
    );
    assert_eq!(s.cache.images.ready(), 3);
    assert_eq!(s.cache.images.pending(), 0);
}

/// Records the files handed to the system.
#[derive(Clone, Default)]
struct Launched(Rc<RefCell<Vec<PathBuf>>>);

impl crate::platform::Launcher for Launched {
    fn reveal(&self, _: &Path, _: &mut gpui::App) {}

    fn open_with_default_app(&self, path: &Path, _: &mut gpui::App) {
        self.0.borrow_mut().push(path.to_path_buf());
    }

    fn open_url(&self, _: &str, _: &mut gpui::App) {}
}

#[gpui::test]
fn images_that_cannot_be_shown_get_a_card(cx: &mut TestAppContext) {
    let mut broken = png(8, 8, None);
    broken.truncate(40);
    // An AVIF header: recognised, not decoded here.
    let mut avif = vec![0, 0, 0, 0x1c];
    avif.extend_from_slice(b"ftypavif\0\0\0\0avifmif1miaf");
    avif.resize(256, 0);
    let doc = "![broken](broken.png)\n\n![avif](photo.avif)\n\n![missing](nope.png) ![web](https://example.org/a.png)\n";
    let (_dir, file) = folder(doc, &[("broken.png", broken), ("photo.avif", avif)]);
    let launched = Launched::default();
    let l = launched.clone();
    cx.update(|cx| crate::platform::set_launcher(cx, l));
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    settle(&mut vcx);
    let st = state(&reader, &mut vcx);
    let at = {
        let s = st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        let cards: Vec<_> = doc
            .tree
            .items()
            .iter()
            .filter_map(|i| s.cache.image_cards.get(&i.id))
            .flatten()
            .collect();
        assert_eq!(cards.len(), 3, "{cards:?}");
        assert!(cards[0].message.starts_with("The image could not be read"));
        assert!(cards[0].open.is_none());
        assert_eq!(cards[1].message, "AVIF images cannot be shown here.");
        assert!(
            cards[1]
                .open
                .as_ref()
                .is_some_and(|p| p.ends_with("photo.avif"))
        );
        // A remote image waits for approval (the default mode is Ask):
        // its placeholder card; nothing is fetched.
        assert_eq!(
            cards[2].message,
            "web: Remote image from example.org, not loaded."
        );
        assert!(cards[2].open.is_none());
        // A missing image shows its alt text.
        let last = doc.tree.items().last().expect("an item");
        let text = s
            .cache
            .cached(last.id)
            .expect("laid out")
            .borrow()
            .paragraph()
            .text()
            .to_owned();
        assert!(text.contains("missing"), "{text:?}");
        s.geometry
            .controls
            .iter()
            .find(|c| matches!(c.kind, ControlKind::OpenExternally { .. }))
            .expect("Open externally")
            .bounds
            .center()
    };
    vcx.simulate_click(at, Modifiers::none());
    let opened = launched.0.borrow().clone();
    assert_eq!(opened.len(), 1);
    assert!(opened[0].ends_with("photo.avif"));
}

#[gpui::test]
fn pictures_and_github_fragments_follow_the_theme(cx: &mut TestAppContext) {
    let doc = "<picture>\n  <source media=\"(prefers-color-scheme: dark)\" srcset=\"dark.png\">\n  <img alt=\"Logo\" src=\"light.png\">\n</picture>\n\n![d](only.png#gh-dark-mode-only) ![l](only.png#gh-light-mode-only)\n";
    let files = [
        ("light.png", png(30, 10, None)),
        ("dark.png", png(10, 30, None)),
        ("only.png", png(12, 12, None)),
    ];
    let (_dir, file) = folder(doc, &files);
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    settle(&mut vcx);
    let st = state(&reader, &mut vcx);
    let zoom = |st: &State| {
        st.doc
            .as_ref()
            .expect("loaded")
            .tree
            .options()
            .styles
            .zoom()
    };
    let sizes = |st: &State| {
        images(st)
            .iter()
            .map(|(w, h, i)| ((w / zoom(st)).round(), (h / zoom(st)).round(), i.is_some()))
            .collect::<Vec<_>>()
    };
    {
        let s = st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        assert!(matches!(doc.tree.nodes()[0].kind, NodeKind::Picture(_)));
        assert_eq!(doc.tree.items()[0].kind, ItemKind::Picture);
        // Light: the light source, and only the light-mode image takes room.
        assert_eq!(
            sizes(&s),
            [(30.0, 10.0, true), (0.0, 0.0, false), (12.0, 12.0, true)]
        );
    }
    set_theme(&mut vcx, ThemeChoice::Dark);
    settle(&mut vcx);
    let s = st.borrow();
    assert_eq!(
        sizes(&s),
        [(10.0, 30.0, true), (12.0, 12.0, true), (0.0, 0.0, false)]
    );
}

/// Frame timing with the platform's real text system: a document of 20
/// diagrams and 20 images, scrolled after every job landed.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "timing run: cargo test -p gv-ui --profile measure -- --ignored timing_diagrams"]
fn timing_diagrams_and_images() {
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, Instant};

    use gpui::{PlatformTextSystem, TestDispatcher};

    fn platform_text_system() -> Arc<dyn PlatformTextSystem> {
        static TEXT: OnceLock<Arc<dyn PlatformTextSystem>> = OnceLock::new();
        TEXT.get_or_init(|| gpui::platform::current_platform(true).text_system())
            .clone()
    }

    let mut doc = String::from("# Diagrams and images\n\n");
    let mut files = Vec::new();
    for i in 0..20 {
        let _ = write!(
            doc,
            "Paragraph {i} before a diagram, long enough to wrap over a line or two of \
             the text column in the reading view.\n\n```mermaid\nflowchart LR\n  \
             a{i}[Start {i}] --> b{i}{{Choice}}\n  b{i} -->|yes| c{i}[Done]\n  \
             b{i} -->|no| d{i}[Again] --> a{i}\n```\n\n![photo {i}](img{i}.png)\n\n"
        );
        files.push((format!("img{i}.png"), png(1200, 800, None)));
    }
    let dir = temp(&[("doc.md", &doc)]);
    for (name, bytes) in &files {
        std::fs::write(dir.path().join(name), bytes).expect("write");
    }
    let dispatcher = TestDispatcher::new(0);
    let mut cx = TestAppContext::build_with_text_system(
        dispatcher.clone(),
        Some("media_timing"),
        platform_text_system(),
    );
    {
        let (window, workspace) = open(&mut cx, &dir.path().join("doc.md"));
        let mut vcx = VisualTestContext::from_window(window, &cx);
        draw(&mut vcx);
        let reader = reader(&workspace, &mut vcx);
        let st = state(&reader, &mut vcx);
        // Land every job: scroll through once, letting jobs run.
        let t = Instant::now();
        for _ in 0..200 {
            st.borrow_mut().engine.scroll_by(120.0);
            draw(&mut vcx);
        }
        st.borrow_mut().engine.scroll_to_y(0.0);
        draw(&mut vcx);
        eprintln!(
            "20 diagrams + 20 images: first pass with jobs {:.0} ms",
            t.elapsed().as_secs_f64() * 1e3
        );
        let mut frames: Vec<Duration> = Vec::new();
        for step in [24.0f32, 400.0] {
            for _ in 0..300 {
                {
                    let mut s = st.borrow_mut();
                    s.engine.scroll_by(step);
                }
                let t = Instant::now();
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                frames.push(t.elapsed());
                if st.borrow().engine.placed().is_empty() {
                    break;
                }
            }
            eprintln!(
                "20 diagrams + 20 images: scroll {step} px/frame (layout and scene, no GPU): {}",
                crate::reader::bench::percentiles(&frames)
            );
            frames.clear();
            st.borrow_mut().engine.scroll_to_y(0.0);
            draw(&mut vcx);
        }
        let s = st.borrow();
        eprintln!(
            "decoded images kept: {}; diagram jobs {}; image jobs {}",
            s.cache.images.ready(),
            diagram_jobs(),
            image_jobs()
        );
    }
    cx.run_until_parked();
    cx.update(|cx| cx.quit());
    cx.run_until_parked();
    drop(cx);
    dispatcher.drain_tasks();
}
