// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live blocks in the Reading view, in headless windows holding only the
//! view: a canvas embed drawn as shapes (also when its canvas has a card
//! of itself and of the embedding note), bases hosted and listing the
//! embedding note, query blocks listing their results, clicks that open
//! files, the scroll anchor while a block's content lands, and profiles
//! without live blocks.

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
        let file = file.to_path_buf();
        let followed = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&followed);
        let (window, reader) = cx.update(|cx| {
            crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
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
    let card = |shown: &Shown| match shown {
        Shown::Media(m) => (*m).clone(),
        other => panic!("not a card: {other:?}"),
    };
    let (shown, (top, bottom), _) = v.live("![[Talk.mp3]]");
    let talk = card(&shown);
    assert_eq!(talk.title, "Talk.mp3");
    assert_eq!(talk.detail, "MP3 audio \u{b7} 3.4 KB");
    assert_eq!(talk.action, "Open externally");
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

    // A click on a card follows its action; a notice follows nothing.
    for (y0, y1) in [(top, bottom), (f_top, f_bottom), (g_top, g_bottom)] {
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
    let Shown::Media(memo) = shown else {
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
