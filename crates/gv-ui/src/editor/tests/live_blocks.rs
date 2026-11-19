// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live Preview block widgets in headless windows: each kind rendered
//! away from the caret and shown as source with the caret inside, heights
//! in the block layer, the caret entering with arrows and clicks, undo,
//! edits that change a block's kind, and the scroll staying put when a
//! height lands late.

use super::*;

fn obsidian() -> ParseOptions {
    ParseOptions::from_profile(&gv_profile::compile_builtin(
        gv_profile::ProfileId::Obsidian,
    ))
}

/// A Source view of `text` in Live Preview with the Obsidian syntax, the
/// caret at the start, its blocks measured.
fn live(cx: &mut TestAppContext, text: &str) -> (Entity<EditorView>, VisualTestContext) {
    let (view, mut vcx) = editor_with(cx, text, obsidian());
    view.update(&mut vcx, |v, cx| {
        v.set_profile("obsidian", cx);
        v.set_live_preview(true, cx);
    });
    settle(&mut vcx);
    (view, vcx)
}

/// Draws until block heights stop landing.
fn settle(vcx: &mut VisualTestContext) {
    for _ in 0..8 {
        draw(vcx);
    }
}

fn caret_at(view: &Entity<EditorView>, vcx: &mut VisualTestContext, offset: usize) {
    view.update(vcx, |v, cx| v.set_cursor(offset, cx));
    settle(vcx);
}

/// The blocks in the block layer: kind, range, below its line.
fn blocks(
    view: &Entity<EditorView>,
    vcx: &mut VisualTestContext,
) -> Vec<(&'static str, std::ops::Range<usize>, bool)> {
    view.read_with(vcx, |v, _| v.live_blocks())
}

fn kinds(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Vec<&'static str> {
    blocks(view, vcx).into_iter().map(|(k, _, _)| k).collect()
}

/// The blocks drawn in the last frame: kind and bounds.
fn drawn_blocks(
    view: &Entity<EditorView>,
    vcx: &mut VisualTestContext,
) -> Vec<(&'static str, Bounds<Pixels>)> {
    probe_of(view, vcx)
        .blocks
        .iter()
        .map(|(b, k)| (*k, *b))
        .collect()
}

/// The lines whose text was drawn.
fn drawn_lines(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Vec<usize> {
    probe_of(view, vcx)
        .row_texts
        .iter()
        .map(|(l, _)| *l)
        .collect()
}

const CODE: &str = "intro\n\n```rust\nfn a() {}\n```\n\nafter\n";

#[gpui::test]
fn code_is_rendered_away_from_the_caret_and_source_inside(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, CODE);
    assert_eq!(blocks(&view, &mut vcx), [("code", 7..28, false)]);
    let drawn = drawn_blocks(&view, &mut vcx);
    assert_eq!(drawn.len(), 1);
    // The rows of the fence are not drawn; the lines around it are.
    assert_eq!(drawn_lines(&view, &mut vcx), [0, 1, 5, 6, 7]);
    // Its height is the view's content height, in the block layer, and
    // the text below starts under it.
    let heights = view.read_with(&vcx, |v, _| v.live_block_heights());
    let (_, bounds) = drawn[0];
    assert!((f32::from(bounds.size.height) - heights[0]).abs() < 0.01);
    assert!(heights[0] > LINE, "{heights:?}");
    let probe = probe_of(&view, &mut vcx);
    let row = |line: usize| {
        probe
            .number_rows
            .iter()
            .zip(&probe.numbers)
            .find(|(_, (l, _))| *l == line)
            .map(|(r, _)| *r)
    };
    assert!(row(5).is_some());
    let after = view.update_in(&mut vcx, |v, window, cx| {
        v.range_bounds(29..29, None, window, cx).expect("on screen")
    });
    assert!(
        (f32::from(after.top()) - f32::from(bounds.bottom())).abs() < 0.5,
        "{after:?} {bounds:?}"
    );
    // The caret inside: source, no block.
    caret_at(&view, &mut vcx, 12);
    assert!(blocks(&view, &mut vcx).is_empty());
    assert!(drawn_blocks(&view, &mut vcx).is_empty());
    assert_eq!(drawn_lines(&view, &mut vcx), [0, 1, 2, 3, 4, 5, 6, 7]);
    // Out again: rendered, at the height it was measured at (no jump).
    caret_at(&view, &mut vcx, 0);
    assert_eq!(kinds(&view, &mut vcx), ["code"]);
    assert_eq!(view.read_with(&vcx, |v, _| v.live_block_heights()), heights);
    // A caret at the end of the line before or the start of the line
    // after does not touch it.
    caret_at(&view, &mut vcx, 6);
    assert_eq!(kinds(&view, &mut vcx), ["code"]);
    caret_at(&view, &mut vcx, 29);
    assert_eq!(kinds(&view, &mut vcx), ["code"]);
    // The end of its last line does.
    caret_at(&view, &mut vcx, 28);
    assert!(kinds(&view, &mut vcx).is_empty());
}

#[gpui::test]
fn arrows_enter_a_block_at_its_nearest_line(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, CODE);
    // Down from the blank line above: its first line.
    caret_at(&view, &mut vcx, 6);
    vcx.simulate_keystrokes("down");
    settle(&mut vcx);
    assert_eq!(selections(&view, &mut vcx), [(7, 7)]);
    assert!(kinds(&view, &mut vcx).is_empty());
    // Away, then up from the blank line below: its last line.
    caret_at(&view, &mut vcx, 29);
    assert_eq!(kinds(&view, &mut vcx), ["code"]);
    vcx.simulate_keystrokes("up");
    settle(&mut vcx);
    assert_eq!(selections(&view, &mut vcx), [(25, 25)]);
    assert!(kinds(&view, &mut vcx).is_empty());
    // Leaving with Down: rendered again.
    vcx.simulate_keystrokes("down down");
    settle(&mut vcx);
    assert_eq!(kinds(&view, &mut vcx), ["code"]);
    // Right from the end of the line above goes in too.
    caret_at(&view, &mut vcx, 6);
    vcx.simulate_keystrokes("right");
    settle(&mut vcx);
    assert_eq!(selections(&view, &mut vcx), [(7, 7)]);
    assert!(kinds(&view, &mut vcx).is_empty());
}

#[gpui::test]
fn a_click_on_a_block_puts_the_caret_in_its_source(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, CODE);
    let (_, bounds) = drawn_blocks(&view, &mut vcx)[0];
    press(&mut vcx, bounds.center(), 1, Modifiers::none());
    settle(&mut vcx);
    let (_, head) = selections(&view, &mut vcx)[0];
    assert!((7..=28).contains(&head), "{head}");
    assert!(kinds(&view, &mut vcx).is_empty());
    // The editor keeps the focus.
    assert!(vcx.update(|window, cx| view.read(cx).focus.is_focused(window)));
}

#[gpui::test]
fn undo_and_edits_that_change_a_blocks_kind(cx: &mut TestAppContext) {
    // 0123 4 5678 9
    // a\n\n---\n\nb\n
    let (view, mut vcx) = live(cx, "a\n\n---\n\nb\n");
    assert_eq!(blocks(&view, &mut vcx), [("rule", 3..6, false)]);
    // Typing after the rule makes it a paragraph: no block once the
    // caret leaves.
    caret_at(&view, &mut vcx, 6);
    vcx.simulate_keystrokes("x");
    settle(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx), "a\n\n---x\n\nb\n");
    caret_at(&view, &mut vcx, 0);
    assert!(kinds(&view, &mut vcx).is_empty());
    // Undo puts the text and the caret back: the caret is in the rule,
    // which shows its source; away from it, it is a rule again.
    vcx.simulate_keystrokes("secondary-z");
    settle(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx), "a\n\n---\n\nb\n");
    assert_eq!(selections(&view, &mut vcx), [(6, 6)]);
    assert!(kinds(&view, &mut vcx).is_empty());
    caret_at(&view, &mut vcx, 0);
    assert_eq!(kinds(&view, &mut vcx), ["rule"]);
    // Redo: a paragraph again.
    vcx.simulate_keystrokes("secondary-shift-z");
    settle(&mut vcx);
    caret_at(&view, &mut vcx, 0);
    assert!(kinds(&view, &mut vcx).is_empty());
    // A paragraph that becomes a fence becomes a block.
    let (view, mut vcx) = live(cx, "a\n\nb\n");
    caret_at(&view, &mut vcx, 3);
    vcx.simulate_keystrokes("` ` ` enter");
    settle(&mut vcx);
    caret_at(&view, &mut vcx, 0);
    assert_eq!(kinds(&view, &mut vcx), ["code"]);
}

#[gpui::test]
fn every_kind_of_block(cx: &mut TestAppContext) {
    let text = "---\ntitle: Note\ntags: [a]\n---\n\nintro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n$$\nx^2 + 1\n$$\n\n```mermaid\ngraph TD\n  A --> B\n```\n\n![[other]]\n\n> [!note] Title\n> body\n\n***\n\nend\n";
    let (view, mut vcx) = live(cx, text);
    caret_at(&view, &mut vcx, text.len());
    let all = [
        "properties",
        "table",
        "math",
        "diagram",
        "embed",
        "callout",
        "rule",
    ];
    assert_eq!(kinds(&view, &mut vcx), all);
    let drawn: Vec<&str> = drawn_blocks(&view, &mut vcx)
        .iter()
        .map(|(k, _)| *k)
        .collect();
    assert_eq!(drawn, all);
    // Every one shows its source with the caret inside, the others stay.
    for (i, (kind, range, _)) in blocks(&view, &mut vcx).into_iter().enumerate() {
        caret_at(&view, &mut vcx, range.start + 1);
        let now = kinds(&view, &mut vcx);
        assert!(!now.contains(&kind) || all.iter().filter(|k| **k == kind).count() > 1);
        assert_eq!(now.len(), all.len() - 1, "{kind}: {now:?}");
        let lines = drawn_lines(&view, &mut vcx);
        let line = text[..range.start].matches('\n').count();
        assert!(lines.contains(&line), "{kind} line {line} drawn: {lines:?}");
        caret_at(&view, &mut vcx, text.len());
        assert_eq!(kinds(&view, &mut vcx).len(), all.len(), "{i}");
    }
}

#[gpui::test]
fn a_callout_folds_from_its_title_and_keeps_the_caret_out(cx: &mut TestAppContext) {
    let text = "a\n\n> [!note]- Title\n> body\n> more\n\nb\n";
    let (view, mut vcx) = live(cx, text);
    caret_at(&view, &mut vcx, 0);
    assert_eq!(kinds(&view, &mut vcx), ["callout"]);
    let folded_height = view.read_with(&vcx, |v, _| v.live_block_heights())[0];
    let control = view.read_with(&vcx, |v, cx| {
        let (_, _, reader) = v.live.blocks_drawn.first().cloned().expect("drawn");
        reader.read(cx).fold_controls().first().copied()
    });
    let control = control.expect("a fold toggle");
    press(&mut vcx, control, 1, Modifiers::none());
    settle(&mut vcx);
    // Unfolded: taller, the caret where it was, the editor focused.
    assert_eq!(selections(&view, &mut vcx), [(0, 0)]);
    assert_eq!(kinds(&view, &mut vcx), ["callout"]);
    let open_height = view.read_with(&vcx, |v, _| v.live_block_heights())[0];
    assert!(
        open_height > folded_height + 1.0,
        "{folded_height} {open_height}"
    );
    assert!(vcx.update(|window, cx| view.read(cx).focus.is_focused(window)));
}

#[gpui::test]
fn an_image_on_its_own_line_is_a_block_and_stays_below_its_source(cx: &mut TestAppContext) {
    // 0 1 2         12
    // a\n\n![[pic.png|20]]\n\nb\n
    let text = "a\n\n![[pic.png|20]]\n\nb\n";
    let (view, mut vcx) = live(cx, text);
    caret_at(&view, &mut vcx, 0);
    assert_eq!(blocks(&view, &mut vcx), [("image", 3..18, false)]);
    // No inline widget on that line.
    assert!(probe_of(&view, &mut vcx).widgets.is_empty());
    // The caret on its line: the source shows, the image below it.
    caret_at(&view, &mut vcx, 10);
    assert_eq!(blocks(&view, &mut vcx), [("image", 18..18, true)]);
    assert!(drawn_lines(&view, &mut vcx).contains(&2));
    assert_eq!(drawn_blocks(&view, &mut vcx).len(), 1);
    caret_at(&view, &mut vcx, 0);
    assert_eq!(blocks(&view, &mut vcx), [("image", 3..18, false)]);
    // An image inside a paragraph stays inline.
    let (view, mut vcx) = live(cx, "see ![[pic.png]] here\n");
    assert!(kinds(&view, &mut vcx).is_empty());
}

#[gpui::test]
fn blocks_inside_lists_and_quotes_stay_source(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, "x\n\n- item\n\n  ```\n  code\n  ```\n\n> ---\n");
    caret_at(&view, &mut vcx, 0);
    assert!(kinds(&view, &mut vcx).is_empty());
}

#[gpui::test]
fn the_scroll_stays_put_when_a_height_lands_late(cx: &mut TestAppContext) {
    let mut text = String::from("```\na\nb\n```\n\n");
    for i in 0..200 {
        text.push_str(&format!("line {i}\n"));
    }
    let (view, mut vcx) = live(cx, &text);
    caret_at(&view, &mut vcx, text.len());
    assert_eq!(kinds(&view, &mut vcx), ["code"]);
    // Scroll so that the block is above the view.
    view.update(&mut vcx, |v, cx| v.scroll_to_line(100, cx));
    settle(&mut vcx);
    let first = |vcx: &mut VisualTestContext| {
        let probe = probe_of(&view, vcx);
        let line = probe.row_texts.first().map(|(l, _)| *l).expect("drawn");
        let at = text
            .match_indices('\n')
            .nth(line.saturating_sub(1))
            .map_or(0, |(i, _)| i + 1);
        let y = view.update_in(vcx, |v, window, cx| {
            v.range_bounds(at..at, None, window, cx)
                .expect("on screen")
                .top()
        });
        (line, y)
    };
    let before = first(&mut vcx);
    // Its height lands 300 px taller.
    view.update(&mut vcx, |v, cx| {
        let id = v.live_block_ids()[0];
        let now = v.live_block_heights()[0];
        v.block_heights(vec![(id, now + 300.0)], cx);
    });
    draw(&mut vcx);
    let after = first(&mut vcx);
    assert_eq!(before, after);
    assert!(view.read_with(&vcx, |v, _| v.live_block_heights())[0] > 300.0);
}

#[gpui::test]
fn page_down_goes_through_block_heights(cx: &mut TestAppContext) {
    let mut text = String::new();
    for i in 0..15 {
        text.push_str(&format!("p {i}\n\n```\nc\n```\n\n"));
    }
    let (view, mut vcx) = live(cx, &text);
    caret_at(&view, &mut vcx, 0);
    assert_eq!(kinds(&view, &mut vcx).len(), 15);
    vcx.simulate_keystrokes("pagedown");
    settle(&mut vcx);
    let (_, head) = selections(&view, &mut vcx)[0];
    assert!(head > 0, "moved");
    // The caret is in view and on a row a block does not hide (or in a
    // block it entered).
    let probe = probe_of(&view, &mut vcx);
    assert_eq!(probe.carets.len(), 1);
}

#[gpui::test]
fn source_mode_and_plain_text_have_no_blocks(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, CODE);
    assert_eq!(kinds(&view, &mut vcx), ["code"]);
    view.update(&mut vcx, |v, cx| v.set_live_preview(false, cx));
    settle(&mut vcx);
    assert!(kinds(&view, &mut vcx).is_empty());
    assert_eq!(
        view.read_with(&vcx, |v, _| v
            .editor()
            .expect("loaded")
            .blocks()
            .block_count()),
        0
    );
    view.update(&mut vcx, |v, cx| v.set_live_preview(true, cx));
    settle(&mut vcx);
    assert_eq!(kinds(&view, &mut vcx), ["code"]);
    view.update(&mut vcx, |v, cx| v.set_plain(true, cx));
    settle(&mut vcx);
    assert_eq!(
        view.read_with(&vcx, |v, _| v
            .editor()
            .expect("loaded")
            .blocks()
            .block_count()),
        0
    );
}

/// Live Preview keeps its frame budget on a large note with many blocks:
/// the frames that put the whole document's blocks in and the frames of
/// moving the caret through it.
#[gpui::test]
#[ignore = "timing; run with --ignored --nocapture"]
fn blocks_on_a_large_note(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};
    let mut text = String::new();
    let mut i = 0;
    while text.len() < 1 << 20 {
        text.push_str(&format!(
            "## Section {i}\n\nSome **bold** text and a [link](x.md) in paragraph {i}.\n\n"
        ));
        match i % 5 {
            0 => text.push_str("```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n"),
            1 => text.push_str("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n\n"),
            2 => text.push_str("$$\n\\sum_{k=1}^n k^2\n$$\n\n"),
            3 => text.push_str("> [!note] Title\n> body text\n\n"),
            _ => text.push_str("---\n\n"),
        }
        i += 1;
    }
    let blocks_expected = i;
    let (view, mut vcx) = editor_with(cx, &text, obsidian());
    view.update(&mut vcx, |v, cx| v.set_profile("obsidian", cx));
    draw(&mut vcx);
    // The first Live Preview frame: the whole note's blocks go in.
    view.update(&mut vcx, |v, cx| v.set_live_preview(true, cx));
    vcx.run_until_parked();
    let started = Instant::now();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    let first = started.elapsed();
    assert_eq!(
        view.read_with(&vcx, |v, _| v.live_block_ids().len()),
        blocks_expected
    );
    let frame = |vcx: &mut VisualTestContext| {
        vcx.run_until_parked();
        let started = Instant::now();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        started.elapsed()
    };
    let mut window_frames = Vec::new();
    for _ in 0..4 {
        window_frames.push(frame(&mut vcx));
    }
    // Every block from the first frame (the block pass runs before it).
    let in_window = view.read_with(&vcx, |v, _| v.live_block_ids().len());
    assert_eq!(in_window, blocks_expected);
    vcx.executor()
        .advance_clock(super::super::live_preview::FULL_DELAY * 2);
    vcx.run_until_parked();
    let mut full_frames = Vec::new();
    for _ in 0..80 {
        full_frames.push(frame(&mut vcx));
    }
    let all = view.read_with(&vcx, |v, _| v.live_block_ids().len());
    // Caret moves through blocks (each reveals one and hides another).
    let mut moves = Vec::new();
    for _ in 0..100 {
        vcx.simulate_keystrokes("down");
        moves.push(frame(&mut vcx));
    }
    let ms = |d: &Duration| d.as_secs_f64() * 1e3;
    let max = |v: &[Duration]| v.iter().map(ms).fold(0.0, f64::max);
    let mut sorted: Vec<f64> = moves.iter().map(ms).collect();
    sorted.sort_by(f64::total_cmp);
    println!(
        "note {} bytes, {blocks_expected} blocks; first Live Preview frame {:.2} ms, then max {:.2} ms; \
         whole document: {all} blocks, first frame {:.2} ms, max {:.2} ms; \
         caret moves: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
        text.len(),
        ms(&first),
        max(&window_frames),
        ms(&full_frames[0]),
        max(&full_frames),
        sorted[sorted.len() / 2],
        sorted[sorted.len() * 95 / 100],
        sorted[sorted.len() - 1],
    );
    assert_eq!(all, blocks_expected);
}

#[gpui::test]
fn a_blocks_height_ends_with_its_last_item(cx: &mut TestAppContext) {
    // Callouts with a paragraph that has a margin, a table and code: the
    // Reading view's gaps between items stay, none comes after the last
    // (the rows around the block keep the editor's spacing).
    let text = "a\n\n```rust\nfn a() {}\n```\n\nb\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nc\n\n> [!note] T\n> body\n>\n> more\n\nd\n";
    let (view, mut vcx) = live(cx, text);
    caret_at(&view, &mut vcx, 0);
    assert_eq!(kinds(&view, &mut vcx), ["code", "table", "callout"]);
    let heights = view.read_with(&vcx, |v, _| v.live_block_heights());
    for n in 0..3 {
        let (bounds, reader) = block_reader(&view, &mut vcx, n);
        let (total, bottom) = reader.read_with(&vcx, |r, _| r.height_and_last_bottom());
        assert!((total - bottom).abs() < 0.5, "{n}: {total} {bottom}");
        assert!(
            (heights[n] - bottom).abs() < 0.5,
            "{n}: {heights:?} {bottom}"
        );
        assert!((f32::from(bounds.size.height) - bottom).abs() < 0.5);
    }
}

/// The view of the `n`th block drawn in the last frame.
fn block_reader(
    view: &Entity<EditorView>,
    vcx: &mut VisualTestContext,
    n: usize,
) -> (Bounds<Pixels>, Entity<crate::reader::Reader>) {
    view.read_with(vcx, |v, _| {
        let (_, bounds, reader) = v.live.blocks_drawn[n].clone();
        (bounds, reader)
    })
}

/// A point on a link in `bounds` of a block view, found by scanning.
fn link_point(
    reader: &Entity<crate::reader::Reader>,
    bounds: Bounds<Pixels>,
    vcx: &mut VisualTestContext,
) -> Point<Pixels> {
    reader.read_with(vcx, |r, _| {
        let (x0, y0) = (f32::from(bounds.left()), f32::from(bounds.top()));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let mut y = y0 + 1.0;
        while y < y0 + h {
            let mut x = x0 + 1.0;
            while x < x0 + w {
                let p = point(px(x), px(y));
                if r.link_start_at(p).is_some() {
                    return p;
                }
                x += 3.0;
            }
            y += 3.0;
        }
        panic!("no link in {bounds:?}")
    })
}

const CONTEXT: &str = "$\\newcommand{\\R}{\\mathbb{R}}$ and[^a].\n\n| A | B |\n|---|---|\n| [site] | x[^b] |\n\n$$\n\\R^2\n$$\n\n[site]: https://example.com/\n\n[^a]: A.\n[^b]: B.\n";

#[gpui::test]
fn blocks_resolve_the_notes_definitions_footnotes_and_macros(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, CONTEXT);
    caret_at(&view, &mut vcx, CONTEXT.len());
    assert_eq!(kinds(&view, &mut vcx), ["table", "math"]);
    let (_, table) = block_reader(&view, &mut vcx, 0);
    let (_, math) = block_reader(&view, &mut vcx, 1);
    // The reference link resolves, the footnote has its number in the
    // whole note (2: [^a] comes first), and only the table's own rows show
    // (no footnote section).
    let (links, notes, _, items) = table.read_with(&vcx, |r, _| r.shown_references());
    assert_eq!(links, ["https://example.com/"]);
    assert_eq!(notes, [2]);
    assert_eq!(items, 2, "header and one row");
    // The macro defined before the formula is in force.
    let (_, _, macros, items) = math.read_with(&vcx, |r, _| r.shown_references());
    assert_eq!((macros, items), (vec![1], 1));
    // Editing the definition draws the table again, not the formula.
    let def = CONTEXT.find("example.com/").expect("def") + "example.com/".len();
    caret_at(&view, &mut vcx, def);
    vcx.simulate_input("x");
    settle(&mut vcx);
    assert_eq!(kinds(&view, &mut vcx), ["table", "math"]);
    let (_, table2) = block_reader(&view, &mut vcx, 0);
    let (_, math2) = block_reader(&view, &mut vcx, 1);
    let (links, _, _, _) = table2.read_with(&vcx, |r, _| r.shown_references());
    assert_eq!(links, ["https://example.com/x"]);
    assert_ne!(table2.entity_id(), table.entity_id());
    assert_eq!(math2.entity_id(), math.entity_id());
}

#[gpui::test]
fn mod_click_on_a_link_in_a_block_follows_it_and_a_plain_click_enters(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, CONTEXT);
    view.update(&mut vcx, |v, _| {
        v.path = Some(std::path::PathBuf::from("/vault/Note.md"));
    });
    caret_at(&view, &mut vcx, CONTEXT.len());
    let (bounds, table) = block_reader(&view, &mut vcx, 0);
    let at = link_point(&table, bounds, &mut vcx);
    // Hovering shows it as the editor's hovered link (tooltips, previews).
    vcx.simulate_event(gpui::MouseMoveEvent {
        position: at,
        pressed_button: None,
        modifiers: Modifiers::none(),
    });
    settle(&mut vcx);
    let hovered = view.read_with(&vcx, |v, _| v.hovered_link());
    assert_eq!(
        hovered.map(|l| l.destination.to_string()).as_deref(),
        Some("https://example.com/")
    );
    let secondary = vcx.update(|_, cx| Modifiers::secondary_key(cx));
    press(&mut vcx, at, 1, secondary);
    settle(&mut vcx);
    let followed = view.update(&mut vcx, |v, _| v.take_clicked_link());
    let followed = followed.expect("followed");
    assert_eq!(&*followed.destination, "https://example.com/");
    // The caret stayed out: the table is still drawn.
    assert_eq!(
        selections(&view, &mut vcx),
        [(CONTEXT.len(), CONTEXT.len())]
    );
    assert_eq!(kinds(&view, &mut vcx), ["table", "math"]);
    // A plain click enters the block.
    press(&mut vcx, at, 1, Modifiers::none());
    settle(&mut vcx);
    assert_eq!(kinds(&view, &mut vcx), ["math"]);
    assert!(
        view.update(&mut vcx, |v, _| v.take_clicked_link())
            .is_none()
    );
}

#[gpui::test]
fn sideways_wheel_scrolls_a_wide_code_block_and_vertical_scrolls_the_editor(
    cx: &mut TestAppContext,
) {
    let long = "x".repeat(600);
    let mut text = format!("a\n\n```\n{long}\n```\n\n");
    for i in 0..80 {
        text.push_str(&format!("line {i}\n\n"));
    }
    let (view, mut vcx) = live(cx, &text);
    caret_at(&view, &mut vcx, text.len());
    caret_at(&view, &mut vcx, 0);
    assert_eq!(kinds(&view, &mut vcx), ["code"]);
    let (bounds, code) = block_reader(&view, &mut vcx, 0);
    let at = bounds.center();
    let scroll_y = view.read_with(&vcx, |v, _| v.scroll_y);
    let wheel = |vcx: &mut VisualTestContext, dx: f32, dy: f32, modifiers: Modifiers| {
        vcx.simulate_event(gpui::ScrollEvent {
            position: at,
            delta: gpui::ScrollDelta::Pixels(point(px(dx), px(dy))),
            modifiers,
            ..gpui::ScrollEvent::default()
        });
        settle(vcx);
    };
    // Sideways: the block scrolls, the editor does not.
    wheel(&mut vcx, -40.0, 0.0, Modifiers::none());
    let x = code.read_with(&vcx, |r, _| r.sideways_scroll());
    assert!(x > 30.0, "{x}");
    assert_eq!(view.read_with(&vcx, |v, _| v.scroll_y), scroll_y);
    // Shift with a vertical turn: sideways too.
    wheel(&mut vcx, 0.0, -20.0, Modifiers::shift());
    let x2 = code.read_with(&vcx, |r, _| r.sideways_scroll());
    assert!(x2 > x + 10.0, "{x} {x2}");
    // A vertical turn scrolls the editor.
    wheel(&mut vcx, 0.0, -60.0, Modifiers::none());
    assert!(view.read_with(&vcx, |v, _| v.scroll_y) > scroll_y + 50.0);
    assert_eq!(code.read_with(&vcx, |r, _| r.sideways_scroll()), x2);
}

#[gpui::test]
fn blocks_are_described_for_assistive_technology(cx: &mut TestAppContext) {
    let table = "| A | B |\n|---|---|\n| 1 | 2 |";
    let text = format!("a\n\n{table}\n\nb\n\n{table}\n\n![alt text](missing.png)\n\nc\n");
    let (view, mut vcx) = live(cx, &text);
    caret_at(&view, &mut vcx, 0);
    assert_eq!(kinds(&view, &mut vcx), ["table", "table", "image"]);
    let described = view.read_with(&vcx, |v, cx| v.blocks_a11y(cx));
    assert_eq!(described.len(), 3);
    let roles: Vec<Vec<gpui::Role>> = described
        .iter()
        .map(|(_, _, nodes)| nodes.iter().map(|n| n.role).collect())
        .collect();
    assert_eq!(roles[0], [gpui::Role::Table]);
    assert_eq!(roles[1], [gpui::Role::Table]);
    // Rows and cells under the table.
    assert!(!described[0].2[0].children.is_empty());
    // The same table twice: one view, but distinct node keys.
    assert_ne!(described[0].2[0].key, described[1].2[0].key);
    // In order down the page.
    assert!(described[0].0 < described[1].0 && described[1].0 < described[2].0);
    // The image by its alt text.
    fn labels(nodes: &[crate::a11y::document::DocNode], out: &mut Vec<String>) {
        for n in nodes {
            out.extend(n.label.clone());
            labels(&n.children, out);
        }
    }
    let mut image = Vec::new();
    labels(&described[2].2, &mut image);
    assert!(image.iter().any(|l| l.contains("alt text")), "{image:?}");
}

#[gpui::test]
fn unmeasured_blocks_are_estimated_from_measured_ones(cx: &mut TestAppContext) {
    let mut text = String::new();
    for i in 0..60 {
        text.push_str(&format!("para {i}\n\n```\none\ntwo\nthree\n```\n\n"));
    }
    let (view, mut vcx) = live(cx, &text);
    caret_at(&view, &mut vcx, 0);
    // Every block is in the layer from the first frame (a short note).
    let heights = view.read_with(&vcx, |v, _| v.live_block_heights());
    assert_eq!(heights.len(), 60);
    // Those far below were never drawn; they take the measured height.
    let measured = heights[0];
    assert!((measured - 5.0 * LINE).abs() > 1.0, "{heights:?}");
    assert!((heights[59] - measured).abs() < 0.5, "{heights:?}");
    // Scrolling to the end changes the content height by little.
    let total = view.update_in(&mut vcx, |v, _, _| v.geo().total(0));
    caret_at(&view, &mut vcx, text.len());
    let after = view.update_in(&mut vcx, |v, _, _| v.geo().total(0));
    assert!((after - total).abs() < 1.0, "{total} {after}");
}

#[gpui::test]
#[ignore = "timing; run with --ignored --nocapture"]
fn first_frame_of_a_short_note(cx: &mut TestAppContext) {
    use std::time::Instant;
    let mut text = String::from("$\\newcommand{\\R}{\\mathbb{R}}$ macros.\n\n");
    let mut i = 0;
    while text.len() < super::super::live_preview::SMALL_NOTE_BYTES - 400 {
        text.push_str(&format!(
            "## Section {i}\n\nSome **bold** text, a [ref][r{i}] and a note[^n{i}] in paragraph {i}.\n\n"
        ));
        match i % 4 {
            0 => text.push_str("```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n"),
            1 => text.push_str(&format!(
                "| a | b |\n|---|---|\n| [ref][r{i}] | x[^n{i}] |\n\n"
            )),
            2 => text.push_str("$$\n\\R^2\n$$\n\n"),
            _ => text.push_str("> [!note] Title\n> body text\n\n"),
        }
        text.push_str(&format!(
            "[r{i}]: https://example.com/{i}\n\n[^n{i}]: Note {i}.\n\n"
        ));
        i += 1;
    }
    let (view, mut vcx) = editor_with(cx, &text, obsidian());
    view.update(&mut vcx, |v, cx| v.set_profile("obsidian", cx));
    draw(&mut vcx);
    let mut frames = Vec::new();
    for _ in 0..5 {
        view.update(&mut vcx, |v, cx| v.set_live_preview(true, cx));
        vcx.run_until_parked();
        let started = Instant::now();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        frames.push(started.elapsed().as_secs_f64() * 1e3);
        let blocks = view.read_with(&vcx, |v, _| v.live_block_ids().len());
        assert_eq!(blocks, i, "every block from the first frame");
        view.update(&mut vcx, |v, cx| v.set_live_preview(false, cx));
        draw(&mut vcx);
    }
    println!(
        "note {} bytes, {i} blocks; first Live Preview frame: {:?} ms",
        text.len(),
        frames
            .iter()
            .map(|f| (f * 100.0).round() / 100.0)
            .collect::<Vec<_>>()
    );
}

/// The first line drawn and the y of its start.
fn top_line(view: &Entity<EditorView>, vcx: &mut VisualTestContext, text: &str) -> (usize, Pixels) {
    let probe = probe_of(view, vcx);
    let line = probe.row_texts.first().map(|(l, _)| *l).expect("drawn");
    let at = text
        .match_indices('\n')
        .nth(line.saturating_sub(1))
        .map_or(0, |(i, _)| i + 1);
    let y = view.update_in(vcx, |v, window, cx| {
        v.range_bounds(at..at, None, window, cx)
            .expect("on screen")
            .top()
    });
    (line, y)
}

#[gpui::test]
fn a_long_note_has_its_blocks_from_the_first_frame_and_keeps_its_top_line(cx: &mut TestAppContext) {
    let mut text = String::new();
    let mut n = 0;
    while text.len() <= super::super::live_preview::SMALL_NOTE_BYTES + 1000 {
        text.push_str(&format!(
            "para {n} with some words in it\n\n```\none\ntwo\nthree\n```\n\n"
        ));
        n += 1;
    }
    let (view, mut vcx) = editor_with(cx, &text, obsidian());
    view.update(&mut vcx, |v, cx| {
        v.set_profile("obsidian", cx);
        v.set_live_preview(true, cx);
    });
    draw(&mut vcx);
    assert_eq!(view.read_with(&vcx, |v, _| v.live_block_ids().len()), n);
    // The blocks at the top are measured (not as tall as their source).
    settle(&mut vcx);
    let heights = view.read_with(&vcx, |v, _| v.live_block_heights());
    assert!((heights[0] - 5.0 * LINE).abs() > 1.0, "{heights:?}");
    // Back in Source, scrolled to the middle: turning Live Preview on puts
    // every block above the view in at its estimated height, and the line
    // at the top stays where it is.
    view.update(&mut vcx, |v, cx| v.set_live_preview(false, cx));
    settle(&mut vcx);
    view.update(&mut vcx, |v, cx| v.scroll_to_line(n * 3, cx));
    settle(&mut vcx);
    let before = top_line(&view, &mut vcx, &text);
    assert!(before.0 >= n * 3 - 1, "{before:?}");
    view.update(&mut vcx, |v, cx| v.set_live_preview(true, cx));
    draw(&mut vcx);
    assert_eq!(view.read_with(&vcx, |v, _| v.live_block_ids().len()), n);
    assert_eq!(top_line(&view, &mut vcx, &text).0, before.0);
    // The background pass landing changes nothing on screen.
    vcx.executor()
        .advance_clock(super::super::live_preview::FULL_DELAY * 2);
    vcx.run_until_parked();
    settle(&mut vcx);
    assert_eq!(top_line(&view, &mut vcx, &text).0, before.0);
    assert_eq!(view.read_with(&vcx, |v, _| v.live_block_ids().len()), n);
}

#[gpui::test]
fn mod_click_on_an_embeds_link_follows_the_embedded_note(cx: &mut TestAppContext) {
    // The embed's own link goes to the note.
    let vault = super::autocomplete::vault(&[
        ("Host.md", "intro\n\n![[Other]]\n\nend\n"),
        ("Sub/Other.md", "See [near](Near.md) here.\n"),
    ]);
    let (view, mut vcx) =
        super::autocomplete::editor_in(cx, &vault, "Host.md", gv_profile::ProfileId::Obsidian);
    view.update(&mut vcx, |v, cx| {
        v.set_profile("obsidian", cx);
        v.set_live_preview(true, cx);
    });
    settle(&mut vcx);
    vcx.run_until_parked();
    settle(&mut vcx);
    assert_eq!(kinds(&view, &mut vcx), ["embed"]);
    let (bounds, embed) = block_reader(&view, &mut vcx, 0);
    let at = link_point(&embed, bounds, &mut vcx);
    let secondary = vcx.update(|_, cx| Modifiers::secondary_key(cx));
    press(&mut vcx, at, 1, secondary);
    settle(&mut vcx);
    let followed = view
        .update(&mut vcx, |v, _| v.take_clicked_link())
        .expect("followed");
    assert_eq!(followed.file, vault.root.join("Host.md"));
    assert_eq!(&*followed.destination, "Other");
    // The caret stayed out of the block.
    assert_eq!(kinds(&view, &mut vcx), ["embed"]);
}

#[gpui::test]
fn mod_click_on_a_link_inside_an_embed_follows_it_from_the_embedded_note(cx: &mut TestAppContext) {
    let vault = super::autocomplete::vault(&[
        ("Host.md", "intro\n\n![[Other]]\n\nend\n"),
        ("Sub/Other.md", "See [near](Near.md) here.\n"),
        ("Sub/Near.md", "# Near\n"),
    ]);
    let (view, mut vcx) =
        super::autocomplete::editor_in(cx, &vault, "Host.md", gv_profile::ProfileId::Obsidian);
    view.update(&mut vcx, |v, cx| {
        v.set_profile("obsidian", cx);
        v.set_live_preview(true, cx);
    });
    settle(&mut vcx);
    vcx.run_until_parked();
    settle(&mut vcx);
    assert_eq!(kinds(&view, &mut vcx), ["embed"]);
    let (bounds, embed) = block_reader(&view, &mut vcx, 0);
    // The link inside the embedded note is not in the host's text.
    let at = embed.read_with(&vcx, |r, _| {
        let (x0, y0) = (f32::from(bounds.left()), f32::from(bounds.top()));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let mut y = y0 + 1.0;
        while y < y0 + h {
            let mut x = x0 + 1.0;
            while x < x0 + w {
                let p = point(px(x), px(y));
                if r.embedded_link_at(p).is_some() {
                    return p;
                }
                x += 3.0;
            }
            y += 3.0;
        }
        panic!("no embedded link in {bounds:?}")
    });
    let secondary = vcx.update(|_, cx| Modifiers::secondary_key(cx));
    press(&mut vcx, at, 1, secondary);
    settle(&mut vcx);
    let followed = view
        .update(&mut vcx, |v, _| v.take_clicked_link())
        .expect("followed");
    // Followed from the embedded note: `Near.md` beside it.
    assert_eq!(followed.file, vault.root.join("Sub/Other.md"));
    assert_eq!(&*followed.destination, "Near.md");
    assert_eq!(followed.range, None);
    let nav = crate::reader::navigation(
        &followed.file,
        None,
        followed.profile,
        followed.kind,
        &followed.destination,
    );
    assert!(
        matches!(&nav, crate::reader::Navigation::Open { path, .. } if path.ends_with("Sub/Near.md")),
        "{nav:?}"
    );
    // A plain click does not follow it.
    press(&mut vcx, at, 1, Modifiers::none());
    settle(&mut vcx);
    assert!(
        view.update(&mut vcx, |v, _| v.take_clicked_link())
            .is_none()
    );
}
