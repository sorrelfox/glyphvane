// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `cargo xtask ci`: the local equivalent of the CI checks, fail-fast.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::{cargo, deps, fixtures, headers, spec};

enum Step {
    Cargo(&'static [&'static str]),
    Headers,
    Deps,
    SpecFixtures,
    SpecRatchet,
}

const STEPS: &[(&str, Step)] = &[
    (
        "cargo fmt --all --check",
        Step::Cargo(&["fmt", "--all", "--check"]),
    ),
    ("check-headers", Step::Headers),
    ("check-deps", Step::Deps),
    ("fetch-spec --verify --offline", Step::SpecFixtures),
    ("spec --ratchet (commonmark, gfm)", Step::SpecRatchet),
    (
        "cargo clippy --workspace --all-targets -- -D warnings",
        Step::Cargo(&[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ]),
    ),
    (
        "cargo test --workspace",
        Step::Cargo(&["test", "--workspace"]),
    ),
];

pub(crate) fn run(root: &Path) -> Result<bool> {
    let total = STEPS.len();
    for (i, (name, step)) in STEPS.iter().enumerate() {
        eprintln!("==> [{}/{total}] {name}", i + 1);
        let ok = match step {
            Step::Cargo(args) => Command::new(cargo())
                .args(*args)
                .current_dir(root)
                .status()
                .with_context(|| format!("failed to run `{name}`"))?
                .success(),
            Step::Headers => headers::run(root, false)?,
            Step::Deps => deps::run(root)?,
            Step::SpecFixtures => fixtures::verify_local(root)?,
            Step::SpecRatchet => spec::ratchet_all(root)?,
        };
        if !ok {
            eprintln!("==> FAILED: {name}");
            return Ok(false);
        }
    }
    eprintln!("==> ci: all {total} steps passed");
    Ok(true)
}
