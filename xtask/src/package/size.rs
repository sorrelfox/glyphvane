// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `cargo xtask size-report`: executable sizes against the NFR-SIZE budgets
//! and against a baseline report (CI: the base branch's last run).

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::mb;

/// NFR-SIZE-01: `glyphvane`, stripped, `[profile.dist]`.
pub(crate) const MAIN_BUDGET: u64 = 95_000_000;
/// NFR-SIZE-02: `glyphvane-helper`, stripped, `[profile.dist]`.
pub(crate) const HELPER_BUDGET: u64 = 45_000_000;
/// Growth against the baseline that fails without a justification.
const DEFAULT_MAX_GROWTH_PERCENT: f64 = 3.0;

const USAGE: &str = "\
Usage: cargo xtask size-report --bin-dir DIR [options]

Reports the size of glyphvane and glyphvane-helper (when built) in DIR
against the NFR-SIZE budgets and, with --baseline, against an earlier report.
Fails when an executable is over budget, or grew by more than the limit.

Options:
  --json FILE          Write this report as JSON (the next run's baseline)
  --baseline FILE      An earlier JSON report to compare against
  --max-growth PCT     Allowed growth against the baseline (default 3)
  --allow-growth       Report growth over the limit without failing (the
                       change explains the growth)

Appends the table to $GITHUB_STEP_SUMMARY when that is set.
";

pub(crate) fn run(args: &[String]) -> Result<bool> {
    let mut bin_dir = None;
    let mut json_out = None;
    let mut baseline = None;
    let mut max_growth = DEFAULT_MAX_GROWTH_PERCENT;
    let mut allow_growth = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("`{arg}` needs a value"));
        match arg.as_str() {
            "--bin-dir" => bin_dir = Some(PathBuf::from(value()?)),
            "--json" => json_out = Some(PathBuf::from(value()?)),
            "--baseline" => baseline = Some(PathBuf::from(value()?)),
            "--max-growth" => {
                max_growth = value()?.parse().context("--max-growth takes a number")?;
            }
            "--allow-growth" => allow_growth = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(true);
            }
            other => bail!("unknown option `{other}`\n\n{USAGE}"),
        }
    }
    let bin_dir = bin_dir.with_context(|| format!("--bin-dir is required\n\n{USAGE}"))?;

    let mut sizes = serde_json::Map::new();
    for name in ["glyphvane", "glyphvane-helper"] {
        let path = [name.to_owned(), format!("{name}.exe")]
            .into_iter()
            .map(|n| bin_dir.join(n))
            .find(|p| p.is_file());
        if let Some(path) = path {
            sizes.insert(name.to_owned(), fs::metadata(&path)?.len().into());
        }
    }
    if !sizes.contains_key("glyphvane") {
        bail!("no glyphvane executable in {}", bin_dir.display());
    }
    let base: Option<serde_json::Value> = match &baseline {
        Some(p) => Some(
            serde_json::from_str(
                &fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?,
            )
            .with_context(|| format!("parsing {}", p.display()))?,
        ),
        None => None,
    };

    let mut ok = true;
    let mut table = String::from(
        "### Binary size (NFR-SIZE)\n\n\
         | Executable | Size | Budget | Baseline | Change |\n\
         |---|---:|---:|---:|---:|\n",
    );
    let mut notes = Vec::new();
    for (name, size) in &sizes {
        let size = size.as_u64().unwrap_or(0);
        let budget = if name == "glyphvane" {
            MAIN_BUDGET
        } else {
            HELPER_BUDGET
        };
        if size > budget {
            ok = false;
            notes.push(format!(
                "**{name} is over its budget**: {:.2} MB > {} MB.",
                mb(size),
                budget / 1_000_000
            ));
        }
        let before = base
            .as_ref()
            .and_then(|b| b["binaries"][name.as_str()].as_u64());
        let (before_cell, change_cell) = match before {
            Some(before) if before > 0 => {
                let pct = (mb(size) - mb(before)) / mb(before) * 100.0;
                if pct > max_growth {
                    if allow_growth {
                        notes.push(format!(
                            "{name} grew by {pct:.2} % (limit {max_growth} %); accepted as justified."
                        ));
                    } else {
                        ok = false;
                        notes.push(format!(
                            "**{name} grew by {pct:.2} %** (limit {max_growth} %). Reduce it, or \
                             explain the growth in the pull request and add the \
                             `binary-size-justified` label."
                        ));
                    }
                }
                (format!("{:.2} MB", mb(before)), format!("{pct:+.2} %"))
            }
            _ => ("n/a".to_owned(), "n/a".to_owned()),
        };
        let _ = writeln!(
            table,
            "| `{name}` | {:.2} MB ({size} B) | {} MB | {before_cell} | {change_cell} |",
            mb(size),
            budget / 1_000_000
        );
    }
    if base.is_none() {
        notes.push("No baseline report: sizes are not compared.".to_owned());
    }
    for note in &notes {
        let _ = write!(table, "\n{note}\n");
    }
    print!("{table}");
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write as _;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(summary)?;
        writeln!(f, "{table}")?;
    }
    if let Some(out) = json_out {
        let report = serde_json::json!({ "binaries": sizes });
        fs::write(&out, serde_json::to_string_pretty(&report)? + "\n")
            .with_context(|| format!("writing {}", out.display()))?;
    }
    Ok(ok)
}
