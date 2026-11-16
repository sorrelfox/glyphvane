// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Pathological inputs: every shape is rendered at size n and 2n, and the
//! time ratio, normalized by the byte ratio, must stay below 2.5 (linear
//! time). Where the expected output is known it is checked too.
//!
//! Two measures keep cache and memory effects from passing for
//! superlinear time:
//!
//! - **Growth cap.** Inputs start at cmark's sizes and are grown (x4) only
//!   until the 2n run takes about 5 ms, and never beyond 1 MB at 2n: larger
//!   delimiter-dense paragraphs have working sets of 100 MB and more, which
//!   spill out of the CPU caches and inflate the 2n time by 1.3-1.8x with
//!   no algorithmic cause.
//! - **Confirmation over an 8x step.** A ratio at or above the limit is
//!   confirmed by running the shape at n/4 and 2n (same cap): the case
//!   fails only if the growth exponent
//!   `log(t(2n) / t(n/4)) / log(bytes(2n) / bytes(n/4))` is at least 1.5
//!   (linear time gives 1, quadratic time 2; a cache penalty of 1.8x over
//!   the 8x step gives 1.28).
//!
//! Run in release mode with `cargo xtask pathological` (or
//! `--engine comrak` to validate the suite itself against comrak). Both
//! engine tests are ignored by default because timings in a debug build or
//! on a busy machine mean little. Environment variables:
//! `GV_PATHOLOGICAL_FILTER` (substring of case names) and
//! `GV_PATHOLOGICAL_MAX_RATIO` (default 2.5; 3.0 for comrak).
//!
//! Sources of the shapes:
//! - `cmark:` cases are cmark's `test/pathological_tests.py` (0.31), with
//!   the same 2n sizes and output checks;
//! - `block:` cases stress the block pass (deep containers, long lines,
//!   unclosed fences and comments, long lists, reference definitions);
//! - `gfm:` cases run through the GFM renderer with every extension on
//!   (long and wide tables, delimiter-like lines, tilde runs, autolink
//!   triggers); the comrak engine skips them.
//!
//! TODO(profiles): the other profile-specific extras of the test plan (10k
//! `[[` on one line, an unclosed `%%` at the top, 10k footnotes, 1 MB front
//! matter, 10k `$` signs, embed fan-out) need the Obsidian renderer; add
//! them as `profile:` cases when it exists.

mod support;

use std::fmt::Write as _;
use std::time::Instant;

use gv_syntax::harness::{IS_PLACEHOLDER, render_commonmark_html, render_gfm_html};

type Make = fn(usize) -> String;
type Check = fn(&str, usize) -> bool;

struct Case {
    name: &'static str,
    /// Starting size parameter n; the case also runs at 2n. For fast
    /// engines n grows (×4 steps, up to ×64) until 2n takes a few ms.
    n: usize,
    make: Make,
    /// Checks the output at 2n.
    check: Option<Check>,
    /// Why comrak fails this case (its failures are then reported, not
    /// asserted). gv-syntax must pass every case.
    comrak_known: Option<&'static str>,
}

const fn case(name: &'static str, n: usize, make: Make, check: Option<Check>) -> Case {
    Case {
        name,
        n,
        make,
        check,
        comrak_known: None,
    }
}

/// Inputs whose size does not scale: output check only.
struct Fixed {
    name: &'static str,
    input: &'static str,
    check: fn(&str) -> bool,
}

// ----------------------------------------------------------------------------
// Output matchers for cmark's regular expressions.

/// `^(<\w+>\n?)+INNER(</\w+>\n)+$`
fn wrapped_in_tags(out: &str, inner: &str) -> bool {
    let Some(i) = out.find(inner) else {
        return false;
    };
    open_tags(&out[..i], true) && close_tags(&out[i + inner.len()..])
}

/// One or more `<word>` tags, each followed by a newline (optional if
/// `newline_optional`), and nothing else.
fn open_tags(mut s: &str, newline_optional: bool) -> bool {
    let mut count = 0;
    while !s.is_empty() {
        let Some(rest) = s.strip_prefix('<') else {
            return false;
        };
        let word = rest
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        let Some(rest) = rest[word..].strip_prefix('>') else {
            return false;
        };
        if word == 0 {
            return false;
        }
        s = match rest.strip_prefix('\n') {
            Some(r) => r,
            None if newline_optional => rest,
            None => return false,
        };
        count += 1;
    }
    count > 0
}

/// `(</\w+>\n)+$`
fn close_tags(mut s: &str) -> bool {
    let mut count = 0;
    while !s.is_empty() {
        let Some(rest) = s.strip_prefix("</") else {
            return false;
        };
        let word = rest
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        let Some(rest) = rest[word..].strip_prefix(">\n") else {
            return false;
        };
        if word == 0 {
            return false;
        }
        s = rest;
        count += 1;
    }
    count > 0
}

// ----------------------------------------------------------------------------
// Generators.

/// cmark's `hash_collisions()`: `count` reference labels that collide in
/// cmark's old 16-bucket hash, each defined and one label referenced
/// `count - 1` times.
fn reference_collisions(count: usize) -> (String, String) {
    fn bad_hash(s: &str) -> bool {
        let mut h: u32 = 0;
        for c in s.chars() {
            h = u32::from(c)
                .wrapping_add(h << 6)
                .wrapping_add(h << 16)
                .wrapping_sub(h);
        }
        h.is_multiple_of(16)
    }
    let mut keys = (0..)
        .map(|i| format!("x{i}"))
        .filter(|k| bad_hash(k))
        .take(count);
    let bad_key = keys.next().unwrap_or_default();
    let mut doc = String::new();
    for key in keys {
        let _ = write!(doc, "[{key}]: /url\n\n[{bad_key}]\n\n");
    }
    (doc, bad_key)
}

/// A list nested `depth` levels, built 99 levels per line (cmark and comrak
/// open at most 100 per line), then `blank_lines` blank lines.
fn deep_list_then_blank_lines(depth: usize, blank_lines: usize) -> String {
    let mut s = String::new();
    let mut d = 0;
    while d < depth {
        s.push_str(&" ".repeat(2 * d));
        let k = 99.min(depth - d);
        s.push_str(&"- ".repeat(k));
        s.push_str("x\n");
        d += k;
    }
    s.push_str(&"\n".repeat(blank_lines));
    s
}

fn nested_list_by_indent(depth: usize) -> String {
    (0..depth)
        .map(|x| format!("{}* a\n", "  ".repeat(x)))
        .collect()
}

fn cases() -> Vec<Case> {
    vec![
        // cmark: test/pathological_tests.py. `n` is half of cmark's size.
        case(
            "cmark: nested strong emph",
            16_250,
            |k| format!("{}b{}", "*a **a ".repeat(k), " a** a*".repeat(k)),
            Some(|o, k| {
                o.contains(&format!(
                    "{}b{}",
                    "<em>a <strong>a ".repeat(k),
                    " a</strong> a</em>".repeat(k)
                ))
            }),
        ),
        case(
            "cmark: many emph closers with no openers",
            16_250,
            |k| "a_ ".repeat(k),
            Some(|o, k| o.contains(&format!("{}a_", "a_ ".repeat(k - 1)))),
        ),
        case(
            "cmark: many emph openers with no closers",
            16_250,
            |k| "_a ".repeat(k),
            Some(|o, k| o.contains(&format!("{}_a", "_a ".repeat(k - 1)))),
        ),
        case(
            "cmark: many link closers with no openers",
            16_250,
            |k| "a]".repeat(k),
            Some(|o, k| o.contains(&"a]".repeat(k))),
        ),
        case(
            "cmark: many link openers with no closers",
            16_250,
            |k| "[a".repeat(k),
            Some(|o, k| o.contains(&"[a".repeat(k))),
        ),
        case(
            "cmark: mismatched openers and closers",
            12_500,
            |k| "*a_ ".repeat(k),
            Some(|o, k| o.contains(&format!("{}*a_", "*a_ ".repeat(k - 1)))),
        ),
        case(
            "cmark: issue #389 (emph openers, then closers in links)",
            10_000,
            |k| format!("{}{}", "*a ".repeat(k), "_a*_ ".repeat(k)),
            Some(|o, k| {
                o.contains(&format!(
                    "{}{}_a</em>_",
                    "<em>a ".repeat(k),
                    "_a</em>_ ".repeat(k - 1)
                ))
            }),
        ),
        case(
            "cmark: openers and closers multiple of 3",
            12_500,
            |k| format!("a**b{}", "c* ".repeat(k)),
            Some(|o, k| o.contains(&format!("a**b{}c*", "c* ".repeat(k - 1)))),
        ),
        case(
            "cmark: link openers and emph closers",
            12_500,
            |k| "[ a_".repeat(k),
            Some(|o, k| o.contains(&"[ a_".repeat(k))),
        ),
        case(
            "cmark: pattern [ (]( repeated",
            20_000,
            |k| "[ (](".repeat(k),
            Some(|o, k| o.contains(&"[ (](".repeat(k))),
        ),
        case(
            "cmark: pattern ![[]() repeated",
            80_000,
            |k| "![[]()".repeat(k),
            Some(|o, k| o.contains(&"![<a href=\"\"></a>".repeat(k))),
        ),
        case(
            "cmark: nested brackets",
            12_500,
            |k| format!("{}a{}", "[".repeat(k), "]".repeat(k)),
            Some(|o, k| o.contains(&format!("{}a{}", "[".repeat(k), "]".repeat(k)))),
        ),
        case(
            "cmark: nested block quotes",
            12_500,
            |k| format!("{}a", "> ".repeat(k)),
            Some(|o, k| o.contains(&"<blockquote>\n".repeat(k))),
        ),
        case(
            "cmark: deeply nested lists",
            250,
            nested_list_by_indent,
            Some(|o, k| {
                o.contains(&format!(
                    "<ul>\n{}<li>a</li>\n</ul>\n{}",
                    "<li>a\n<ul>\n".repeat(k - 1),
                    "</li>\n</ul>\n".repeat(k - 1)
                ))
            }),
        ),
        case(
            "cmark: backticks",
            1_250,
            |k| (1..k).map(|x| format!("e{}", "`".repeat(x))).collect(),
            Some(|o, _| {
                o.strip_prefix("<p>")
                    .and_then(|r| r.strip_suffix("</p>\n"))
                    .is_some_and(|mid| mid.bytes().all(|b| b == b'e' || b == b'`'))
            }),
        ),
        case(
            "cmark: unclosed links A",
            15_000,
            |k| "[a](<b".repeat(k),
            Some(|o, k| o.contains(&"[a](&lt;b".repeat(k))),
        ),
        case(
            "cmark: unclosed links B",
            15_000,
            |k| "[a](b".repeat(k),
            Some(|o, k| o.contains(&"[a](b".repeat(k))),
        ),
        case(
            "cmark: unclosed <!--",
            150_000,
            |k| format!("</{}", "<!--".repeat(k)),
            Some(|o, k| o.contains(&format!("&lt;/{}", "&lt;!--".repeat(k)))),
        ),
        Case {
            comrak_known: Some(
                "comrak opens at most 100 list items per line (divergence list-starts-per-line-cap)",
            ),
            ..case(
                "cmark: empty lines in deeply nested lists",
                15_000,
                |k| format!("{}x{}", "- ".repeat(k), "\n".repeat(k)),
                Some(|o, _| wrapped_in_tags(o, "x")),
            )
        },
        Case {
            comrak_known: Some(
                "comrak opens at most 100 list items per line (divergence list-starts-per-line-cap)",
            ),
            ..case(
                "cmark: empty lines in deeply nested lists in blockquote",
                15_000,
                |k| format!("> {}x\n{}", "- ".repeat(k), ">\n".repeat(k)),
                Some(|o, _| wrapped_in_tags(o, "x")),
            )
        },
        case(
            "cmark: emph in deep blockquote",
            50_000,
            |k| format!("{}{}", ">".repeat(k), "a*".repeat(k)),
            Some(|o, _| {
                let Some(p) = o.find("<p>") else { return false };
                let Some(end) = o[p..].find("</p>\n").map(|e| p + e) else {
                    return false;
                };
                open_tags(&o[..p], false)
                    && !o[p..end].contains('\n')
                    && close_tags(&o[end + "</p>\n".len()..])
            }),
        ),
        case(
            "cmark: reference collisions",
            12_500,
            |k| reference_collisions(k).0,
            Some(|o, k| {
                let (_, bad) = reference_collisions(k);
                o.contains(&format!("<p>[{bad}]</p>\n").repeat(k - 1))
            }),
        ),
        case(
            "cmark: nested inlines",
            10_000,
            |k| format!("{}a{}", "*".repeat(k), "*".repeat(k)),
            None,
        ),
        // Block pass.
        case(
            "block: `> ` repeated on one line",
            100_000,
            |k| format!("{}a\n", "> ".repeat(k)),
            None,
        ),
        case(
            "block: deep quote, then lazy lines",
            200_000,
            |k| format!("{} a\n{}", ">".repeat(10_000), "b\n".repeat(k)),
            None,
        ),
        case(
            "block: 100-deep quote on every line",
            2_500,
            |k| format!("{} a\n", ">".repeat(100)).repeat(k),
            None,
        ),
        case(
            "block: deeply nested list, 700 levels",
            700,
            nested_list_by_indent,
            None,
        ),
        case(
            "block: list markers on one line, then blank lines",
            100_000,
            |k| format!("{}x{}", "- ".repeat(k), "\n".repeat(k)),
            None,
        ),
        Case {
            comrak_known: Some("blank lines continue every open item: O(depth) each"),
            ..case(
                "block: blank lines after a list nested over many lines",
                20_000,
                |k| deep_list_then_blank_lines(k / 10, k),
                None,
            )
        },
        case(
            "block: 10k-char lines",
            100,
            |k| format!("{}\n", "a".repeat(10_000)).repeat(k),
            None,
        ),
        case("block: one huge line", 1_000_000, |k| "a".repeat(k), None),
        case(
            "block: unclosed fences with info",
            200_000,
            |k| "```a\n".repeat(k),
            None,
        ),
        case(
            "block: a fence in every list item",
            200_000,
            |k| "- ```\n".repeat(k),
            None,
        ),
        case(
            "block: a fence on every quote line",
            200_000,
            |k| "> ```\n".repeat(k),
            None,
        ),
        case(
            "block: unclosed HTML comment",
            300_000,
            |k| format!("<!--\n{}", "x\n".repeat(k)),
            None,
        ),
        case(
            "block: one long reference-definition paragraph",
            25_000,
            |k| "[a]: /u\n".repeat(k),
            None,
        ),
        case(
            "block: reference definitions and setext underlines",
            50_000,
            |k| "[a]: /u\n===\n".repeat(k),
            None,
        ),
        case(
            "block: long tight list",
            200_000,
            |k| "- a\n".repeat(k),
            None,
        ),
        case(
            "block: long loose list",
            150_000,
            |k| "- a\n\n".repeat(k),
            None,
        ),
        case(
            "block: paragraphs of unclosed brackets",
            5_000,
            |k| format!("[{}\n", "[a ".repeat(50)).repeat(k),
            None,
        ),
    ]
}

/// GFM shapes, rendered with every extension (see [`render_gfm`]).
fn gfm_cases() -> Vec<Case> {
    vec![
        case(
            "gfm: many table rows",
            50_000,
            |k| format!("| a | b |\n| - | - |\n{}", "| x | y |\n".repeat(k)),
            Some(|o, k| o.matches("<td>x</td>").count() == k),
        ),
        case(
            "gfm: wide header, one-cell rows",
            2_000,
            |k| {
                format!(
                    "{}\n{}\n{}",
                    "|a".repeat(k),
                    "|-".repeat(k),
                    "x\n".repeat(k)
                )
            },
            None,
        ),
        case(
            "gfm: delimiter-like lines in one paragraph",
            50_000,
            |k| format!("a | b\n{}", "--- |\nc | d\n".repeat(k)),
            None,
        ),
        case(
            "gfm: paragraph lines that are rows",
            50_000,
            |k| "| a | b | c |\n".repeat(k),
            None,
        ),
        case(
            "gfm: unclosed single tildes",
            16_250,
            |k| "~a ".repeat(k),
            Some(|o, k| o.contains(&format!("{}~a", "~a ".repeat(k - 1)))),
        ),
        case(
            "gfm: nested strikethrough and emphasis",
            16_250,
            |k| format!("{}b{}", "~~a *a ".repeat(k), " a* a~~".repeat(k)),
            None,
        ),
        case(
            "gfm: www after underscores",
            50_000,
            |k| "_www.a_".repeat(k),
            None,
        ),
        case(
            "gfm: www triggers that fail",
            50_000,
            |k| "(www.a_b.c ".repeat(k),
            None,
        ),
        case(
            "gfm: scheme triggers",
            50_000,
            |k| "http:/ http://a_b.c_d ".repeat(k),
            None,
        ),
        case("gfm: a run of @", 200_000, |k| "@".repeat(k), None),
        case("gfm: @ between letters", 100_000, |k| "a@".repeat(k), None),
        case(
            "gfm: emails in nested emphasis",
            16_250,
            |k| format!("{}a@b.c{}", "*x ".repeat(k), " x*".repeat(k)),
            None,
        ),
        case(
            "gfm: long email local parts",
            100,
            |k| format!("{}@b", "a.".repeat(10_000)).repeat(k),
            None,
        ),
    ]
}

/// Renders with the GFM renderer and every extension on.
fn render_gfm(src: &str) -> String {
    render_gfm_html(src, &["table", "strikethrough", "autolink", "tagfilter"])
}

fn fixed_cases() -> Vec<Fixed> {
    vec![
        Fixed {
            name: "cmark: hard link/emph case",
            input: "**x [a*b**c*](d)",
            check: |o| o.contains("**x <a href=\"d\">a<em>b**c</em></a>"),
        },
        Fixed {
            name: "cmark: U+0000 in input",
            input: "abc\u{0}de\u{0}",
            check: |o| o.contains("abc\u{fffd}de") || o.contains("abcde"),
        },
    ]
}

/// Inputs are grown until the 2n run takes at least this long...
const TARGET_SECONDS: f64 = 0.005;
/// ...unless that would make the 2n input larger than this (beyond about
/// 1 MB, cache misses and page faults dominate and distort the ratio).
const MAX_GROWN_BYTES: usize = 1 << 20;
/// Growth exponent (n/4 vs 2n) at which a high n/2n ratio is confirmed as
/// superlinear.
const MAX_EXPONENT: f64 = 1.5;

/// Best of `runs` timings, in seconds, and the last output.
fn time(render: fn(&str) -> String, input: &str, runs: usize) -> (f64, String) {
    let mut best = f64::INFINITY;
    let mut out = String::new();
    for _ in 0..runs {
        let t = Instant::now();
        out = std::hint::black_box(render(std::hint::black_box(input)));
        best = best.min(t.elapsed().as_secs_f64());
    }
    (best, out)
}

/// Runs every case through `render`; panics listing the failures.
fn run_suite(engine: &str, render: fn(&str) -> String, cases: Vec<Case>, fixed: Vec<Fixed>) {
    let filter = std::env::var("GV_PATHOLOGICAL_FILTER").unwrap_or_default();
    let is_comrak = engine == "comrak";
    // comrak sits at 2.0-2.7 on several shapes (it is not tuned for this
    // check, and the large outputs feel cache effects); its run validates the
    // generators and output checks, so it gets a looser limit.
    let max_ratio: f64 = std::env::var("GV_PATHOLOGICAL_MAX_RATIO")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if is_comrak { 3.0 } else { 2.5 });
    // Below this the timer and the machine's noise dominate (best-of-5
    // timings are stable well below a millisecond).
    let min_seconds = 0.0005;

    let mut failures = Vec::new();
    println!("\nPathological inputs, engine {engine}, max normalized ratio {max_ratio}\n");
    println!("| Case | bytes n | ms | bytes 2n | ms | ratio | output |");
    println!("|---|---:|---:|---:|---:|---:|---|");
    for c in cases.iter().filter(|c| c.name.contains(filter.as_str())) {
        // Grow the input until the 2n run is long enough to time reliably
        // (fast engines finish cmark's sizes in well under a millisecond).
        let mut n = c.n;
        while n < c.n * 64 {
            let b = (c.make)(2 * n);
            if b.len() * 4 > MAX_GROWN_BYTES.max((c.make)(2 * c.n).len())
                || time(render, &b, 1).0 >= TARGET_SECONDS
            {
                break;
            }
            n *= 4;
        }
        let a = (c.make)(n);
        let b = (c.make)(2 * n);
        let measure = |runs| {
            let (ta, _) = time(render, &a, runs);
            let (tb, out) = time(render, &b, runs);
            let ratio = (tb / ta) / (b.len() as f64 / a.len() as f64) * 2.0;
            (ta, tb, ratio, out)
        };
        let (mut ta, mut tb, mut ratio, out) = measure(5);
        if ratio >= max_ratio && tb >= min_seconds {
            // Timing noise is about +-0.3: measure once more, more carefully.
            let (ta2, tb2, ratio2, _) = measure(7);
            if ratio2 < ratio {
                (ta, tb, ratio) = (ta2, tb2, ratio2);
            }
        }
        let output_ok = c.check.is_none_or(|check| check(&out, 2 * n));
        let measurable = tb >= min_seconds;
        let ratio_text = if measurable {
            format!("{ratio:.2}")
        } else {
            String::from("(fast)")
        };
        let known = c.comrak_known.filter(|_| is_comrak);
        let mut note = String::new();
        if measurable && ratio >= max_ratio {
            // Confirm over an 8x size step, n/4 to 2n, within the capped
            // size (see the module documentation).
            let small = (c.make)((n / 4).max(1));
            let (ts, _) = time(render, &small, 9);
            let (tb8, _) = time(render, &b, 5);
            let exponent = (tb8 / ts).ln() / (b.len() as f64 / small.len() as f64).ln();
            if exponent >= MAX_EXPONENT {
                let _ = write!(note, " SUPERLINEAR (exponent {exponent:.2} over 8x)");
                if known.is_none() {
                    failures.push(format!(
                        "{}: ratio {ratio:.2}, exponent {exponent:.2}",
                        c.name
                    ));
                }
            } else {
                let _ = write!(note, " (exponent {exponent:.2} over 8x: linear)");
            }
        }
        let output = match (c.check, output_ok) {
            (None, _) => "-",
            (Some(_), true) => "ok",
            (Some(_), false) => {
                if known.is_none() {
                    failures.push(format!("{}: unexpected output", c.name));
                }
                "FAILED"
            }
        };
        if let Some(why) = known
            && (note.contains("SUPERLINEAR") || !output_ok)
        {
            let _ = write!(note, " (known: {why})");
        }
        println!(
            "| {} | {} | {:.2} | {} | {:.2} | {ratio_text}{note} | {output} |",
            c.name,
            a.len(),
            ta * 1e3,
            b.len(),
            tb * 1e3,
        );
    }
    for f in fixed.iter().filter(|f| f.name.contains(filter.as_str())) {
        let out = render(f.input);
        let ok = (f.check)(&out);
        if !ok {
            failures.push(format!("{}: unexpected output {out:?}", f.name));
        }
        println!(
            "| {} | {} | | | | | {} |",
            f.name,
            f.input.len(),
            if ok { "ok" } else { "FAILED" }
        );
    }
    assert!(failures.is_empty(), "{engine}: {failures:#?}");
}

#[test]
#[ignore = "timing suite; run with `cargo xtask pathological` (release build)"]
fn pathological_gv_syntax() {
    if IS_PLACEHOLDER {
        println!("pathological_gv_syntax: skipped, gv-syntax's renderer is a placeholder");
        return;
    }
    run_suite("gv-syntax", render_commonmark_html, cases(), fixed_cases());
    run_suite("gv-syntax (GFM)", render_gfm, gfm_cases(), Vec::new());
}

#[test]
#[ignore = "timing suite; run with `cargo xtask pathological --engine comrak` (release build)"]
fn pathological_comrak() {
    run_suite(
        "comrak",
        support::comrak_commonmark_html,
        cases(),
        fixed_cases(),
    );
}

/// Every generator runs at a small size, and the output matchers accept
/// what they should (cheap; runs on every `cargo test`).
#[test]
fn generators_and_matchers() {
    for c in cases().into_iter().chain(gfm_cases()) {
        let small = (c.make)((c.n / 1000).max(3));
        assert!(!small.is_empty(), "{}", c.name);
    }
    let (doc, bad) = reference_collisions(4);
    assert_eq!(doc.matches(&format!("[{bad}]\n")).count(), 3);
    assert!(wrapped_in_tags("<ul>\n<li>x</li>\n</ul>\n", "x"));
    assert!(!wrapped_in_tags("<p>a x</p>\n", "x"));
    assert!(open_tags("<blockquote>\n<blockquote>\n", false));
    assert!(!open_tags("<li>", false));
    assert!(close_tags("</p>\n</li>\n"));
    assert!(!close_tags("</p>"));
}
