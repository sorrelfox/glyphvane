// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Find in page in headless windows: the popover's keys, the search
//! options, the current match and its scrolling, the highlights, and
//! searches on the background executor.

use gpui::Focusable as _;

use super::*;
use crate::find::{FindBar, FindStatus};
use crate::reader::find::{CURRENT_MATCH, MATCH, MAX_MATCHES, SYNC_ITEMS};
use crate::reader::state::Selection;

const DOC: &str = "# Alpha notes

Alpha and beta, then alpha again.

The alphabet is not a match for whole words.

```text
code alpha
```
";

/// A window on `text`, drawn, with its reader.
fn setup(
    cx: &mut TestAppContext,
    text: &str,
) -> (
    tempfile::TempDir,
    VisualTestContext,
    Entity<Workspace>,
    Entity<Reader>,
) {
    let dir = temp(&[("a.md", text)]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    (dir, vcx, workspace, reader)
}

fn bar(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> Entity<FindBar> {
    reader
        .read_with(vcx, |r, _| r.find_bar().cloned())
        .expect("the find popover exists")
}

fn status(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> FindStatus {
    let bar = bar(reader, vcx);
    bar.read_with(vcx, |b, _| b.status())
}

fn count(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> Option<String> {
    status(reader, vcx).text()
}

/// The text of the current match.
fn current(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> Option<(usize, String)> {
    let st = state(reader, vcx);
    let s = st.borrow();
    let doc = s.doc.as_ref()?;
    let k = s.find.current?;
    let m = s.find.matches.get(k)?;
    Some((k, doc.text[m.source.clone()].to_owned()))
}

fn field_focused(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> bool {
    let bar = bar(reader, vcx);
    vcx.update(|window, cx| bar.read(cx).field_focused(window, cx))
}

fn reader_focused(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> bool {
    let reader = reader.clone();
    vcx.update(|window, cx| reader.read(cx).focus_handle(cx).is_focused(window))
}

/// Opens find with Mod+F and types `query`.
fn find(vcx: &mut VisualTestContext, query: &str) {
    vcx.simulate_keystrokes("secondary-f");
    draw(vcx);
    if !query.is_empty() {
        vcx.simulate_input(query);
    }
    draw(vcx);
}

fn click(vcx: &mut VisualTestContext, selector: &'static str) {
    let bounds = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    vcx.simulate_click(bounds.center(), Modifiers::none());
    draw(vcx);
}

#[gpui::test]
fn mod_f_opens_find_filled_with_the_selection(cx: &mut TestAppContext) {
    let (_dir, mut vcx, _ws, reader) = setup(cx, DOC);
    assert!(!reader.read_with(&vcx, |r, _| r.is_find_open()));
    // Select the second "alpha" of the first paragraph.
    {
        let st = state(&reader, &mut vcx);
        let mut s = st.borrow_mut();
        let at = s
            .doc
            .as_ref()
            .expect("loaded")
            .text
            .find("alpha again")
            .expect("text");
        s.selection = Some(Selection {
            anchor: at,
            head: at + 5,
        });
    }
    vcx.simulate_keystrokes("secondary-f");
    draw(&mut vcx);
    assert!(reader.read_with(&vcx, |r, _| r.is_find_open()));
    assert!(
        vcx.debug_bounds("find-bar").is_some(),
        "the popover is drawn"
    );
    let bar = bar(&reader, &mut vcx);
    assert_eq!(bar.read_with(&vcx, |b, cx| b.query(cx)), "alpha");
    assert!(field_focused(&reader, &mut vcx));
    // Five matches (case-insensitive, inside words too); the current one
    // is the selected one.
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("3 of 5"));
    assert_eq!(current(&reader, &mut vcx), Some((2, "alpha".to_owned())));
    // The popover sits at the top right of the document area, 380 wide.
    let popover = vcx.debug_bounds("find-bar").expect("drawn");
    let area = state(&reader, &mut vcx).borrow().geometry.bounds;
    assert_eq!(popover.size.width, px(380.0));
    assert!((popover.right() - (area.right() - px(16.0))).abs() < px(0.5));
    assert!((popover.top() - (area.top() + px(8.0))).abs() < px(0.5));
    // A selection over several blocks does not replace the query.
    {
        let st = state(&reader, &mut vcx);
        let mut s = st.borrow_mut();
        let len = s.doc.as_ref().expect("loaded").text.len();
        s.selection = Some(Selection {
            anchor: 0,
            head: len,
        });
    }
    vcx.simulate_keystrokes("secondary-f");
    draw(&mut vcx);
    assert_eq!(bar.read_with(&vcx, |b, cx| b.query(cx)), "alpha");
}

#[gpui::test]
fn typing_finds_and_counts_incrementally(cx: &mut TestAppContext) {
    let (_dir, mut vcx, _ws, reader) = setup(cx, DOC);
    find(&mut vcx, "");
    assert!(field_focused(&reader, &mut vcx));
    assert_eq!(status(&reader, &mut vcx), FindStatus::Empty);
    assert_eq!(count(&reader, &mut vcx), None);
    vcx.simulate_input("alp");
    draw(&mut vcx);
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("1 of 5"));
    vcx.simulate_input("ha a");
    draw(&mut vcx);
    // "Alpha and", "alpha again".
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("1 of 2"));
    vcx.simulate_input("x");
    draw(&mut vcx);
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("No results"));
    let st = state(&reader, &mut vcx);
    assert!(st.borrow().find.matches.is_empty());
}

#[gpui::test]
fn next_and_previous_wrap_around(cx: &mut TestAppContext) {
    let (_dir, mut vcx, _ws, reader) = setup(cx, DOC);
    find(&mut vcx, "alpha");
    let at = |vcx: &mut VisualTestContext| current(&reader, vcx).map(|c| c.0);
    assert_eq!(at(&mut vcx), Some(0));
    // Enter and Shift+Enter in the field.
    for expected in [1, 2, 3, 4, 0] {
        vcx.simulate_keystrokes("enter");
        draw(&mut vcx);
        assert_eq!(at(&mut vcx), Some(expected));
    }
    vcx.simulate_keystrokes("shift-enter");
    draw(&mut vcx);
    assert_eq!(at(&mut vcx), Some(4));
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("5 of 5"));
    // F3 and Shift+F3 (Cmd+G on macOS), in the field and in the document.
    let (next, previous) = if cfg!(target_os = "macos") {
        ("cmd-g", "cmd-shift-g")
    } else {
        ("f3", "shift-f3")
    };
    vcx.simulate_keystrokes(next);
    draw(&mut vcx);
    assert_eq!(at(&mut vcx), Some(0));
    vcx.simulate_keystrokes(previous);
    draw(&mut vcx);
    assert_eq!(at(&mut vcx), Some(4));
    // The buttons.
    click(&mut vcx, "find-next");
    assert_eq!(at(&mut vcx), Some(0));
    click(&mut vcx, "find-previous");
    assert_eq!(at(&mut vcx), Some(4));
    // From the document, with the popover open.
    let r = reader.clone();
    vcx.update(|window, cx| r.read(cx).focus_handle(cx).focus(window, cx));
    vcx.simulate_keystrokes(next);
    draw(&mut vcx);
    assert_eq!(at(&mut vcx), Some(0));
}

#[gpui::test]
fn the_toggles_change_what_matches(cx: &mut TestAppContext) {
    let (_dir, mut vcx, _ws, reader) = setup(cx, DOC);
    find(&mut vcx, "alpha");
    let texts = |vcx: &mut VisualTestContext| {
        let st = state(&reader, vcx);
        let s = st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        s.find
            .matches
            .iter()
            .map(|m| doc.text[m.source.clone()].to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(texts(&mut vcx).len(), 5);
    click(&mut vcx, "find-match-case");
    assert!(bar(&reader, &mut vcx).read_with(&vcx, |b, _| b.options().case_sensitive));
    assert_eq!(texts(&mut vcx), ["alpha", "alpha", "alpha"]);
    click(&mut vcx, "find-whole-word");
    // "alphabet" is not a whole word.
    assert_eq!(texts(&mut vcx), ["alpha", "alpha"]);
    click(&mut vcx, "find-match-case");
    assert_eq!(texts(&mut vcx), ["Alpha", "Alpha", "alpha", "alpha"]);
    click(&mut vcx, "find-whole-word");
    // A regular expression: invalid, then valid.
    click(&mut vcx, "find-regex");
    vcx.simulate_input("(");
    draw(&mut vcx);
    assert_eq!(status(&reader, &mut vcx), FindStatus::Invalid);
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("Invalid"));
    assert!(state(&reader, &mut vcx).borrow().find.matches.is_empty());
    vcx.simulate_keystrokes("backspace");
    vcx.simulate_input("|beta");
    draw(&mut vcx);
    assert_eq!(
        texts(&mut vcx),
        ["Alpha", "Alpha", "beta", "alpha", "alpha", "alpha"]
    );
    // Without regex the same text is a literal.
    click(&mut vcx, "find-regex");
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("No results"));
}

#[gpui::test]
fn escape_closes_and_the_document_gets_the_keyboard(cx: &mut TestAppContext) {
    let (_dir, mut vcx, _ws, reader) = setup(cx, DOC);
    find(&mut vcx, "alpha");
    assert!(field_focused(&reader, &mut vcx));
    assert!(!state(&reader, &mut vcx).borrow().find.matches.is_empty());
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(!reader.read_with(&vcx, |r, _| r.is_find_open()));
    assert!(
        vcx.debug_bounds("find-bar").is_none(),
        "the popover is gone"
    );
    assert!(reader_focused(&reader, &mut vcx));
    assert!(state(&reader, &mut vcx).borrow().find.matches.is_empty());
    // Mod+F again: the query is kept and searched again.
    vcx.simulate_keystrokes("secondary-f");
    draw(&mut vcx);
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("1 of 5"));
    // The close button does the same as Esc.
    click(&mut vcx, "find-close");
    assert!(!reader.read_with(&vcx, |r, _| r.is_find_open()));
    assert!(reader_focused(&reader, &mut vcx));
}

/// `n` paragraphs with "needle" in each, between filler; paragraphs 3
/// and 150 also have a marker.
fn needles(n: usize) -> String {
    let mut s = String::from("# Haystack\n\n");
    for i in 0..n {
        let marker = if i == 3 || i == 150 { " marker" } else { "" };
        s.push_str(&format!(
            "Paragraph {i} is filler text that goes on for a while and then has a needle \
             in it, and some more words after that{marker}.\n\n"
        ));
    }
    s
}

/// The highlight rectangles painted in the last frame, and the items they
/// were painted in.
fn painted_highlights(reader: &Entity<Reader>, vcx: &mut VisualTestContext) -> (u32, Vec<usize>) {
    let st = state(reader, vcx);
    let s = st.borrow();
    let mut rects = 0;
    let mut items = Vec::new();
    for g in &s.geometry.items {
        let Some(block) = &g.text else {
            continue;
        };
        let n = block.borrow().paint_stats().highlight_rects;
        if n > 0 {
            items.push(g.ix);
        }
        rects += n;
    }
    (rects, items)
}

#[gpui::test]
fn only_visible_matches_are_painted_and_the_current_one_scrolls_into_view(cx: &mut TestAppContext) {
    let (_dir, mut vcx, _ws, reader) = setup(cx, &needles(300));
    find(&mut vcx, "needle");
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("1 of 300"));
    let (rects, items) = painted_highlights(&reader, &mut vcx);
    let placed = state(&reader, &mut vcx).borrow().geometry.items.len();
    // One match per painted paragraph (a match may wrap onto two lines).
    assert!(!items.is_empty() && items.len() < 300, "{items:?}");
    assert!(items.len() <= placed);
    assert!(rects as usize >= items.len());
    // The highlights carry the match tokens: the current one differs.
    {
        let st = state(&reader, &mut vcx);
        let s = st.borrow();
        let ix = s.find.matches[0].item;
        let node = s
            .doc
            .as_ref()
            .expect("loaded")
            .tree
            .node_of(ix)
            .expect("node");
        let tokens: Vec<_> = s
            .find
            .highlights(ix, node.source.start)
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        assert_eq!(tokens, [CURRENT_MATCH]);
        let next = s.find.matches[1].item;
        let node = s
            .doc
            .as_ref()
            .expect("loaded")
            .tree
            .node_of(next)
            .expect("node");
        assert_eq!(s.find.highlights(next, node.source.start)[0].1, MATCH);
    }
    // Shift+Enter wraps to the last match, at the end of the document: it
    // is scrolled into view, and the highlights follow.
    vcx.simulate_keystrokes("shift-enter");
    draw(&mut vcx);
    draw(&mut vcx);
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("300 of 300"));
    let last = {
        let st = state(&reader, &mut vcx);
        let s = st.borrow();
        s.find.matches[299].item
    };
    let (_, items) = painted_highlights(&reader, &mut vcx);
    assert!(items.contains(&last), "{last} not in {items:?}");
    // Close, back to the top, and find the two markers: the first is on
    // screen and does not scroll; the second arrives centred.
    vcx.simulate_keystrokes("escape");
    let st = state(&reader, &mut vcx);
    st.borrow_mut().engine.scroll_to_y(0.0);
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-f");
    vcx.simulate_input("marker");
    draw(&mut vcx);
    draw(&mut vcx);
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("1 of 2"));
    assert_eq!(st.borrow().engine.scroll_top(), 0.0);
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    draw(&mut vcx);
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("2 of 2"));
    let s = st.borrow();
    let m = &s.find.matches[1];
    let g = s
        .geometry
        .items
        .iter()
        .find(|g| g.ix == m.item)
        .expect("the current match's item is placed");
    let node = s
        .doc
        .as_ref()
        .expect("loaded")
        .tree
        .node_of(m.item)
        .expect("node");
    let caret = g
        .text
        .as_ref()
        .expect("text")
        .borrow()
        .caret_bounds(m.source.start - node.source.start, Bias::Left)
        .expect("painted");
    let bounds = s.geometry.bounds;
    let middle = bounds.top() + bounds.size.height / 2.0;
    assert!(
        (caret.center().y - middle).abs() < px(2.0),
        "match at {:?}, middle {middle:?}",
        caret.center().y
    );
}

#[gpui::test]
fn large_documents_are_searched_in_the_background(cx: &mut TestAppContext) {
    // More items than are searched on the UI thread, and more matches
    // than are kept.
    let n = (SYNC_ITEMS + 10).max(MAX_MATCHES / 2 + 10);
    let mut text = String::from("# Many\n\n");
    for i in 0..n {
        text.push_str(&format!("Line {i}: aa aa\n\n"));
    }
    let (_dir, mut vcx, _ws, reader) = setup(cx, &text);
    vcx.simulate_keystrokes("secondary-f");
    draw(&mut vcx);
    vcx.simulate_input("aa");
    draw(&mut vcx);
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("1 of 10,000+"));
    assert_eq!(
        state(&reader, &mut vcx).borrow().find.matches.len(),
        MAX_MATCHES
    );
    // Each keystroke starts a new search; what shows is the last one's.
    vcx.simulate_input(" a");
    vcx.simulate_input("a");
    draw(&mut vcx);
    // One "aa aa" per line.
    assert_eq!(
        status(&reader, &mut vcx),
        FindStatus::Matches {
            current: Some(0),
            total: n,
            more: false,
        }
    );
    let st = state(&reader, &mut vcx);
    let s = st.borrow();
    let doc = s.doc.as_ref().expect("loaded");
    assert_eq!(&doc.text[s.find.matches[0].source.clone()], "aa aa");
}

#[gpui::test]
fn a_reloaded_document_is_searched_again(cx: &mut TestAppContext) {
    let (dir, mut vcx, _ws, reader) = setup(cx, DOC);
    find(&mut vcx, "alpha");
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("1 of 5"));
    let path = dir.path().join("a.md");
    std::fs::write(&path, "alpha\n\nAlpha beta\n").expect("write");
    reader.update(&mut vcx, |r, cx| r.open(path, None, cx));
    draw(&mut vcx);
    draw(&mut vcx);
    assert_eq!(count(&reader, &mut vcx).as_deref(), Some("1 of 2"));
    let (rects, _) = painted_highlights(&reader, &mut vcx);
    assert_eq!(rects, 2);
}

#[gpui::test]
fn matches_in_code_are_painted(cx: &mut TestAppContext) {
    let (_dir, mut vcx, _ws, reader) = setup(cx, DOC);
    find(&mut vcx, "alpha");
    let (_, items) = painted_highlights(&reader, &mut vcx);
    let code = {
        let st = state(&reader, &mut vcx);
        let s = st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        doc.tree
            .items()
            .iter()
            .position(|it| it.kind.name() == "code")
            .expect("a code item")
    };
    assert!(items.contains(&code), "{code} not in {items:?}");
}
