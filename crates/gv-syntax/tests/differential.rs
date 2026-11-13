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
use support::comrak_outline::comrak_outline;
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
        divergences.push(Divergence {
            id: s(t, "id").unwrap_or_default(),
            profile: s(t, "profile").unwrap_or_default(),
            input: s(t, "input"),
            comrak_html: s(t, "comrak_html"),
        });
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
        if known.contains(&small.as_str()) {
            known_hits += 1;
        } else {
            *new_failures.entry(small).or_default() += 1;
        }
    }
    eprintln!(
        "differential_vs_comrak: {cases} documents (seed {seed}), {known_hits} known divergences, {} new",
        new_failures.len()
    );
    let report: Vec<String> = new_failures
        .iter()
        .take(10)
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
