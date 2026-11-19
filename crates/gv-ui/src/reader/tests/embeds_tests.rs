// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live blocks in the Reading view, in headless windows holding only the
//! view: a canvas embed drawn as shapes (also when its canvas has a card
//! of itself and of the embedding note), bases hosted and listing the
//! embedding note, query blocks listing their results, clicks that open
//! files, the scroll anchor while a block's content lands, and profiles
//! without live blocks, and audio embeds with their player (on a silent
//! output that never opens a device).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use gpui::{ScrollDelta, ScrollEvent};
use gv_render_tree::{ItemKind, LIVE_EMBED_HEIGHT, NodeKind};
use gv_vault::content::{ContentOptions, ContentSnapshot, DiskSource};
use gv_vault::{IndexConfig, PathIndex};

use super::*;
use crate::bases_view::BasesEvent;
use crate::reader::embeds::Shown;
use crate::reader::{Disposition, LinkRequest, ReaderEvent};

/// A vault folder (with `.obsidian/`) holding `files`.
fn vault(files: &[(&str, &str)]) -> tempfile::TempDir {
    let mut all: Vec<(&str, &str)> = vec![(".obsidian/app.json", "{}")];
    all.extend_from_slice(files);
    temp(&all)
}

/// The content index of the vault at `root`.
fn snapshot(root: &Path) -> ContentSnapshot {
    let paths = PathIndex::build(root, IndexConfig::default())
        .expect("index")
        .0;
    let options = ContentOptions::for_profile(gv_profile::ProfileId::Obsidian);
    let source = DiskSource::new(root, &options);
    ContentSnapshot::build(paths, options, &source, &AtomicBool::new(false)).expect("content")
}

/// A window showing only a Reading view of `file` in `profile`, with the
/// links it asks to follow.
struct View {
    vcx: VisualTestContext,
    reader: Entity<Reader>,
    followed: Arc<Mutex<Vec<LinkRequest>>>,
}

impl View {
    fn open(cx: &mut TestAppContext, file: &Path, profile: gv_profile::ProfileId) -> Self {
        Self::open_with(cx, file, profile, None)
    }

    /// [`View::open`] with `pdf` as the PDF engine.
    fn open_with(
        cx: &mut TestAppContext,
        file: &Path,
        profile: gv_profile::ProfileId,
        pdf: Option<Arc<dyn crate::export::PdfEngine>>,
    ) -> Self {
        let file = file.to_path_buf();
        let followed = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&followed);
        let (window, reader) = cx.update(|cx| {
            crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
            // Audio players never open a real device here.
            let (backend, _) = gv_media::audio::NullBackend::new(gv_media::audio::OutputFormat {
                sample_rate: 48_000,
                channels: 2,
            });
            crate::audio::set_engine(cx, crate::audio::AudioEngine::new(backend));
            crate::export::set_pdf_engine(cx, pdf);
            let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(900.0), px(600.0)));
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            };
            gpui::open_window(options, cx, |_, cx| {
                let reader = cx.new(|cx| {
                    let mut r = Reader::new(cx);
                    r.set_profile(Arc::new(gv_profile::compile_builtin(profile)), cx);
                    r.open(file, None, cx);
                    r
                });
                cx.subscribe(&reader, move |_, e: &ReaderEvent, _| {
                    if let ReaderEvent::FollowLink(r) = e {
                        seen.lock().expect("lock").push(r.clone());
                    }
                })
                .detach();
                reader
            })
            .expect("test window opens")
        });
        let mut vcx = VisualTestContext::from_window(window, cx);
        for _ in 0..4 {
            draw(&mut vcx);
        }
        Self {
            vcx,
            reader,
            followed,
        }
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            draw(&mut self.vcx);
        }
    }

    fn st(&mut self) -> Rc<RefCell<State>> {
        state(&self.reader, &mut self.vcx)
    }

    /// What the live block whose source starts with `prefix` shows, its
    /// item's painted box (top, bottom) and its index.
    fn live(&mut self, prefix: &str) -> (Shown, (f32, f32), usize) {
        let st = self.st();
        let st = st.borrow();
        let doc = st.doc.as_ref().expect("loaded");
        let at = doc.text.find(prefix).expect("prefix");
        let g = st
            .geometry
            .items
            .iter()
            .find(|g| doc.tree.items()[g.ix].source.start == at)
            .expect("painted");
        let NodeKind::Live(live) = &doc.tree.node_of(g.ix).expect("node").kind else {
            panic!("not a live block");
        };
        let shown = st.cache.live.get(live.key).cloned().expect("asked for");
        let gap = doc.tree.items()[g.ix].gap_before;
        (shown, (g.top + gap, g.bottom), g.ix)
    }

    /// Runs the window until the live block at `prefix` shows what `cond`
    /// accepts (the decoder runs on its own thread: real time too).
    fn wait_live(&mut self, prefix: &str, what: &str, cond: impl Fn(&Shown) -> bool) -> Shown {
        let start = std::time::Instant::now();
        loop {
            self.vcx
                .executor()
                .advance_clock(std::time::Duration::from_millis(50));
            self.settle();
            let (shown, _, _) = self.live(prefix);
            if cond(&shown) {
                return shown;
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "timed out waiting for {what}: {shown:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn followed(&self) -> Vec<LinkRequest> {
        self.followed.lock().expect("lock").clone()
    }
}

const CANVAS: &str = r#"{
  "nodes": [
    {"id": "a", "type": "text", "text": "Hello", "x": 0, "y": 0, "width": 200, "height": 100},
    {"id": "b", "type": "file", "file": "Plan.canvas", "x": 300, "y": 0, "width": 200, "height": 100},
    {"id": "c", "type": "file", "file": "Doc.md", "x": 0, "y": 200, "width": 200, "height": 100}
  ],
  "edges": [{"id": "e", "fromNode": "a", "toNode": "b"}]
}"#;

#[gpui::test]
fn a_canvas_embed_is_drawn_as_shapes_and_opens_the_canvas(cx: &mut TestAppContext) {
    // The canvas holds a card of itself and of the note embedding it: drawn
    // as shapes, neither is opened, so there is nothing to recurse into.
    let dir = vault(&[
        ("Doc.md", "Intro.\n\n![[Plan.canvas]]\n\nAfter.\n"),
        ("Plan.canvas", CANVAS),
    ]);
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
    );
    let (shown, (top, bottom), _) = v.live("![[Plan.canvas]]");
    let Shown::Canvas(doc) = shown else {
        panic!("not drawn: {shown:?}");
    };
    assert_eq!(crate::canvas_view::preview_size(&doc), (3, 1));
    // As tall as reserved: nothing moved when it landed.
    assert!(
        (bottom - top - LIVE_EMBED_HEIGHT).abs() < 10.0,
        "{top} {bottom}"
    );
    // A click on the shapes opens the canvas.
    let at = point(px(450.0), px(bottom - 60.0));
    v.vcx.simulate_click(at, Modifiers::none());
    v.settle();
    let followed = v.followed();
    assert_eq!(followed.len(), 1, "{followed:?}");
    assert_eq!(&*followed[0].destination, "Plan.canvas");
    assert_eq!(followed[0].disposition, Disposition::InPlace);
    // The copy of the whole document has the embed's line.
    let text = v
        .reader
        .read_with(&v.vcx, |r, _| r.plain_text())
        .expect("text");
    assert!(text.contains("Plan.canvas"), "{text:?}");
}

#[gpui::test]
fn a_missing_canvas_says_so(cx: &mut TestAppContext) {
    let dir = vault(&[("Doc.md", "![[Gone.canvas]]\n")]);
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
    );
    let (shown, (top, bottom), _) = v.live("![[Gone.canvas]]");
    let Shown::Notice(why) = shown else {
        panic!("{shown:?}");
    };
    assert!(why.contains("not found"), "{why}");
    assert!(bottom - top < LIVE_EMBED_HEIGHT / 2.0);
}

const BASE: &str = "filters: 'file.hasTag(\"book\")'\nviews:\n  - type: table\n    name: All\n  - type: list\n    name: Shelf\n";

#[gpui::test]
fn bases_are_hosted_with_the_embedding_note_as_this(cx: &mut TestAppContext) {
    // The base lists the note that embeds it (it has the tag): a row, not
    // a recursion.
    let doc = format!(
        "#book\n\n```base\n{BASE}```\n\n![[Books.base#Shelf]]\n\n{}",
        (0..60)
            .map(|i| format!("Paragraph {i}.\n\n"))
            .collect::<String>()
    );
    let dir = vault(&[
        ("Doc.md", &doc),
        ("Books.base", BASE),
        ("Dune.md", "#book Dune.\n"),
        ("Other.md", "Nothing.\n"),
    ]);
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
    );
    let content = snapshot(dir.path());
    v.reader
        .update(&mut v.vcx, |r, cx| r.set_content(Some(content), cx));
    v.settle();
    let (fence, (top, bottom), _) = v.live("```base");
    let Shown::Base(fence) = fence else {
        panic!("{fence:?}");
    };
    assert!((bottom - top - LIVE_EMBED_HEIGHT).abs() < 10.0);
    let (embed, _, _) = v.live("![[Books.base#Shelf]]");
    let Shown::Base(embed) = embed else {
        panic!("{embed:?}");
    };
    let (this, rows) = fence.read_with(&v.vcx, |b, _| {
        (
            b.this().map(str::to_owned),
            b.result()
                .map(|r| r.rows().map(|r| r.path.to_string()).collect::<Vec<_>>())
                .unwrap_or_default(),
        )
    });
    assert_eq!(this.as_deref(), Some("Doc.md"));
    let mut rows = rows;
    rows.sort();
    assert_eq!(rows, ["Doc.md", "Dune.md"]);
    // `#Shelf` picks the view.
    assert_eq!(embed.read_with(&v.vcx, |b, _| b.view_index()), 1);
    // The wheel over a base is the base's: the document does not scroll.
    let host = {
        let st = v.st();
        let st = st.borrow();
        assert!(!st.geometry.hosts.is_empty());
        st.geometry.hosts[0]
    };
    let before = v.st().borrow().engine.scroll_top();
    let at = host.center();
    v.vcx.simulate_mouse_move(at, None, Modifiers::none());
    v.vcx.simulate_event(ScrollEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(0.0), px(-80.0))),
        ..Default::default()
    });
    v.settle();
    assert_eq!(v.st().borrow().engine.scroll_top(), before);
    // Outside it (beside the text column), it does.
    let below = point(px(40.0), px(300.0));
    v.vcx.simulate_mouse_move(below, None, Modifiers::none());
    v.vcx.simulate_event(ScrollEvent {
        position: below,
        delta: ScrollDelta::Pixels(point(px(0.0), px(-80.0))),
        ..Default::default()
    });
    v.settle();
    assert!(v.st().borrow().engine.scroll_top() > before);
    // A file the base opens is followed as a link from the vault's root.
    let path = dir.path().join("Dune.md");
    fence.update(&mut v.vcx, |_, cx| {
        cx.emit(BasesEvent::OpenFile {
            path,
            fragment: None,
            disposition: Disposition::NewTab,
        });
    });
    v.settle();
    let followed = v.followed();
    assert_eq!(followed.len(), 1, "{followed:?}");
    assert_eq!(&*followed[0].destination, "Dune.md");
    assert_eq!(followed[0].disposition, Disposition::NewTab);
    // A new snapshot keeps the views (same text) and lists the files again.
    let content = snapshot(dir.path());
    v.reader
        .update(&mut v.vcx, |r, cx| r.set_content(Some(content), cx));
    v.settle();
    let (again, _, _) = v.live("```base");
    let Shown::Base(again) = again else {
        panic!("{again:?}");
    };
    assert_eq!(again.entity_id(), fence.entity_id());
}

#[gpui::test]
fn a_query_lists_its_results_and_opens_them(cx: &mut TestAppContext) {
    let dir = vault(&[
        ("Doc.md", "Before.\n\n```query\napple\n```\n\nAfter.\n"),
        ("Fruit.md", "An apple a day.\n"),
        ("Pie.md", "Apple pie.\n\nMore apple.\n"),
        ("Other.md", "Pears.\n"),
    ]);
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
    );
    // Without the vault's content: it says so.
    let (shown, _, _) = v.live("```query");
    assert!(matches!(shown, Shown::Notice(_)), "{shown:?}");
    let content = snapshot(dir.path());
    v.reader
        .update(&mut v.vcx, |r, cx| r.set_content(Some(content), cx));
    v.settle();
    let (shown, (top, bottom), _) = v.live("```query");
    let Shown::Query(q) = shown else {
        panic!("{shown:?}");
    };
    let mut names: Vec<&str> = q.files.iter().map(|f| f.path.as_str()).collect();
    names.sort_unstable();
    // The document itself has the word (in its query).
    assert_eq!(names, ["Doc.md", "Fruit.md", "Pie.md"]);
    assert_eq!(q.count(), "3 notes");
    // The box holds its rows: the notes with their lines.
    let line = crate::metrics::text::PREVIEW_BODY.line;
    #[allow(clippy::cast_precision_loss)]
    let rows = q.rows.len() as f32;
    assert!(
        bottom - top >= rows * line,
        "{} rows in {}",
        rows,
        bottom - top
    );
    // A click on the first note's row opens it.
    let first = q.files[0].path.as_str().to_owned();
    let at = point(
        px(450.0),
        px(top + line + 2.0 * crate::reader::embeds::PAD + line / 2.0),
    );
    v.vcx.simulate_click(at, Modifiers::none());
    v.settle();
    let followed = v.followed();
    assert_eq!(followed.len(), 1, "{followed:?}");
    assert_eq!(&*followed[0].destination, first);
    assert_eq!(followed[0].at, None);
    // A click on its context line opens it at that line.
    assert_eq!(q.rows[1], crate::reader::embeds::QueryRow::Line(0, 0));
    let at = point(
        px(450.0),
        px(top + 2.0 * line + 2.0 * crate::reader::embeds::PAD + line / 2.0),
    );
    v.vcx.simulate_click(at, Modifiers::none());
    v.settle();
    let followed = v.followed();
    assert_eq!(followed.len(), 2, "{followed:?}");
    assert_eq!(&*followed[1].destination, first);
    assert_eq!(followed[1].at, Some(q.files[0].snippets[0].range.start));
    // Find in page sees the query.
    let text = v
        .reader
        .read_with(&v.vcx, |r, _| r.plain_text())
        .expect("text");
    assert!(text.contains("apple"), "{text:?}");
}

#[gpui::test]
fn content_landing_above_the_top_does_not_move_the_view(cx: &mut TestAppContext) {
    let notes: Vec<(String, String)> = (0..8)
        .map(|i| (format!("N{i}.md"), format!("apple {i}\n\nmore apple\n")))
        .collect();
    let body: String = (0..120).map(|i| format!("Paragraph {i}.\n\n")).collect();
    let doc = format!("```query\napple\n```\n\n{body}");
    let mut files: Vec<(&str, &str)> = vec![("Doc.md", &doc)];
    files.extend(notes.iter().map(|(a, b)| (a.as_str(), b.as_str())));
    let dir = vault(&files);
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
    );
    let (_, (top, bottom), ix) = v.live("```query");
    let small = bottom - top;
    // Scrolled so that the block is just above the top of the view.
    v.st().borrow_mut().engine.scroll_by(bottom + 40.0);
    v.settle();
    let first = |v: &mut View| {
        v.reader.read_with(&v.vcx, |r, _| {
            r.painted_items()
                .into_iter()
                .find(|(_, y)| *y >= 0.0)
                .expect("an item on screen")
        })
    };
    let (at, y) = first(&mut v);
    let content = snapshot(dir.path());
    v.reader
        .update(&mut v.vcx, |r, cx| r.set_content(Some(content), cx));
    v.settle();
    // The block grew...
    let grown = {
        let st = v.st();
        let st = st.borrow();
        let doc = st.doc.as_ref().expect("loaded");
        let NodeKind::Live(live) = &doc.tree.node_of(ix).expect("node").kind else {
            panic!("not live");
        };
        assert!(matches!(st.cache.live.get(live.key), Some(Shown::Query(_))));
        st.cache.live.height(live.key).expect("measured")
    };
    assert!(grown > small + 50.0, "{small} -> {grown}");
    // ...and what was at the top is still there.
    let (at2, y2) = first(&mut v);
    assert_eq!(at, at2);
    assert!((y - y2).abs() < 0.5, "{y} -> {y2}");
}

#[gpui::test]
fn profiles_without_note_embeds_show_code(cx: &mut TestAppContext) {
    let dir = temp(&[(
        "Doc.md",
        "```query\napple\n```\n\n```base\nviews: []\n```\n\n![[Plan.canvas]]\n",
    )]);
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::CommonMark,
    );
    let st = v.st();
    let st = st.borrow();
    let doc = st.doc.as_ref().expect("loaded");
    assert!(
        !doc.tree
            .items()
            .iter()
            .any(|it| matches!(it.kind, ItemKind::Live { .. }))
    );
    assert_eq!(st.cache.live.len(), 0);
}

#[gpui::test]
fn media_embeds_are_cards_that_open_outside(cx: &mut TestAppContext) {
    let audio = "x".repeat(3_400);
    let doc = "![[Talk.mp3]]\n\n![[Paper.pdf#page=3]]\n\n\
               <iframe src=\"https://www.youtube.com/embed/abc\" title=\"A talk\"></iframe>\n\n\
               ![[Gone.mp4]]\n\nAfter.\n";
    let dir = vault(&[
        ("Doc.md", doc),
        ("Talk.mp3", audio.as_str()),
        ("Paper.pdf", "%PDF-1.7\n"),
    ]);
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
    );
    let card = |shown: &Shown| match shown.card() {
        Some(m) => (**m).clone(),
        None => panic!("not a card: {shown:?}"),
    };
    let (shown, (top, bottom), _) = v.live("![[Talk.mp3]]");
    let talk = card(&shown);
    assert_eq!(talk.title, "Talk.mp3");
    assert_eq!(talk.detail, "MP3 audio \u{b7} 3.4 KB");
    assert_eq!(talk.action, "Open externally");
    assert!(matches!(shown, Shown::Audio(..)), "{shown:?}");
    assert!(bottom - top < LIVE_EMBED_HEIGHT / 2.0, "{top} {bottom}");
    let (shown, _, _) = v.live("![[Paper.pdf");
    let paper = card(&shown);
    assert_eq!(paper.detail, "PDF \u{b7} page 3 \u{b7} 9 bytes");
    assert_eq!(paper.action, "Open in PDF viewer");
    let (shown, (f_top, f_bottom), _) = v.live("<iframe");
    let frame = card(&shown);
    assert_eq!(frame.title, "A talk");
    assert_eq!(frame.detail, "YouTube video, not loaded.");
    let (shown, (g_top, g_bottom), _) = v.live("![[Gone.mp4]]");
    let Shown::Notice(why) = shown else {
        panic!("{shown:?}");
    };
    assert!(why.contains("not found"), "{why}");

    // A click on a card (on an audio card's header) follows its action; a
    // notice follows nothing.
    for (y0, y1) in [(top, top + 20.0), (f_top, f_bottom), (g_top, g_bottom)] {
        v.vcx
            .simulate_click(point(px(450.0), px((y0 + y1) / 2.0)), Modifiers::none());
        v.settle();
    }
    let followed = v.followed();
    assert_eq!(followed.len(), 2, "{followed:?}");
    assert_eq!(&*followed[0].destination, "Talk.mp3");
    assert_eq!(followed[0].kind, gv_model::LinkKind::Wikilink);
    assert_eq!(
        &*followed[1].destination,
        "https://www.youtube.com/watch?v=abc"
    );
    assert_eq!(followed[1].kind, gv_model::LinkKind::Inline);
    // Copy sees the embed line and the HTML.
    let text = v
        .reader
        .read_with(&v.vcx, |r, _| r.plain_text())
        .expect("text");
    assert!(text.contains("Talk.mp3"), "{text:?}");
    assert!(text.contains("<iframe"), "{text:?}");
}

/// A PDF embed shows the page its `#page=N` names, as high as
/// `#height=N` says, drawn by the PDF engine; a click opens the PDF
/// viewer at that page; a page the engine cannot draw leaves the card,
/// saying why.
#[gpui::test]
fn pdf_embeds_show_their_page(cx: &mut TestAppContext) {
    use crate::file_viewer::pdf_tests::FakePdf;
    use crate::reader::media_card::PDF_EMBED_HEIGHT;

    let doc =
        "![[Paper.pdf#page=9]]\n\n![[Paper.pdf#page=3&height=100]]\n\n![[Paper.pdf]]\n\nAfter.\n";
    let dir = vault(&[("Doc.md", doc), ("Paper.pdf", "%PDF-1.7\n")]);
    let engine = Arc::new(FakePdf::pages(&[(612.0, 792.0); 4]));
    let mut v = View::open_with(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
        Some(Arc::clone(&engine) as Arc<dyn crate::export::PdfEngine>),
    );
    let shown = v.wait_live("![[Paper.pdf#page=3", "the page", |s| {
        matches!(s, Shown::PdfPage(_))
    });
    let Shown::PdfPage(third) = shown else {
        unreachable!()
    };
    assert_eq!(third.page, 2);
    assert!((third.height - 100.0).abs() < f32::EPSILON);
    assert_eq!(third.card.action, "Open in PDF viewer");
    let (_, (top, bottom), _) = v.live("![[Paper.pdf#page=3");
    assert!(bottom - top >= 100.0, "{top} {bottom}");
    // Without an anchor: the first page, at the default height.
    let Shown::PdfPage(first) = v.wait_live("![[Paper.pdf]]", "the first page", |s| {
        matches!(s, Shown::PdfPage(_))
    }) else {
        unreachable!()
    };
    assert_eq!(first.page, 0);
    assert!((first.height - PDF_EMBED_HEIGHT).abs() < f32::EPSILON);
    // The engine drew pages fitted to the embeds' boxes.
    let fitted = engine.fitted.lock().expect("lock").clone();
    assert!(fitted.contains(&(2, 1600, 200)), "{fitted:?}");
    // No page 9: the card says why.
    let shown = v.wait_live("![[Paper.pdf#page=9", "the card", |s| {
        s.card().is_some_and(|c| c.detail.contains("no page 9"))
    });
    assert!(matches!(shown, Shown::Media(_)), "{shown:?}");

    // A click opens the PDF viewer at the page.
    v.vcx.simulate_click(
        point(px(450.0), px((top + bottom) / 2.0)),
        Modifiers::none(),
    );
    v.settle();
    let followed = v.followed();
    assert_eq!(followed.len(), 1, "{followed:?}");
    assert_eq!(&*followed[0].destination, "Paper.pdf#page=3");

    // Drawn again, nothing changed: the pages are kept, not drawn again,
    // and a page that could not be drawn is not asked for again.
    let drawn = |e: &FakePdf| e.fitted.lock().expect("lock").len();
    let before = drawn(&engine);
    v.reader.update(&mut v.vcx, |r, cx| {
        r.refresh_live_targets();
        cx.notify();
    });
    v.settle();
    assert_eq!(drawn(&engine), before);
}

#[gpui::test]
fn a_pdf_embed_is_drawn_again_sharper_when_zoomed_in(cx: &mut TestAppContext) {
    use crate::file_viewer::pdf_tests::FakePdf;

    let doc = "![[Paper.pdf#page=2&height=100]]\n\nAfter.\n";
    let dir = vault(&[("Doc.md", doc), ("Paper.pdf", "%PDF-1.7\n")]);
    let engine = Arc::new(FakePdf::pages(&[(612.0, 792.0); 2]));
    let mut v = View::open_with(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
        Some(Arc::clone(&engine) as Arc<dyn crate::export::PdfEngine>),
    );
    let prefix = "![[Paper.pdf";
    v.wait_live(prefix, "the page", |s| matches!(s, Shown::PdfPage(_)));
    let fitted = |e: &FakePdf| e.fitted.lock().expect("lock").clone();
    assert_eq!(fitted(&engine), [(1, 1600, 200)]);
    // The test window has a scale factor of 2. At 150 %: drawn again for
    // three device pixels per px.
    v.vcx.update(|_, cx| crate::zoom::set(cx, 150));
    let shown = v.wait_live(
        prefix,
        "the sharper page",
        |s| matches!(s, Shown::PdfPage(p) if p.scale > 2.5),
    );
    let Shown::PdfPage(page) = shown else {
        unreachable!()
    };
    assert!((page.scale - 3.0).abs() < 0.01, "{}", page.scale);
    assert_eq!(fitted(&engine), [(1, 1600, 200), (1, 2400, 300)]);
    // Zoomed out again: the sharper page stays.
    for percent in [100, 50] {
        v.vcx.update(|_, cx| crate::zoom::set(cx, percent));
        v.settle();
    }
    assert_eq!(fitted(&engine).len(), 2);
    assert!(matches!(v.live(prefix).0, Shown::PdfPage(_)));
}

#[gpui::test]
fn html_audio_resolves_next_to_the_note_and_network_paths_are_blocked(cx: &mut TestAppContext) {
    // Audio and video are HTML blocks when the start tag ends its line.
    let doc = "<audio controls src=\"rec/memo.ogg\">\n</audio>\n\n\
               <video src=\"//cdn.example.com/v/clip.mp4\" controls>\n</video>\n\n\
               <audio src=\"\\\\server\\share\\a.mp3\">\n</audio>\n";
    let dir = vault(&[("Doc.md", doc), ("rec/memo.ogg", "ogg")]);
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
    );
    let (shown, _, _) = v.live("<audio controls");
    let Some(memo) = shown.card().cloned() else {
        panic!("{shown:?}");
    };
    assert_eq!(memo.title, "memo.ogg");
    assert_eq!(memo.detail, "OGG audio \u{b7} 3 bytes");
    assert_eq!(
        memo.open,
        Some(("rec/memo.ogg".into(), gv_model::LinkKind::Inline))
    );
    let (shown, _, _) = v.live("<video");
    let Shown::Media(clip) = shown else {
        panic!("{shown:?}");
    };
    assert_eq!(clip.title, "clip.mp4");
    assert_eq!(clip.detail, "Video from cdn.example.com, not loaded.");
    assert_eq!(clip.action, "Open in browser");
    let (shown, _, _) = v.live("<audio src");
    let Shown::Notice(why) = shown else {
        panic!("{shown:?}");
    };
    assert!(why.contains("network share"), "{why}");
}

/// A 48 kHz stereo 16-bit WAV of `frames` frames of a quiet ramp.
fn wav(frames: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(frames * 4);
    for f in 0..frames {
        let v = i16::try_from(f % 1000).expect("small") + 1;
        data.extend_from_slice(&v.to_le_bytes());
        data.extend_from_slice(&(-v).to_le_bytes());
    }
    let len = u32::try_from(data.len()).expect("small");
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&48_000u32.to_le_bytes());
    out.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&data);
    out
}

fn player_of(shown: &Shown) -> Entity<crate::audio::AudioPlayerView> {
    match shown {
        Shown::Audio(_, p) => p.clone(),
        other => panic!("no player: {other:?}"),
    }
}

#[gpui::test]
fn audio_embeds_host_a_player_that_loads_on_first_play(cx: &mut TestAppContext) {
    use crate::audio::PlayerState;
    let dir = vault(&[("Doc.md", "![[Tone.wav]]\n\nAfter.\n")]);
    std::fs::write(dir.path().join("Tone.wav"), wav(48_000 * 4)).expect("writes");
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
    );
    let (shown, _, _) = v.live("![[Tone.wav]]");
    let card = shown.card().cloned().expect("card");
    assert_eq!(card.title, "Tone.wav");
    assert_eq!(card.action, "Open externally");
    let player = player_of(&shown);
    // Nothing is read before the first play.
    assert!(!player.read_with(&v.vcx, |p, _| p.is_loaded()));
    assert!(!v.st().borrow().geometry.hosts.is_empty());

    // A click on play (inside the reader) loads and plays.
    let play = v.vcx.debug_bounds("audio-play").expect("drawn").center();
    assert!(v.st().borrow().in_host(play));
    v.vcx.simulate_click(play, Modifiers::none());
    let state = |v: &View| player.read_with(&v.vcx, |p, _| p.state().clone());
    let start = std::time::Instant::now();
    while state(&v) != PlayerState::Playing {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            state(&v)
        );
        v.vcx
            .executor()
            .advance_clock(std::time::Duration::from_millis(50));
        v.settle();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(player.read_with(&v.vcx, |p, _| p.is_loaded()));
    assert!(v.followed().is_empty(), "the click stayed with the player");
    // The click focused it: Space is its own key in the reader.
    v.vcx.simulate_keystrokes("space");
    v.settle();
    assert_eq!(state(&v), PlayerState::Paused);
    // The same player stays across a refresh of the live blocks.
    v.reader.update(&mut v.vcx, |r, cx| {
        let items = r.state.borrow_mut().cache.live.refresh(|_| true);
        r.state.borrow_mut().stale.extend(items);
        cx.notify();
    });
    v.settle();
    let (again, _, _) = v.live("![[Tone.wav]]");
    assert_eq!(player_of(&again), player);
}

#[gpui::test]
fn an_unplayable_audio_file_falls_back_to_the_card(cx: &mut TestAppContext) {
    let dir = vault(&[
        ("Doc.md", "![[Broken.mp3]]\n"),
        ("Broken.mp3", "not audio at all"),
    ]);
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
    );
    let (shown, _, _) = v.live("![[Broken.mp3]]");
    assert!(matches!(shown, Shown::Audio(..)), "{shown:?}");
    let play = v.vcx.debug_bounds("audio-play").expect("drawn").center();
    v.vcx.simulate_click(play, Modifiers::none());
    let shown = v.wait_live("![[Broken.mp3]]", "the card", |s| {
        matches!(s, Shown::Media(_))
    });
    let card = shown.card().cloned().expect("card");
    assert_eq!(card.title, "Broken.mp3");
    assert_eq!(card.action, "Open externally");
    assert!(
        card.detail.starts_with("MP3 audio \u{b7} 16 bytes \u{b7} "),
        "{}",
        card.detail
    );
    assert!(v.st().borrow().geometry.hosts.is_empty());
}

#[gpui::test]
fn html_audio_inside_a_note_embed_resolves_next_to_the_embedded_note(cx: &mut TestAppContext) {
    let dir = vault(&[
        ("Doc.md", "![[notes/b]]\n"),
        // `./`: beside the note that holds it (in this profile).
        (
            "notes/b.md",
            "<audio controls src=\"./rec/memo.ogg\">\n</audio>\n",
        ),
        ("notes/rec/memo.ogg", "ogg"),
    ]);
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
    );
    v.settle();
    let st = v.st();
    let st = st.borrow();
    let doc = st.doc.as_ref().expect("loaded");
    let live = doc
        .tree
        .nodes()
        .iter()
        .find_map(|n| match &n.kind {
            NodeKind::Live(l) if l.html.is_some() => Some(Arc::clone(l)),
            _ => None,
        })
        .expect("the embedded audio block");
    assert_eq!(live.host.as_deref(), Some("notes/b.md"));
    let shown = st.cache.live.get(live.key).cloned().expect("asked for");
    let Some(memo) = shown.card().cloned() else {
        panic!("{shown:?}");
    };
    // Beside the embedded note, not beside the document.
    assert_eq!(memo.title, "memo.ogg");
    assert_eq!(memo.detail, "OGG audio \u{b7} 3 bytes");
}

#[gpui::test]
fn a_live_block_inside_a_note_embed_opens_from_the_embedded_note(cx: &mut TestAppContext) {
    let dir = vault(&[
        ("Doc.md", "![[notes/b]]\n"),
        ("notes/b.md", "![[Plan.canvas]]\n"),
        ("notes/Plan.canvas", CANVAS),
    ]);
    let mut v = View::open(
        cx,
        &dir.path().join("Doc.md"),
        gv_profile::ProfileId::Obsidian,
    );
    v.settle();
    let st = v.st();
    let st = st.borrow();
    let doc = st.doc.as_ref().expect("loaded");
    let live = doc
        .tree
        .nodes()
        .iter()
        .find_map(|n| match &n.kind {
            NodeKind::Live(l) => Some(Arc::clone(l)),
            _ => None,
        })
        .expect("the embedded canvas block");
    assert_eq!(live.host.as_deref(), Some("notes/b.md"));
    let request = crate::reader::embeds::open_request(&live, None, None, Disposition::InPlace)
        .expect("a request");
    assert_eq!(&*request.destination, "Plan.canvas");
    // Followed from the embedded note, where the canvas is.
    assert_eq!(request.from.as_deref(), Some("notes/b.md"));
}
