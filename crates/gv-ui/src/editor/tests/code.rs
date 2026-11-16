// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Fenced code highlighting in the Source view, in headless windows: a
//! frame never highlights (jobs run on the background executor; one that
//! runs inside a frame fails the test), a fence is drawn plain until its
//! result lands and is drawn, an edit in a fence re-highlights that fence
//! only and keeps its colours meanwhile, and unknown languages stay plain.
//!
//! Each test uses code of its own: the highlighter's cache is shared by
//! the whole process.

use super::*;
use crate::editor::highlight::Style;
use crate::reader::code_highlight::tests::{job_texts, jobs_run, pending_draws};

/// The style at the first `needle` on line `line`, as last highlighted.
fn style_at(
    view: &Entity<EditorView>,
    vcx: &mut VisualTestContext,
    line: usize,
    needle: &str,
) -> Style {
    view.read_with(vcx, |v, _| {
        let doc = v.doc.as_ref().expect("loaded");
        let text = doc.editor.text();
        let range = text.line_range(line).expect("line");
        let content = text.text_for_range(range).expect("text");
        let col = content
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} on line {line}"));
        doc.highlights
            .get(line)
            .unwrap_or_else(|| panic!("line {line} highlighted"))
            .style_at(col)
    })
}

const TEXT: &str = "Intro\n\n```rust\nfn source_view_fence() -> &'static str { \"s\" }\n```\n\n```nosuchlanguage\nfn plain() {}\n```\n\n```python\ndef source_view_other(): pass\n```\n";

#[gpui::test]
fn fences_are_drawn_plain_then_highlighted_when_their_job_lands(cx: &mut TestAppContext) {
    let (jobs, pending) = (jobs_run(), pending_draws());
    // Frames run no job (a job inside a frame fails the test); the first
    // frame draws the fences plain and asks for two jobs (two known
    // languages), which land, and the view draws again.
    let (view, mut vcx) = editor(cx, TEXT);
    draw(&mut vcx);
    assert_eq!(jobs_run(), jobs + 2);
    assert!(pending_draws() >= pending + 2, "drawn plain while pending");
    assert_eq!(style_at(&view, &mut vcx, 3, "fn").color, Token::SyntaxTag);
    assert_eq!(
        style_at(&view, &mut vcx, 3, "source_view_fence").color,
        Token::SyntaxKey
    );
    assert_eq!(
        style_at(&view, &mut vcx, 3, "\"s\"").color,
        Token::SyntaxString
    );
    assert_eq!(style_at(&view, &mut vcx, 11, "def").color, Token::SyntaxTag);
    // The fence itself stays Markdown's; an unknown language stays plain.
    assert_eq!(style_at(&view, &mut vcx, 2, "```"), Style::MARKER);
    assert_eq!(style_at(&view, &mut vcx, 7, "fn"), Style::PLAIN);
    // Nothing more to do.
    draw(&mut vcx);
    assert_eq!(jobs_run(), jobs + 2);
}

#[gpui::test]
fn typing_in_a_fence_rehighlights_that_fence_only(cx: &mut TestAppContext) {
    let text = TEXT
        .replace("source_view_fence", "typing_in_fence")
        .replace("source_view_other", "typing_other");
    let (view, mut vcx) = editor(cx, &text);
    draw(&mut vcx);
    let (jobs, pending) = (jobs_run(), pending_draws());
    // Type at the end of the Rust line.
    let at = text.find(" }\n```").expect("rust line") + 2;
    view.update(&mut vcx, |v, _| {
        v.doc.as_mut().expect("loaded").editor.set_cursor(at);
    });
    vcx.simulate_input(" // more");
    draw(&mut vcx);
    draw(&mut vcx);
    // Jobs for the Rust fence only, and meanwhile it kept its previous
    // colours (never drawn plain).
    let texts = job_texts(jobs);
    assert!(!texts.is_empty());
    assert!(
        texts.iter().all(|t| t.contains("typing_in_fence")),
        "{texts:?}"
    );
    assert_eq!(pending_draws(), pending, "never drawn plain");
    assert_eq!(style_at(&view, &mut vcx, 3, "fn").color, Token::SyntaxTag);
    let comment = style_at(&view, &mut vcx, 3, "// more");
    assert!(comment.italic && comment.color == Token::TextTertiary);
    assert_eq!(style_at(&view, &mut vcx, 11, "def").color, Token::SyntaxTag);
}
