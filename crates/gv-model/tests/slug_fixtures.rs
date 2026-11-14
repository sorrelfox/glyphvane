// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The slug algorithms against the tools' own fixtures.
//!
//! `fixtures/github-slugger-2.0.0.json` is github-slugger's
//! `test/fixtures.json`, unmodified (github.com/Flet/github-slugger, commit
//! 3461c4350868329c8530904d170358bca1d31448, fetched 2026-11-13; SHA-256
//! 26e59aec9fb3adee7a56445049ef2d537126b28581802903b529d2556778b248). ISC
//! licence, copyright Dan Flettre: `fixtures/github-slugger-2.0.0.LICENSE`.
//! github-slugger's own test runs every fixture through one slugger, in
//! order, so the duplicate suffixes depend on the earlier entries.
//!
//! The fixtures of the other tools are short and live next to each
//! algorithm's unit tests, with their sources.

use std::path::PathBuf;

use gv_model::{SlugAlgorithm, Slugger};

/// Fixtures whose expectation depends on github-slugger's Unicode 13
/// tables: characters unassigned in Unicode 13 but assigned since (letters
/// here) are removed there and kept here.
const UNICODE_VERSION_DIFFERENCES: &[&str] = &["Unassigned"];

#[test]
fn github_slugger_fixtures() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/github-slugger-2.0.0.json");
    let text = std::fs::read_to_string(&path).expect("fixture file");
    let fixtures: serde_json::Value = serde_json::from_str(&text).expect("fixture JSON");
    let fixtures = fixtures.as_array().expect("an array");
    assert_eq!(fixtures.len(), 78);
    let mut slugger = Slugger::new(SlugAlgorithm::GitHub);
    let mut failures = Vec::new();
    let mut known = 0;
    for f in fixtures {
        let name = f["name"].as_str().expect("name");
        let input = f["input"].as_str().expect("input");
        let expected = f["expected"].as_str().expect("expected");
        let got = slugger.slug(input);
        if got != expected {
            if UNICODE_VERSION_DIFFERENCES.contains(&name) {
                known += 1;
            } else {
                failures.push(format!(
                    "{name}: {input:?}\n   got {got:?}\n  want {expected:?}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("fixtures differing by Unicode version: {known}");
    assert!(known <= UNICODE_VERSION_DIFFERENCES.len());
}

#[test]
fn github_slugger_basic() {
    // github-slugger's test/index.js, "basic" and "static method".
    let mut s = Slugger::new(SlugAlgorithm::GitHub);
    assert_eq!(s.slug("fooCamelCase"), "foocamelcase");
    assert_eq!(s.slug("asd"), "asd");
    assert_eq!(s.slug("asd"), "asd-1");
    assert_eq!(SlugAlgorithm::GitHub.slug("foo"), "foo");
    assert_eq!(SlugAlgorithm::GitHub.slug("foo"), "foo");
}
