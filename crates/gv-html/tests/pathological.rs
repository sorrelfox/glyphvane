// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Hostile and huge inputs stay bounded: the pre-parse guards reduce them
//! to text, the walker and the emitter cap nesting, and nothing recurses
//! without a limit.

use std::time::{Duration, Instant};

use gv_html::{GuardTrip, PasteOptions, Profile, convert_html, html_to_markdown};

/// Generous for unoptimized test builds on a slow CI machine; the real
/// cost is a small fraction of this.
const BUDGET: Duration = Duration::from_secs(20);

fn timed(html: &str) -> (gv_html::Conversion, Duration) {
    let t = Instant::now();
    let c = convert_html(html, PasteOptions::new(Profile::GfmFile));
    (c, t.elapsed())
}

#[test]
fn deep_nesting_is_reduced_to_text() {
    let html = format!(
        "{}deep{}",
        "<div>".repeat(100_000),
        "</div>".repeat(100_000)
    );
    let (c, took) = timed(&html);
    assert_eq!(c.guard, Some(GuardTrip::TooDeep));
    assert_eq!(c.markdown, "deep");
    assert!(took < BUDGET, "{took:?}");

    let html = format!("{}x", "<b><i>".repeat(5_000));
    let (c, took) = timed(&html);
    assert_eq!(c.guard, Some(GuardTrip::TooDeep));
    assert_eq!(c.markdown, "x");
    assert!(took < BUDGET, "{took:?}");

    let html = format!("{}item", "<ul><li>".repeat(1_000));
    let (c, _) = timed(&html);
    assert_eq!(c.guard, Some(GuardTrip::TooDeep));
    assert_eq!(c.markdown, "item");
}

#[test]
fn many_attributes_and_huge_tags() {
    let attrs: String = (0..20_000).map(|i| format!(" a{i}=\"v\"")).collect();
    let (c, took) = timed(&format!("<p{attrs}>text</p>"));
    assert_eq!(c.guard, Some(GuardTrip::TooManyAttributes));
    assert_eq!(c.markdown, "text");
    assert!(took < BUDGET, "{took:?}");

    let (c, _) = timed(&format!("<p title=\"{}\">text</p>", "x".repeat(200_000)));
    assert_eq!(c.guard, Some(GuardTrip::TagTooLong));
    assert_eq!(c.markdown, "text");

    // Under the limits: parsed normally.
    let attrs: String = (0..900).map(|i| format!(" a{i}=\"v\"")).collect();
    let (c, _) = timed(&format!("<p{attrs}><b>text</b></p>"));
    assert_eq!(c.guard, None);
    assert_eq!(c.markdown, "**text**");
}

#[test]
fn oversized_input_is_reduced_to_text() {
    let html = format!("<p>{}</p>", "word ".repeat(2_000_000));
    let (c, took) = timed(&html);
    assert_eq!(c.guard, Some(GuardTrip::TooLarge));
    assert!(c.markdown.starts_with("word word"));
    assert!(took < BUDGET, "{took:?}");
}

#[test]
fn nesting_below_the_guard_is_capped() {
    // 200 quotes: parsed, but at most 24 levels of `>` are written.
    let html = format!(
        "{}q{}",
        "<blockquote>".repeat(200),
        "</blockquote>".repeat(200)
    );
    let (c, _) = timed(&html);
    assert_eq!(c.guard, None);
    let line = c
        .markdown
        .lines()
        .find(|l| l.ends_with('q'))
        .expect("text kept");
    assert!(line.matches('>').count() <= 24, "{line}");

    // 120 nested lists (240 elements deep): walker and container caps.
    let html = format!(
        "{}leaf{}",
        "<ul><li>x".repeat(120),
        "</li></ul>".repeat(120)
    );
    let (c, took) = timed(&html);
    assert_eq!(c.guard, None);
    assert!(c.markdown.contains("leaf"));
    let widest = c.markdown.lines().map(str::len).max().unwrap_or(0);
    assert!(widest < 200, "{widest}");
    assert!(took < BUDGET, "{took:?}");

    // 400 nested spans: deeper than the walker goes, text kept.
    let html = format!(
        "{}<b>deep</b>{}",
        "<span>".repeat(400),
        "</span>".repeat(400)
    );
    let (c, _) = timed(&html);
    assert_eq!(c.guard, None);
    assert!(c.markdown.contains("deep"));
}

#[test]
fn wide_inputs_stay_linear() {
    // 20,000 intra-word bold runs: every delimiter falls back to HTML.
    let html = format!("<p>{}</p>", "a<b>b</b>".repeat(20_000));
    let (c, took) = timed(&html);
    assert!(c.markdown.starts_with("a<strong>b</strong>a"));
    assert!(took < BUDGET, "{took:?}");

    // A 5,000-row table.
    let rows = "<tr><td>a</td><td><i>b</i></td></tr>".repeat(5_000);
    let (c, took) = timed(&format!("<table>{rows}</table>"));
    assert_eq!(c.markdown.lines().count(), 5_001);
    assert!(took < BUDGET, "{took:?}");

    // Misnested formatting (adoption agency) and unclosed paragraphs.
    let (c, took) = timed(&"<a href=\"https://x.org/\"><b><a>m</a></b>".repeat(3_000));
    assert!(c.markdown.contains('m'));
    assert!(took < BUDGET, "{took:?}");
    let (c, took) = timed(&"<p>para".repeat(20_000));
    assert!(c.markdown.starts_with("para\n\npara"));
    assert!(took < BUDGET, "{took:?}");

    // Comments and Word conditionals in bulk.
    let html = format!(
        "<p>start</p>{}",
        "<!--[if !supportLists]-->x<!--[endif]-->".repeat(10_000)
    );
    let (c, _) = timed(&html);
    assert_eq!(c.markdown, "start");
}

#[test]
fn degenerate_inputs() {
    let p = PasteOptions::new(Profile::CommonMark);
    for html in [
        "",
        "<",
        "</",
        "<!--",
        "<p",
        "&",
        "&#",
        "&#xD800;",
        "\0",
        "<a href>",
        "<img>",
        "<table>",
        "<ol start=-5><li>x",
    ] {
        let _ = html_to_markdown(html, p);
    }
    assert_eq!(
        html_to_markdown("<ol start=\"99999999999\"><li>x</li></ol>", p),
        "999999999. x"
    );
    assert_eq!(html_to_markdown("<td colspan=99999>x", p), "x");
}
