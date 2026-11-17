// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Reading view in headless GPUI windows (GPUI's no-op text system:
//! 0.6 em per character): loading, the anchor across width changes, link
//! clicks, selection and copy, the code buttons, the scrollbar and link
//! statuses. Timing runs with the platform's real text system are
//! `#[ignore]`d (`cargo test -p gv-ui --release -- --ignored --nocapture`).

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Modifiers, MouseButton, Pixels, Point,
    TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_inline::Bias;
use gv_render_tree::{InlineRole, LinkStatus};

use super::state::{ButtonKind, State};
use super::{Copy as CopyAction, CopyAsMarkdown, LoadStatus, Reader, SelectAll};
use crate::chrome::WindowChrome;
use crate::metrics::size::{SCROLLBAR_THUMB, document};
use crate::metrics::spacing::scrollbar;
use crate::shell::{OpenTarget, VaultStatus, Workspace};
use crate::{AppSettings, ThemeChoice, ToggleSidebar};

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 800.0;
/// The document area of a window `HEIGHT` tall: the window minus the title
/// row and the status bar.
const HEIGHT_OF_DOCUMENT: f32 =
    HEIGHT - crate::metrics::size::TITLE_ROW - crate::metrics::size::STATUS_BAR;

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

fn drawn(window: AnyWindowHandle, cx: &mut TestAppContext) -> VisualTestContext {
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    // The first frame measures; the second paints with settled heights.
    draw(&mut vcx);
    vcx
}

fn reader(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Entity<Reader> {
    workspace
        .read_with(cx, |ws, _| ws.active_tab().reader().cloned())
        .expect("the active tab has a reader")
}

fn state(reader: &Entity<Reader>, cx: &mut VisualTestContext) -> Rc<RefCell<State>> {
    reader.read_with(cx, |r, _| r.state.clone())
}

fn file_of(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> String {
    workspace.read_with(cx, |ws, _| {
        ws.active_tab()
            .path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

fn temp(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, text) in files {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("folder");
        }
        std::fs::write(path, text).expect("write");
    }
    dir
}

/// The window position of source offset `offset`'s caret (its top-left,
/// nudged into the line).
fn caret_point(st: &State, offset: usize) -> Point<Pixels> {
    let doc = st.doc.as_ref().expect("loaded");
    for g in &st.geometry.items {
        let node = doc.tree.node_of(g.ix).expect("node");
        if !(node.source.start <= offset && offset <= node.source.end) {
            continue;
        }
        let Some(block) = &g.text else {
            continue;
        };
        let caret = block
            .borrow()
            .caret_bounds(offset - node.source.start, Bias::Left)
            .expect("painted");
        return caret.origin + point(px(0.5), px(2.0));
    }
    panic!("offset {offset} is not on screen");
}

/// The middle of link `link`'s text in the item that holds `needle`.
fn link_point(st: &State, needle: &str) -> Point<Pixels> {
    let doc = st.doc.as_ref().expect("loaded");
    let offset = doc.text.find(needle).expect("needle in the document");
    for g in &st.geometry.items {
        let node = doc.tree.node_of(g.ix).expect("node");
        if !(node.source.start <= offset && offset < node.source.end) {
            continue;
        }
        let content = node.inline().expect("inline content");
        let piece = content
            .spans
            .iter()
            .position(|s| s.link.is_some() && s.role == InlineRole::Text)
            .expect("a link");
        let block = g.text.as_ref().expect("text").borrow();
        let layout = block.layout().expect("laid out");
        let origin = block.bounds().expect("painted").origin;
        let r = layout.item_rects(piece)[0];
        return origin + point(px(r.x + r.width / 2.0), px(r.y + r.height / 2.0));
    }
    panic!("{needle:?} is not on screen");
}

fn clipboard(vcx: &mut VisualTestContext) -> Option<String> {
    vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
}

const SAMPLE: &str = "# Sample title

A first paragraph with `code` and a [link](other.md).

```rust
fn main() {
    println!(\"hi\");
}
```

- one
- two

> A quote.

---

Last paragraph.
";

#[gpui::test]
fn loads_and_renders_the_items(cx: &mut TestAppContext) {
    let dir = temp(&[("a.md", SAMPLE)]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    assert_eq!(
        reader.read_with(&vcx, |r, _| r.status().clone()),
        LoadStatus::Ready
    );
    let st = state(&reader, &mut vcx);
    let st = st.borrow();
    let doc = st.doc.as_ref().expect("loaded");
    let kinds: Vec<&str> = doc.tree.items().iter().map(|i| i.kind.name()).collect();
    assert_eq!(
        kinds,
        [
            "heading",
            "paragraph",
            "code",
            "paragraph",
            "paragraph",
            "paragraph",
            "thematic-break",
            "paragraph"
        ]
    );
    // Everything fits: every item is placed, the text ones are painted.
    assert_eq!(st.geometry.items.len(), kinds.len());
    for g in &st.geometry.items {
        if let Some(block) = &g.text {
            assert!(block.borrow().bounds().is_some(), "item {} painted", g.ix);
        }
    }
    // The column: 680 wide, centred; the first block 22 below the top.
    let bounds = st.geometry.bounds;
    let (x0, column) = st.geometry.column;
    assert_eq!(column, document::COLUMN_MAX);
    assert!(
        (x0 - f32::from(bounds.left()) - (f32::from(bounds.size.width) - column) / 2.0).abs() < 1.0
    );
    let first = &st.geometry.items[0];
    assert!((first.top - f32::from(bounds.top()) - document::PAD_TOP).abs() < 0.01);
    let heading = first.text.as_ref().expect("heading text").borrow();
    assert_eq!(heading.paragraph().text(), "Sample title");
    assert!((f32::from(heading.bounds().expect("painted").left()) - x0).abs() < 0.01);
    // The code block's head has its two buttons; its text is the code.
    assert_eq!(st.geometry.buttons.len(), 2);
    let code = st.geometry.items[2]
        .text
        .as_ref()
        .expect("code text")
        .borrow();
    assert_eq!(
        code.paragraph().text(),
        "fn main() {\n    println!(\"hi\");\n}"
    );
    // Without the window's vault, the local link is pending.
    let content = doc
        .tree
        .node_of(1)
        .and_then(|n| n.inline())
        .expect("inline");
    assert_eq!(content.links[0].status, LinkStatus::Pending);
    // No scrollbar when everything fits.
    assert!(st.geometry.thumb.is_none());
}

#[gpui::test]
fn a_missing_file_shows_why(cx: &mut TestAppContext) {
    let dir = temp(&[]);
    let (window, workspace) = open(cx, &dir.path().join("nope.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    assert!(matches!(
        reader.read_with(&vcx, |r, _| r.status().clone()),
        LoadStatus::Failed(_)
    ));
}

fn long_doc(paragraphs: usize) -> String {
    let mut s = String::from("# Long\n\n");
    for i in 0..paragraphs {
        s.push_str(&format!(
            "Paragraph {i} has enough words to wrap over several lines in the text column, \
             so that a change of width moves its line breaks around and the anchor has work to do.\n\n"
        ));
        if i == 30 {
            s.push_str("## Second\n\nThe target section.\n\n");
        }
    }
    s
}

#[gpui::test]
fn the_top_line_stays_put_when_the_width_changes(cx: &mut TestAppContext) {
    let doc = long_doc(80);
    let dir = temp(&[("a.md", &doc)]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    let st = state(&reader, &mut vcx);
    // Scroll into paragraph 20, two lines and 5 px down.
    {
        let mut s = st.borrow_mut();
        let ix = 21;
        let gap = s.doc.as_ref().expect("loaded").tree.items()[ix].gap_before;
        s.engine.jump_to(ix, gap + 2.0 * 24.0 + 5.0);
    }
    draw(&mut vcx);
    draw(&mut vcx);
    let (source, y) = {
        let s = st.borrow();
        let top = s.geometry.bounds.top();
        let (x0, _) = s.geometry.column;
        let source = s
            .offset_at(point(px(x0 + 1.0), top + px(1.0)))
            .expect("text at the top");
        (source, caret_point(&s, source).y)
    };
    for width in [900.0, 600.0, 1100.0, WIDTH] {
        vcx.simulate_resize(size(px(width), px(HEIGHT)));
        draw(&mut vcx);
        let s = st.borrow();
        let now = caret_point(&s, source).y;
        assert!(
            (now - y).abs() < px(0.5),
            "width {width}: the top line moved from {y:?} to {now:?}"
        );
    }
}

#[gpui::test]
fn a_click_on_a_relative_link_opens_it_in_the_same_tab(cx: &mut TestAppContext) {
    let dir = temp(&[
        (
            "a.md",
            "# A\n\nGo to [the second part](b.md#second) of b.\n",
        ),
        ("b.md", &long_doc(60)),
    ]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    let at = link_point(&st.borrow(), "the second part");
    vcx.simulate_click(at, Modifiers::none());
    draw(&mut vcx);
    draw(&mut vcx);
    assert_eq!(workspace.read_with(&vcx, |ws, _| ws.tabs().len()), 1);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");
    let history = workspace.read_with(&vcx, |ws, _| ws.active_tab().history().to_vec());
    assert_eq!(history.len(), 1);
    assert!(history[0].path.ends_with("a.md"));
    // The same reader shows b.md, with the heading a quarter down.
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    let s = st.borrow();
    let doc = s.doc.as_ref().expect("loaded");
    assert!(doc.path.ends_with("b.md"));
    let heading = doc
        .tree
        .items()
        .iter()
        .position(|it| matches!(it.kind, gv_render_tree::ItemKind::Heading { level: 2 }))
        .expect("the heading");
    let g = s
        .geometry
        .items
        .iter()
        .find(|g| g.ix == heading)
        .expect("the heading is on screen");
    let bounds = s.geometry.bounds;
    let text_top = g.top + doc.tree.items()[heading].gap_before - f32::from(bounds.top());
    let quarter = f32::from(bounds.size.height) / 4.0;
    assert!((text_top - quarter).abs() < 1.0, "{text_top} vs {quarter}");
    drop(s);

    // Back returns to a.md.
    let went = vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.go_back(window, cx)));
    assert!(went);
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
}

#[gpui::test]
fn mod_click_opens_a_new_tab_and_middle_click_a_background_tab(cx: &mut TestAppContext) {
    let dir = temp(&[("a.md", "Go to [b](b.md).\n"), ("b.md", "# B\n")]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    let at = link_point(&st.borrow(), "[b]");
    let secondary = vcx.update(|_, cx| Modifiers::secondary_key(cx));
    vcx.simulate_click(at, secondary);
    draw(&mut vcx);
    let tabs = |vcx: &mut VisualTestContext| {
        workspace.read_with(vcx, |ws, _| {
            ws.tabs()
                .iter()
                .map(|t| {
                    t.path()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(tabs(&mut vcx), ["a.md", "b.md"]);
    assert_eq!(file_of(&workspace, &mut vcx), "b.md");

    // Back in a.md, a middle click opens b.md behind it.
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.open_tab(OpenTarget::new(dir.path().join("a.md")), window, cx)
        });
    });
    draw(&mut vcx);
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    let at = link_point(&st.borrow(), "[b]");
    vcx.simulate_mouse_down(at, MouseButton::Middle, Modifiers::none());
    vcx.simulate_mouse_up(at, MouseButton::Middle, Modifiers::none());
    draw(&mut vcx);
    assert_eq!(tabs(&mut vcx), ["a.md", "b.md", "a.md", "b.md"]);
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
}

#[gpui::test]
fn urls_go_to_the_system_and_blocked_links_go_nowhere(cx: &mut TestAppContext) {
    let dir = temp(&[(
        "a.md",
        "See [the site](https://example.com/page).\n\nNot [this](javascript:alert(1)).\n",
    )]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    {
        let s = st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        let status = |ix: usize| {
            doc.tree
                .node_of(ix)
                .and_then(|n| n.inline())
                .expect("inline")
                .links[0]
                .status
        };
        assert_eq!(status(0), LinkStatus::External);
        assert_eq!(status(1), LinkStatus::Blocked);
    }
    let blocked = link_point(&st.borrow(), "[this]");
    vcx.simulate_click(blocked, Modifiers::none());
    draw(&mut vcx);
    assert_eq!(cx.opened_url(), None);
    let site = link_point(&st.borrow(), "the site");
    vcx.simulate_click(site, Modifiers::none());
    draw(&mut vcx);
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/page"));
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
}

#[gpui::test]
fn fragments_and_lines_scroll_the_document(cx: &mut TestAppContext) {
    let mut text = String::from("Jump to [the end](#second).\n\n");
    text.push_str(&long_doc(60));
    let dir = temp(&[("a.md", &text)]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    let heading_top =
        |st: &State| {
            let doc = st.doc.as_ref().expect("loaded");
            let ix = doc
                .tree
                .items()
                .iter()
                .position(|it| matches!(it.kind, gv_render_tree::ItemKind::Heading { level: 2 }))
                .expect("the heading");
            st.geometry.items.iter().find(|g| g.ix == ix).map(|g| {
                g.top + doc.tree.items()[ix].gap_before - f32::from(st.geometry.bounds.top())
            })
        };
    assert_eq!(
        heading_top(&st.borrow()),
        None,
        "the heading starts off screen"
    );
    let at = link_point(&st.borrow(), "the end");
    vcx.simulate_click(at, Modifiers::none());
    draw(&mut vcx);
    draw(&mut vcx);
    let quarter = HEIGHT_OF_DOCUMENT / 4.0;
    let top = heading_top(&st.borrow()).expect("the heading is on screen");
    assert!((top - quarter).abs() < 40.0, "{top} vs {quarter}");
    // A jump inside the document is history too.
    let history = workspace.read_with(&vcx, |ws, _| ws.active_tab().history().len());
    assert_eq!(history, 1);

    // `PATH:LINE` opens scrolled to that line.
    let line = text[..text.find("## Second").expect("heading")]
        .lines()
        .count() as u32
        + 1;
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| {
            let target = OpenTarget::new(dir.path().join("a.md"))
                .at(crate::TextPosition { line, column: None });
            ws.open_tab(target, window, cx);
        });
    });
    draw(&mut vcx);
    draw(&mut vcx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    let top = heading_top(&st.borrow()).expect("the heading is on screen");
    assert!((top - quarter).abs() < 40.0, "{top} vs {quarter}");
}

#[gpui::test]
fn dragging_selects_across_items_and_copy_takes_the_text(cx: &mut TestAppContext) {
    let text = "# Title\n\nHello brave new world.\n\nSecond paragraph here.\n";
    let dir = temp(&[("a.md", text)]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    let st = state(&reader, &mut vcx);
    let (from, to) = {
        let s = st.borrow();
        let start = text.find("Hello").expect("start");
        let end = text.find(" here").expect("end");
        (caret_point(&s, start), caret_point(&s, end))
    };
    vcx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    vcx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    draw(&mut vcx);
    let selected = reader.read_with(&vcx, |r, _| r.selected_text());
    assert_eq!(
        selected.as_deref(),
        Some("Hello brave new world.\n\nSecond paragraph")
    );
    // The selection is painted in both paragraphs.
    {
        let s = st.borrow();
        let painted: Vec<u32> = s.geometry.items[1..]
            .iter()
            .filter_map(|g| {
                g.text
                    .as_ref()
                    .map(|b| b.borrow().paint_stats().selection_rects)
            })
            .collect();
        assert!(painted.iter().all(|&n| n > 0), "{painted:?}");
    }
    vcx.dispatch_action(CopyAction);
    assert_eq!(
        clipboard(&mut vcx).as_deref(),
        Some("Hello brave new world.\n\nSecond paragraph")
    );
    vcx.dispatch_action(SelectAll);
    vcx.dispatch_action(CopyAction);
    assert_eq!(
        clipboard(&mut vcx).as_deref(),
        Some("Title\n\nHello brave new world.\n\nSecond paragraph here.")
    );
}

#[gpui::test]
fn copy_as_markdown_takes_the_source(cx: &mut TestAppContext) {
    let text = "# Title\r\n\r\n- one with **bold** text\r\n- two [link](b.md) end\r\n";
    let dir = temp(&[("a.md", text)]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    let st = state(&reader, &mut vcx);
    // From inside the bold text to inside the link text.
    let (from, to) = {
        let s = st.borrow();
        let start = text.find("old").expect("start");
        let end = text.find("nk]").expect("end");
        (caret_point(&s, start), caret_point(&s, end))
    };
    vcx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    vcx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    draw(&mut vcx);
    vcx.dispatch_action(CopyAsMarkdown);
    assert_eq!(
        clipboard(&mut vcx).as_deref(),
        Some("- **bold** text\r\n- two [link](b.md)")
    );
    // Plain copy is unchanged.
    vcx.dispatch_action(CopyAction);
    assert_eq!(clipboard(&mut vcx).as_deref(), Some("old text\n- two li"));
    // The whole document, byte for byte.
    vcx.dispatch_action(SelectAll);
    vcx.dispatch_action(CopyAsMarkdown);
    assert_eq!(clipboard(&mut vcx).as_deref(), Some(text));
}

#[gpui::test]
fn the_code_buttons_copy_and_wrap(cx: &mut TestAppContext) {
    let long = "x".repeat(400);
    let doc = format!("```sh\necho {long}\nls\n```\n");
    let dir = temp(&[("a.md", &doc)]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    let (copy, wrap, height) = {
        let s = st.borrow();
        let button = |kind| {
            s.geometry
                .buttons
                .iter()
                .find(|b| b.kind == kind)
                .expect("button")
                .bounds
                .center()
        };
        let g = &s.geometry.items[0];
        assert!(
            g.code.expect("code").overflow > 0.0,
            "the long line overflows"
        );
        (
            button(ButtonKind::Copy),
            button(ButtonKind::Wrap),
            g.bottom - g.top,
        )
    };
    vcx.simulate_click(copy, Modifiers::none());
    assert_eq!(clipboard(&mut vcx), Some(format!("echo {long}\nls")));
    vcx.simulate_click(wrap, Modifiers::none());
    draw(&mut vcx);
    draw(&mut vcx);
    let s = st.borrow();
    let g = &s.geometry.items[0];
    assert!(g.bottom - g.top > height + 20.0, "wrapping adds lines");
    assert_eq!(g.code.expect("code").overflow, 0.0);
}

#[gpui::test]
fn the_scrollbar_shows_and_drags(cx: &mut TestAppContext) {
    let dir = temp(&[("a.md", &long_doc(300))]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let st = state(&reader(&workspace, &mut vcx), &mut vcx);
    let (thumb, bounds) = {
        let s = st.borrow();
        (s.geometry.thumb.expect("a thumb"), s.geometry.bounds)
    };
    assert_eq!(f32::from(thumb.size.width), SCROLLBAR_THUMB);
    assert!((f32::from(bounds.right() - thumb.right()) - scrollbar::INSET_X).abs() < 0.01);
    assert!((f32::from(thumb.top() - bounds.top()) - scrollbar::INSET_Y).abs() < 0.01);
    let before = st.borrow().engine.scroll_top();
    let grab = thumb.center();
    vcx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_move(
        grab + point(px(0.0), px(200.0)),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    vcx.simulate_mouse_up(
        grab + point(px(0.0), px(200.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    draw(&mut vcx);
    let s = st.borrow();
    assert!(
        s.engine.scroll_top() > before + 1000.0,
        "{}",
        s.engine.scroll_top()
    );
    let moved = s.geometry.thumb.expect("a thumb");
    assert!((f32::from(moved.top() - thumb.top()) - 200.0).abs() < 2.0);
}

#[gpui::test]
fn statuses_follow_the_vault_once_it_exists(cx: &mut TestAppContext) {
    let dir = temp(&[
        (".obsidian/app.json", "{}"),
        ("a.md", "[here](b.md) and [gone](missing.md)\n"),
        ("b.md", "# B\n"),
    ]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    let statuses = |vcx: &mut VisualTestContext| {
        let st = state(&reader, vcx);
        let s = st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        let content = doc
            .tree
            .node_of(0)
            .and_then(|n| n.inline())
            .expect("inline");
        content.links.iter().map(|l| l.status).collect::<Vec<_>>()
    };
    assert_eq!(
        statuses(&mut vcx),
        [LinkStatus::Pending, LinkStatus::Pending]
    );
    vcx.dispatch_action(ToggleSidebar);
    draw(&mut vcx);
    let ready = workspace.read_with(&vcx, |ws, cx| ws.vault_status(cx));
    assert_eq!(ready, Some(VaultStatus::Ready));
    draw(&mut vcx);
    assert_eq!(
        statuses(&mut vcx),
        [LinkStatus::Exact, LinkStatus::Unresolved]
    );
}

/// The left edges of the cells of table row items, by row: the caret x
/// of each cell's first character (`cells` gives the needles per row).
fn cell_lefts(st: &State, cells: &[&[&str]]) -> Vec<Vec<f32>> {
    let doc = st.doc.as_ref().expect("loaded");
    let rows: Vec<_> = st
        .geometry
        .items
        .iter()
        .filter(|g| {
            matches!(
                doc.tree.items()[g.ix].kind,
                gv_render_tree::ItemKind::TableRow { .. }
            )
        })
        .collect();
    assert_eq!(rows.len(), cells.len(), "table rows on screen");
    rows.iter()
        .zip(cells)
        .map(|(g, needles)| {
            let item = &doc.tree.items()[g.ix];
            let node = doc.tree.node_of(g.ix).expect("node");
            let block = g.text.as_ref().expect("a row block");
            needles
                .iter()
                .map(|n| {
                    let at = item.source.start
                        + doc.text[item.source.clone()].find(n).expect("cell text");
                    let caret = block
                        .borrow()
                        .caret_bounds(at - node.source.start, Bias::Left)
                        .expect("painted");
                    f32::from(caret.origin.x)
                })
                .collect()
        })
        .collect()
}

#[gpui::test]
fn profile_changes_rerender_and_table_columns_line_up(cx: &mut TestAppContext) {
    let md = "| a | bbbbbb |\n|:-|:-:|\n| cccc | d |\n\nx ~~y~~ $z$\n";
    let dir = temp(&[("t.md", md)]);
    let file = dir.path().join("t.md");
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    let set = |vcx: &mut VisualTestContext, profile| {
        vcx.update(|_, cx| {
            crate::profile::ProfileOverrides::set(
                cx,
                &file,
                crate::profile::Scope::File,
                Some(profile),
            );
        });
        draw(vcx);
        draw(vcx);
    };
    let kinds = |vcx: &mut VisualTestContext| -> Vec<&'static str> {
        let st = state(&reader, vcx);
        let s = st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        doc.tree.items().iter().map(|i| i.kind.name()).collect()
    };

    // CommonMark from the chip: no table.
    set(&mut vcx, gv_profile::ProfileId::CommonMark);
    assert_eq!(
        reader.read_with(&vcx, |r, _| r.profile()),
        gv_profile::ProfileId::CommonMark
    );
    assert_eq!(kinds(&mut vcx), ["paragraph", "paragraph"]);

    // GFM: one item per row, the columns line up (left, then centred), and
    // the paragraph's formula and strikethrough render.
    set(&mut vcx, gv_profile::ProfileId::GfmFile);
    assert_eq!(kinds(&mut vcx), ["table-row", "table-row", "paragraph"]);
    let st = state(&reader, &mut vcx);
    let lefts = cell_lefts(&st.borrow(), &[&["a", "bbbbbb"], &["cccc", "d"]]);
    assert!((lefts[0][0] - lefts[1][0]).abs() < 0.5, "{lefts:?}");
    assert!(lefts[1][1] > lefts[0][1] + 1.0, "centred: {lefts:?}");
    {
        let s = st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        let widths = s
            .cache
            .table(doc.tree.items()[0].id.block)
            .expect("measured");
        assert_eq!(widths.columns.len(), 2);
        assert!(
            widths.columns[1] > widths.columns[0],
            "{:?}",
            widths.columns
        );
        let para = doc.tree.items().len() - 1;
        let content = doc.tree.item_inline(para).expect("inline").clone();
        assert_eq!(content.math.len(), 1);
        let g = s
            .geometry
            .items
            .iter()
            .find(|g| g.ix == para)
            .expect("drawn");
        let block = g.text.as_ref().expect("text").borrow();
        let id = gv_render_tree::BoxRef::Math(0).id();
        assert!(
            matches!(
                block.box_content(id),
                Some(crate::inline::BoxContent::Math { .. })
            ),
            "the formula is typeset"
        );
    }
}

/// Sets the reading profile of `file` and draws.
fn set_profile(vcx: &mut VisualTestContext, file: &Path, profile: gv_profile::ProfileId) {
    vcx.update(|_, cx| {
        crate::profile::ProfileOverrides::set(cx, file, crate::profile::Scope::File, Some(profile));
    });
    draw(vcx);
    draw(vcx);
}

#[gpui::test]
fn a_table_wider_than_the_column_wraps_its_cells_to_fit(cx: &mut TestAppContext) {
    let long = "words that go on and on ".repeat(12);
    let md = format!(
        "| Construct | Rendering |\n|---|---|\n| Short | {long}end_of_cell |\n| Two | x |\n\nafter\n"
    );
    let dir = temp(&[("t.md", &md)]);
    let file = dir.path().join("t.md");
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    set_profile(&mut vcx, &file, gv_profile::ProfileId::GfmFile);
    let st = state(&reader, &mut vcx);
    // At each width: the table is no wider than the column, the long cell
    // wraps onto several lines inside it, and the row's height (the one
    // the scroll position uses) is the wrapped one.
    let mut heights = Vec::new();
    for width in [WIDTH, 700.0, 520.0, WIDTH] {
        vcx.simulate_resize(size(px(width), px(HEIGHT)));
        draw(&mut vcx);
        draw(&mut vcx);
        let s = st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        let (x0, column) = s.geometry.column;
        let m = doc.tree.options().metrics;
        let widths = s
            .cache
            .table(doc.tree.items()[0].id.block)
            .expect("measured");
        assert!(
            widths.columns[1] > column,
            "the long cell is wider than the column unwrapped"
        );
        let table_w = widths.width(m.table_padding_x) + 2.0 * m.table_border;
        assert!(
            table_w <= column + 0.5,
            "width {width}: table {table_w} in a column of {column}"
        );
        let row = s
            .geometry
            .items
            .iter()
            .find(|g| {
                matches!(
                    doc.tree.items()[g.ix].kind,
                    gv_render_tree::ItemKind::TableRow { row: 1, .. }
                )
            })
            .expect("the long row is drawn");
        let item = &doc.tree.items()[row.ix];
        let node = doc.tree.node_of(row.ix).expect("node");
        let block = row.text.as_ref().expect("a row block").borrow();
        let at = item.source.start
            + doc.text[item.source.clone()]
                .find("end_of_cell")
                .expect("cell text");
        let end = block
            .caret_bounds(at + "end_of_cell".len() - node.source.start, Bias::Right)
            .expect("painted");
        assert!(
            f32::from(end.origin.x) <= x0 + column + 0.5,
            "width {width}: the cell's end at {:?} is past the column's right edge {}",
            end.origin.x,
            x0 + column
        );
        let lines = block.layout().expect("laid out").lines().len();
        assert!(lines > 3, "width {width}: {lines} lines");
        let body = doc
            .tree
            .options()
            .styles
            .role(gv_render_tree::TextRole::TableCell)
            .line_height;
        let h = row.bottom - row.top;
        assert!(h > 3.0 * body, "width {width}: row height {h}");
        assert_eq!(
            s.engine.height_of(row.ix),
            Some(h),
            "width {width}: the height index has the wrapped height"
        );
        heights.push(h);
    }
    assert!(heights[2] > heights[1], "narrower, taller: {heights:?}");
    assert!((heights[3] - heights[0]).abs() < 0.5, "{heights:?}");
}

mod code_tests;
mod constructs_tests;
mod embeds_tests;
mod expand_tests;
mod find_tests;
mod image_diagram_tests;
mod media_tests;
mod obsidian_tests;
mod remote_tests;
mod wide_tests;

/// Timing runs with the platform's real text system (Linux), for the
/// report: the CommonMark spec examples and a generated 10 MB document,
/// from the open request to the first frame with content, and frame CPU
/// while scrolling.
#[cfg(target_os = "linux")]
mod timing {
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, Instant};

    use gpui::{PlatformTextSystem, TestDispatcher};

    use super::*;
    use crate::reader::bench::percentiles;

    fn platform_text_system() -> Arc<dyn PlatformTextSystem> {
        static TEXT: OnceLock<Arc<dyn PlatformTextSystem>> = OnceLock::new();
        TEXT.get_or_init(|| gpui::platform::current_platform(true).text_system())
            .clone()
    }

    /// The CommonMark 0.31.2 spec examples, one after another under their
    /// section headings, and the GFM spec text.
    fn spec_markdown() -> (String, String) {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/spec");
        let json =
            std::fs::read_to_string(fixtures.join("commonmark-0.31.2.json")).expect("spec fixture");
        let examples: serde_json::Value = serde_json::from_str(&json).expect("json");
        let mut out = String::from("# CommonMark spec examples\n\n");
        let mut section = String::new();
        for e in examples.as_array().into_iter().flatten() {
            let field = |k: &str| e[k].as_str().unwrap_or_default().to_owned();
            if field("section") != section {
                section = field("section");
                out.push_str(&format!("## {section}\n\n"));
            }
            out.push_str(&field("markdown"));
            out.push_str("\n\n");
        }
        let gfm = std::fs::read_to_string(fixtures.join("gfm-0.29.0.gfm.13.txt"))
            .expect("gfm spec fixture");
        (out, gfm)
    }

    /// About `bytes` of mixed prose, lists, quotes and code.
    fn generated(bytes: usize) -> String {
        let mut s = String::with_capacity(bytes + 4096);
        let mut i = 0;
        while s.len() < bytes {
            match i % 10 {
                0 => s.push_str(&format!("## Section {i}\n\n")),
                1..=5 => s.push_str(&format!(
                    "Paragraph {i} with **strong** and *emphasised* words, `inline code` and a \
                     [link](other-{i}.md#part) to another note, written long enough to wrap \
                     over two or three lines of the text column.\n\n"
                )),
                6 => s.push_str(&format!("- item {i} one\n- item two\n  - nested three\n\n")),
                7 => s.push_str(&format!(
                    "> A quote {i} that also has some words in it.\n\n"
                )),
                _ => s.push_str(&format!(
                    "```rust\nfn f{i}(x: u32) -> u32 {{\n    x * 2 + {i}\n}}\n```\n\n"
                )),
            }
            i += 1;
        }
        s
    }

    fn run(name: &str, markdown: &str) {
        let dir = temp(&[("doc.md", markdown)]);
        let dispatcher = TestDispatcher::new(0);
        let mut cx = TestAppContext::build_with_text_system(
            dispatcher.clone(),
            Some("reader_timing"),
            platform_text_system(),
        );
        {
            let started = Instant::now();
            let (window, workspace) = open(&mut cx, &dir.path().join("doc.md"));
            let mut vcx = VisualTestContext::from_window(window, &cx);
            draw(&mut vcx);
            let first = started.elapsed();
            let reader = reader(&workspace, &mut vcx);
            let st = state(&reader, &mut vcx);
            let (items, load) = {
                let s = st.borrow();
                let doc = s.doc.as_ref().expect("loaded");
                (
                    doc.tree.items().len(),
                    crate::reader::bench::load_line(&doc.timings),
                )
            };
            let mut frames: Vec<Duration> = Vec::new();
            for step in [24.0f32, 400.0] {
                for _ in 0..300 {
                    st.borrow_mut().engine.scroll_by(step);
                    let t = Instant::now();
                    vcx.update(|window, cx| window.draw(cx).clear(cx));
                    frames.push(t.elapsed());
                }
                eprintln!(
                    "{name}: scroll {step} px/frame (layout and scene, no GPU): {}",
                    percentiles(&frames[frames.len() - 290..])
                );
            }
            // A live resize: a new width every frame (every height is
            // re-estimated, the visible items re-wrap).
            let mut resize = Vec::new();
            for k in 0..120 {
                // Narrower than the column plus its padding: the column changes.
                let width = 500.0 + 2.0 * (k % 100) as f32;
                // The test platform may draw inside the resize: both count.
                let t = Instant::now();
                vcx.simulate_resize(size(px(width), px(HEIGHT)));
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                resize.push(t.elapsed());
            }
            eprintln!(
                "{name}: live resize, new width every frame (resize and draw): {}",
                percentiles(&resize[20..])
            );
            eprintln!(
                "{name}: {items} items; {load}; open request to first frame with content {:.1} ms",
                first.as_secs_f64() * 1e3
            );
        }
        cx.run_until_parked();
        cx.update(|cx| cx.quit());
        cx.run_until_parked();
        drop(cx);
        dispatcher.drain_tasks();
    }

    /// Find in page on the generated 10 MB document: Mod+F (which builds
    /// the find index), then "t", "th", "the" typed one by one; each from
    /// the keystroke to the frame with the new highlights drawn.
    #[test]
    #[ignore = "timing run; see the module docs"]
    fn timing_find_in_ten_megabytes() {
        let dir = temp(&[("doc.md", &generated(10 << 20))]);
        let dispatcher = TestDispatcher::new(0);
        let mut cx = TestAppContext::build_with_text_system(
            dispatcher.clone(),
            Some("find_timing"),
            platform_text_system(),
        );
        {
            let (window, workspace) = open(&mut cx, &dir.path().join("doc.md"));
            let mut vcx = VisualTestContext::from_window(window, &cx);
            draw(&mut vcx);
            let reader = reader(&workspace, &mut vcx);
            let st = state(&reader, &mut vcx);
            let t = Instant::now();
            vcx.simulate_keystrokes("secondary-f");
            draw(&mut vcx);
            eprintln!(
                "find, generated 10 MB: Mod+F to the popover drawn, find index built: {:.1} ms",
                t.elapsed().as_secs_f64() * 1e3
            );
            for key in ["t", "h", "e"] {
                let t = Instant::now();
                vcx.simulate_input(key);
                vcx.run_until_parked();
                let found = t.elapsed();
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                let drawn = t.elapsed();
                let s = st.borrow();
                let rects: u32 = s
                    .geometry
                    .items
                    .iter()
                    .filter_map(|g| g.text.as_ref())
                    .map(|b| b.borrow().paint_stats().highlight_rects)
                    .sum();
                eprintln!(
                    "find, generated 10 MB: typed {key:?}: {} matches{} found in {:.1} ms, \
                     drawn with {rects} highlights at {:.1} ms",
                    s.find.matches.len(),
                    if s.find.more { "+" } else { "" },
                    found.as_secs_f64() * 1e3,
                    drawn.as_secs_f64() * 1e3,
                );
            }
        }
        cx.run_until_parked();
        cx.update(|cx| cx.quit());
        cx.run_until_parked();
        drop(cx);
        dispatcher.drain_tasks();
    }

    #[test]
    #[ignore = "timing run; see the module docs"]
    fn timing_spec_and_ten_megabytes() {
        let (examples, gfm) = spec_markdown();
        run("CommonMark spec examples", &examples);
        run("GFM spec text", &gfm);
        run("generated 10 MB", &generated(10 << 20));
    }
}
