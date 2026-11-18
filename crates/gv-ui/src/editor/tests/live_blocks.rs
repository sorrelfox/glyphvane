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
    let (view, mut vcx) = live(cx, &text);
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
    let in_window = view.read_with(&vcx, |v, _| v.live_block_ids().len());
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
        "note {} bytes, {blocks_expected} blocks; window: {in_window} blocks, frames max {:.2} ms; \
         whole document: {all} blocks, first frame {:.2} ms, max {:.2} ms; \
         caret moves: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
        text.len(),
        max(&window_frames),
        ms(&full_frames[0]),
        max(&full_frames),
        sorted[sorted.len() / 2],
        sorted[sorted.len() * 95 / 100],
        sorted[sorted.len() - 1],
    );
    assert_eq!(all, blocks_expected);
}
