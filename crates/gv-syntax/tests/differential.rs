// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Differential testing against comrak (CommonMark and GFM profiles only).
//!
//! - The oracle checks run on every `cargo test`: comrak passes the spec
//!   suites through our normalizer (which validates both), its block outline
//!   covers every example, and each recorded divergence still reproduces.
//! - `differential_vs_comrak` compares gv-syntax with comrak on random
//!   documents. It is ignored by default (nightly job):
//!
//!   ```sh
//!   GV_DIFF_CASES=100000 GV_DIFF_SEED=7 \
//!     cargo test --release -p gv-syntax --test differential -- --ignored
//!   ```
//!
//! Known divergences live in `tests/fixtures/differential/known-divergences.toml`.

mod support;

use std::collections::BTreeMap;

use gv_syntax::harness::{IS_PLACEHOLDER, normalize_html, render_commonmark_html};
use support::comrak_outline::{comrak_outline, decided, gv_outline};
use support::{
    COMMONMARK_SPEC, GFM_SPEC, Rng, comrak_commonmark_html, comrak_gfm_html, load_spec, minimize,
    random_document, workspace_root,
};

/// One entry of the known-divergences file.
#[derive(Debug)]
struct Divergence {
    id: String,
    /// `commonmark` or `gfm`.
    profile: String,
    /// A minimal input showing the divergence (absent for documentation-only
    /// entries that the generator cannot reach).
    input: Option<String>,
    /// comrak's output for `input`, as recorded.
    comrak_html: Option<String>,
    /// `undecided`, `match-comrak` or `match-spec`.
    decision: String,
    /// gv-syntax's output for `input`, for decided entries.
    gv_html: Option<String>,
}

/// A spec example comrak fails.
#[derive(Debug)]
struct SpecFailure {
    suite: String,
    example: u64,
}

fn load_known() -> (Vec<Divergence>, Vec<SpecFailure>) {
    let path = workspace_root().join("tests/fixtures/differential/known-divergences.toml");
    let text = std::fs::read_to_string(&path).expect("known-divergences.toml");
    let doc: toml::Table = text.parse().expect("known-divergences.toml is valid TOML");
    let s = |t: &toml::Table, k: &str| t.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let mut divergences = Vec::new();
    for t in doc
        .get("divergence")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let t = t.as_table().expect("[[divergence]] is a table");
        for key in ["id", "profile", "reason", "decision"] {
            assert!(s(t, key).is_some(), "divergence {t:?} lacks `{key}`");
        }
        let d = Divergence {
            id: s(t, "id").unwrap_or_default(),
            profile: s(t, "profile").unwrap_or_default(),
            input: s(t, "input"),
            comrak_html: s(t, "comrak_html"),
            decision: s(t, "decision").unwrap_or_default(),
            gv_html: s(t, "gv_html"),
        };
        assert!(
            ["undecided", "match-comrak", "match-spec"].contains(&d.decision.as_str()),
            "{}: unknown decision `{}`",
            d.id,
            d.decision
        );
        if d.decision != "undecided" && d.input.is_some() {
            assert!(d.gv_html.is_some(), "{}: decided without `gv_html`", d.id);
        }
        if d.decision == "match-spec" {
            assert!(
                s(t, "spec").is_some(),
                "{}: `match-spec` without `spec`",
                d.id
            );
        }
        divergences.push(d);
    }
    let mut failures = Vec::new();
    for t in doc
        .get("comrak_spec_failure")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let t = t.as_table().expect("[[comrak_spec_failure]] is a table");
        assert!(
            s(t, "reason").is_some(),
            "comrak_spec_failure {t:?} lacks `reason`"
        );
        failures.push(SpecFailure {
            suite: s(t, "suite").expect("suite"),
            example: t
                .get("example")
                .and_then(toml::Value::as_integer)
                .and_then(|n| u64::try_from(n).ok())
                .expect("example"),
        });
    }
    (divergences, failures)
}

fn same_html(a: &str, b: &str) -> bool {
    normalize_html(a) == normalize_html(b)
}

/// comrak vs the expected HTML of every example of a suite. Returns the
/// failing example numbers.
fn comrak_spec_failures(file: &str, gfm: bool) -> Vec<u64> {
    load_spec(file)
        .iter()
        .filter(|ex| {
            let html = if gfm {
                comrak_gfm_html(&ex.markdown, &ex.extensions)
            } else {
                comrak_commonmark_html(&ex.markdown)
            };
            !same_html(&html, &ex.html)
        })
        .map(|ex| ex.number)
        .collect()
}

fn recorded_failures(suite: &str) -> Vec<u64> {
    let (_, failures) = load_known();
    let mut v: Vec<u64> = failures
        .iter()
        .filter(|f| f.suite == suite)
        .map(|f| f.example)
        .collect();
    v.sort_unstable();
    v
}

/// The oracle and the normalizer agree with the CommonMark spec, apart from
/// the recorded comrak failures.
#[test]
fn comrak_passes_commonmark_spec() {
    let failing = comrak_spec_failures(COMMONMARK_SPEC, false);
    assert_eq!(
        failing,
        recorded_failures("commonmark"),
        "comrak's CommonMark spec failures changed; update known-divergences.toml"
    );
}

/// Same for the GFM spec (0.29), with per-example extensions.
#[test]
fn comrak_gfm_spec_failures_are_recorded() {
    let failing = comrak_spec_failures(GFM_SPEC, true);
    assert_eq!(
        failing,
        recorded_failures("gfm"),
        "comrak's GFM spec failures changed; update known-divergences.toml"
    );
}

/// The normalizer does not hide real differences: two expected outputs that
/// differ in anything but whitespace normalize differently. (Like the Python
/// original it is not idempotent: a multi-line CDATA section becomes a
/// declaration, which a second pass reads as a bogus comment.)
#[test]
fn normalizer_keeps_distinct_examples_distinct() {
    let examples = load_spec(COMMONMARK_SPEC);
    let squeeze = |s: &str| s.split_ascii_whitespace().collect::<String>();
    let mut seen: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for ex in &examples {
        let n = normalize_html(&ex.html);
        if let Some((other, raw)) = seen.get(&n) {
            assert_eq!(
                squeeze(raw),
                squeeze(&ex.html),
                "examples {other} and {} normalize to the same HTML",
                ex.number
            );
        } else {
            seen.insert(n, (ex.number, ex.html.clone()));
        }
    }
}

/// The block-outline utility understands every block comrak produces for
/// the spec examples.
#[test]
fn comrak_outline_covers_spec() {
    for ex in load_spec(COMMONMARK_SPEC) {
        let outline = comrak_outline(&ex.markdown, &[]);
        assert_eq!(outline.first().map(String::as_str), Some("document"));
        assert!(
            outline.iter().all(|l| !l.contains("<unexpected")),
            "example {}: {outline:#?}",
            ex.number
        );
    }
    assert_eq!(
        comrak_outline("- a\n- b\n\n> # h\n", &[]),
        [
            "document",
            "  list bullet - start=1 tight @1",
            "    item @1",
            "      paragraph @1-1",
            "    item @2",
            "      paragraph @2-2",
            "  block_quote @4",
            "    heading h1 atx @4-4",
        ]
    );
}

/// Every recorded divergence still reproduces with the pinned comrak.
#[test]
fn known_divergences_are_current() {
    let (divergences, _) = load_known();
    assert!(!divergences.is_empty());
    for d in &divergences {
        let (Some(input), Some(recorded)) = (&d.input, &d.comrak_html) else {
            continue;
        };
        let actual = match d.profile.as_str() {
            "commonmark" => comrak_commonmark_html(input),
            "gfm" => comrak_gfm_html(input, &support::gfm_all_extensions()),
            other => panic!("{}: unknown profile `{other}`", d.id),
        };
        assert_eq!(
            &actual, recorded,
            "{}: comrak output changed for {input:?}",
            d.id
        );
    }
}

/// Each decided divergence renders as recorded in `gv_html`; for
/// `match-spec`, that differs from comrak, and for `match-comrak` it is
/// comrak's output.
#[test]
fn decided_divergences_are_pinned() {
    let (divergences, _) = load_known();
    let mut pinned = 0;
    for d in divergences.iter().filter(|d| d.profile == "commonmark") {
        let (Some(input), Some(expected)) = (&d.input, &d.gv_html) else {
            continue;
        };
        let ours = render_commonmark_html(input);
        assert_eq!(&ours, expected, "{}: gv-syntax output changed", d.id);
        let comrak = d.comrak_html.as_deref().expect("comrak_html");
        match d.decision.as_str() {
            "match-spec" => assert_ne!(ours, comrak, "{}: no longer diverges", d.id),
            "match-comrak" => assert_eq!(ours, comrak, "{}", d.id),
            _ => {}
        }
        pinned += 1;
    }
    assert!(
        pinned >= 7,
        "only {pinned} decided divergences have an input"
    );
}

/// Recognizes a divergence from the input, our output and comrak's output.
type InlineClass = fn(&str, &str, &str) -> bool;

/// Decided divergences in inline content or in whitespace, which the block
/// outline does not show, recognized by the shape of the two outputs.
/// Each is the `id` of a `match-spec` entry of the divergences file.
const INLINE_CLASSES: &[(&str, InlineClass)] = &[
    (
        "lazy-first-line-keeps-leading-whitespace",
        lazy_first_line_whitespace,
    ),
    (
        "code-span-after-unmatched-longer-run",
        code_span_after_unmatched_run,
    ),
    ("code-span-crlf-keeps-cr", code_span_crlf),
    (
        "escaped-ampersand-before-entity-in-title-or-info",
        escaped_ampersand,
    ),
];

/// Whether `md` changed by `rewrite` renders, in both gv-syntax and
/// comrak, as gv-syntax renders `md`: the rewrite removes exactly what
/// comrak handles differently, without changing what it means to us.
fn agrees_after(md: &str, ours: &str, rewrite: impl Fn(&str) -> Option<String>) -> bool {
    rewrite(md).is_some_and(|md2| {
        render_commonmark_html(&md2) == ours && comrak_commonmark_html(&md2) == ours
    })
}

/// comrak keeps the leading whitespace of a lazy line that starts a
/// paragraph. Recognized when comrak agrees once one indented line is no
/// longer lazy: its whitespace removed (our paragraph strips it), or the line
/// continued with a block quote marker; or when comrak's output is ours with
/// spaces and tabs at the start of paragraphs.
fn lazy_first_line_whitespace(md: &str, ours: &str, comrak: &str) -> bool {
    let mut stripped = String::with_capacity(comrak.len());
    let mut rest = comrak;
    while let Some(i) = rest.find("<p>") {
        stripped.push_str(&rest[..i + 3]);
        rest = rest[i + 3..].trim_start_matches([' ', '\t']);
    }
    stripped.push_str(rest);
    if stripped == ours {
        return true;
    }
    let lines: Vec<&str> = md.split_inclusive('\n').collect();
    (1..lines.len()).any(|i| {
        let line = lines[i];
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.len() == line.len() {
            return false;
        }
        [trimmed.to_owned(), format!(">{line}"), format!("> {line}")]
            .into_iter()
            .any(|replacement| {
                agrees_after(md, ours, |_| {
                    let mut md2 = lines[..i].concat();
                    md2.push_str(&replacement);
                    md2.push_str(&lines[i + 1..].concat());
                    Some(md2)
                })
            })
    })
}

/// comrak keeps a CR inside a code span, and normalizing CRLF to LF makes
/// it agree.
fn code_span_crlf(md: &str, ours: &str, comrak: &str) -> bool {
    let cr_in_code = comrak.split("<code").skip(1).any(|rest| {
        rest.split("</code>")
            .next()
            .is_some_and(|code| code.contains('\r'))
    });
    cr_in_code && agrees_after(md, ours, |md| Some(md.replace("\r\n", "\n")))
}

/// Writing an escaped `&` as the entity `&amp;` (which means the same)
/// makes comrak agree: it decodes entities in link titles and info strings
/// before backslash escapes.
fn escaped_ampersand(md: &str, ours: &str, _comrak: &str) -> bool {
    md.contains("\\&") && agrees_after(md, ours, |md| Some(md.replace("\\&", "&amp;")))
}

/// The input has a backtick string of length two or more, and comrak's
/// output equals ours with one or more of our last code spans written back
/// as backtick-delimited text.
fn code_span_after_unmatched_run(md: &str, ours: &str, comrak: &str) -> bool {
    if !md.contains("``") || ours == comrak {
        return false;
    }
    let starts: Vec<usize> = ours.match_indices("<code>").map(|(i, _)| i).collect();
    let mut html = ours.to_owned();
    // From the last span backwards: the text after a span already agrees,
    // so the span's literal form must end comrak's output just before it.
    for &start in starts.iter().rev() {
        let Some(len) = html[start..].find("</code>") else {
            return false;
        };
        let end = start + len + "</code>".len();
        let content = html[start + "<code>".len()..start + len].to_owned();
        if content.contains(['`', '<', '\n']) {
            return false;
        }
        let literal = (1..=8).map(|n| "`".repeat(n)).find_map(|ticks| {
            let literal = format!("{ticks}{content}{ticks}");
            comrak
                .ends_with(&format!("{literal}{}", &html[end..]))
                .then_some(literal)
        });
        let Some(literal) = literal else {
            return false;
        };
        html.replace_range(start..end, &literal);
        if html == comrak {
            return true;
        }
    }
    false
}

/// Whether the HTML difference for a minimized input is one of the decided
/// inline or whitespace divergences in `decided` (ids of `match-spec`
/// entries).
fn decided_inline_divergence(md: &str, decided: &[&str]) -> bool {
    let ours = render_commonmark_html(md);
    let comrak = comrak_commonmark_html(md);
    INLINE_CLASSES
        .iter()
        .any(|(id, class)| decided.contains(id) && class(md, &ours, &comrak))
}

/// Each inline class recognizes the recorded input of its entry, and only
/// that one.
#[test]
fn inline_classes_recognize_their_entries() {
    let (divergences, _) = load_known();
    for (id, class) in INLINE_CLASSES {
        let d = divergences
            .iter()
            .find(|d| d.id == *id)
            .unwrap_or_else(|| panic!("{id} is not in the divergences file"));
        assert_eq!(d.decision, "match-spec", "{id}");
        for other in &divergences {
            let Some(input) = &other.input else { continue };
            if other.profile != "commonmark" {
                continue;
            }
            let ours = render_commonmark_html(input);
            let comrak = comrak_commonmark_html(input);
            assert_eq!(
                class(input, &ours, &comrak),
                other.id == *id,
                "{id} on the input of {}",
                other.id
            );
        }
    }
}

/// Whether a minimized divergence is one of the decided block-level ones
/// (see `support::comrak_outline::decided`).
fn decided_block_divergence(md: &str) -> bool {
    let tree = gv_syntax::SyntaxTree::parse(md);
    let (ours, infos) = gv_outline(&tree, md);
    let theirs = comrak_outline(md, &infos);
    ours != theirs && decided(md, &ours, &theirs)
}

/// gv-syntax vs comrak on random documents (nightly).
#[test]
#[ignore = "nightly differential run; needs the gv-syntax HTML emitter"]
fn differential_vs_comrak() {
    if IS_PLACEHOLDER {
        eprintln!("differential_vs_comrak: skipped, gv-syntax's renderer is a placeholder");
        return;
    }
    let cases: usize = std::env::var("GV_DIFF_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);
    let seed: u64 = std::env::var("GV_DIFF_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let (divergences, _) = load_known();
    let known: Vec<&str> = divergences
        .iter()
        .filter(|d| d.profile == "commonmark")
        .filter_map(|d| d.input.as_deref())
        .collect();
    let decided: Vec<&str> = divergences
        .iter()
        .filter(|d| d.decision == "match-spec")
        .map(|d| d.id.as_str())
        .collect();
    let differs = |md: &str| !same_html(&render_commonmark_html(md), &comrak_commonmark_html(md));

    let mut rng = Rng::new(seed);
    let mut new_failures: BTreeMap<String, usize> = BTreeMap::new();
    let mut known_hits = 0usize;
    for _ in 0..cases {
        let doc = random_document(&mut rng, 40);
        if !differs(&doc) {
            continue;
        }
        let small = minimize(&doc, differs);
        if known.contains(&small.as_str())
            || decided_block_divergence(&small)
            || decided_inline_divergence(&small, &decided)
        {
            known_hits += 1;
        } else {
            *new_failures.entry(small).or_default() += 1;
        }
    }
    // Divergences whose block structure agrees with comrak's come from the
    // inline parser.
    let block_level = new_failures
        .keys()
        .filter(|md| {
            let tree = gv_syntax::SyntaxTree::parse(md.as_str());
            let (ours, infos) = gv_outline(&tree, md.as_str());
            ours != comrak_outline(md, &infos)
        })
        .count();
    eprintln!(
        "differential_vs_comrak: {cases} documents (seed {seed}), {known_hits} known divergences, {} new ({block_level} with a different block structure)",
        new_failures.len()
    );
    let report: Vec<String> = new_failures
        .iter()
        .take(20)
        .map(|(md, n)| {
            format!(
                "{md:?} ({n}×)\n  gv-syntax: {:?}\n  comrak:    {:?}",
                render_commonmark_html(md),
                comrak_commonmark_html(md)
            )
        })
        .collect();
    assert!(
        new_failures.is_empty(),
        "new divergences:\n{}",
        report.join("\n")
    );
}

/// The generator and minimizer work (cheap; runs on every `cargo test`).
#[test]
fn generator_and_minimizer() {
    let mut rng = Rng::new(42);
    let docs: Vec<String> = (0..50).map(|_| random_document(&mut rng, 20)).collect();
    assert!(docs.iter().any(|d| d.contains("```")));
    assert!(docs.iter().any(|d| d.contains('>')));
    let small = minimize("a\nb *x* c\nd\n", |s| s.contains("*x*"));
    assert_eq!(small, "*x*");
}
