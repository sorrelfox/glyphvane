// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `cargo xtask pathological`: the linear-time suite. The generators and
//! checks live in `crates/gv-syntax/tests/pathological.rs` as ignored tests;
//! this command runs them in release mode, one at a time.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::cargo;

const USAGE: &str =
    "usage: cargo xtask pathological [--engine gv-syntax|comrak] [--filter TEXT] [--max-ratio R]";

pub(crate) fn run(root: &Path, args: &[String]) -> Result<bool> {
    let mut test = "pathological_gv_syntax";
    let mut env: Vec<(&str, String)> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .with_context(|| format!("`{a}` needs a value\n{USAGE}"))
        };
        match a.as_str() {
            "--engine" => {
                test = match value()?.as_str() {
                    "gv-syntax" => "pathological_gv_syntax",
                    "comrak" => "pathological_comrak",
                    other => bail!("unknown engine `{other}`\n{USAGE}"),
                };
            }
            "--filter" => env.push(("GV_PATHOLOGICAL_FILTER", value()?)),
            "--max-ratio" => env.push(("GV_PATHOLOGICAL_MAX_RATIO", value()?)),
            other => bail!("unknown option `{other}`\n{USAGE}"),
        }
    }
    let mut cmd = Command::new(cargo());
    cmd.current_dir(root)
        .args([
            "test",
            "--release",
            "--package",
            "gv-syntax",
            "--test",
            "pathological",
            "--",
        ])
        .args([
            "--ignored",
            "--exact",
            test,
            "--nocapture",
            "--test-threads=1",
        ])
        .envs(env);
    eprintln!(
        "==> cargo test --release -p gv-syntax --test pathological -- --ignored --exact {test}"
    );
    let status = cmd.status().context("running cargo test")?;
    Ok(status.success())
}
