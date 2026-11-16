// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `cargo xtask bench-icount`: the instruction-count benchmarks (gungraun on
//! Valgrind's Callgrind), with an optional comparison against a saved
//! baseline that fails on a regression over the limit.
//!
//! CI (`ci.yml`, job `icount`) saves a baseline from the pull request's base
//! commit and then runs the head against it, in one job on one machine.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::cargo;

/// The instruction-count bench targets, as `(package, bench)`. Add a line
/// here when a crate gets an `icount` bench.
const BENCHES: &[(&str, &str)] = &[("gv-syntax", "icount"), ("gv-editor", "icount")];

/// Instruction-count growth against the baseline that fails without an
/// explanation.
const DEFAULT_LIMIT_PERCENT: f64 = 3.0;

/// Exit code of the gungraun runner when a limit was exceeded.
const REGRESSION_EXIT_CODE: i32 = 3;

const USAGE: &str = "\
Usage: cargo xtask bench-icount [options] [-- RUNNER-ARGS...]

Runs the instruction-count benchmarks under Valgrind (Linux only). Needs
`valgrind` and a `gungraun-runner` of the same version as the `gungraun`
library in Cargo.lock.

Options:
  --save-baseline NAME  Save the results as baseline NAME
  --baseline NAME       Compare against baseline NAME; fail when the
                        instruction count of any benchmark grew by more than
                        the limit
  --limit PCT           Allowed growth against the baseline (default 3)
  --allow-regression    Report growth over the limit without failing (the
                        change explains the regression)

Results and baselines are kept under target/gungraun (or $GUNGRAUN_HOME).
Arguments after `--` go to gungraun-runner unchanged, for example a filter
on the benchmark path FILE::GROUP::FUNCTION::ID: `-- 'icount::reparse::*'`.
";

pub(crate) fn run(root: &Path, args: &[String]) -> Result<bool> {
    let mut save_baseline = None;
    let mut baseline = None;
    let mut limit = DEFAULT_LIMIT_PERCENT;
    let mut allow_regression = false;
    let mut passthrough = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("`{arg}` needs a value"));
        match arg.as_str() {
            "--save-baseline" => save_baseline = Some(value()?.clone()),
            "--baseline" => baseline = Some(value()?.clone()),
            "--limit" => {
                limit = value()?.parse().context("--limit takes a number")?;
            }
            "--allow-regression" => allow_regression = true,
            "--" => {
                passthrough.extend(it.by_ref().cloned());
                break;
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(true);
            }
            other => bail!("unknown option `{other}`\n\n{USAGE}"),
        }
    }
    if !cfg!(target_os = "linux") {
        bail!("the instruction-count benchmarks run on Linux only (Valgrind)");
    }
    check_tools(root)?;

    let mut runner_args = Vec::new();
    if let Some(name) = &save_baseline {
        runner_args.push(format!("--save-baseline={name}"));
    }
    if let Some(name) = &baseline {
        runner_args.push(format!("--baseline={name}"));
        if !allow_regression {
            runner_args.push(format!("--callgrind-limits=ir={limit}%"));
        }
    }
    runner_args.extend(passthrough);

    let mut regressed = false;
    for (package, bench) in BENCHES {
        eprintln!("==> {package}: bench {bench}");
        let status = Command::new(cargo())
            .args(["bench", "--locked", "-p", package, "--bench", bench, "--"])
            .args(&runner_args)
            .current_dir(root)
            .status()
            .with_context(|| format!("failed to run the `{bench}` bench of {package}"))?;
        match status.code() {
            Some(0) => {}
            Some(REGRESSION_EXIT_CODE) => regressed = true,
            _ => bail!("the `{bench}` bench of {package} failed ({status})"),
        }
    }
    if regressed {
        eprintln!(
            "error: instruction counts grew by more than {limit} % against baseline `{}`. \
             If the change explains the regression, rerun with --allow-regression \
             (CI: add the `perf-regression-justified` label).",
            baseline.as_deref().unwrap_or("?")
        );
    }
    Ok(!regressed)
}

/// Checks that Valgrind is installed and that `gungraun-runner` matches the
/// `gungraun` library version in Cargo.lock.
fn check_tools(root: &Path) -> Result<()> {
    let valgrind = Command::new("valgrind").arg("--version").output();
    if !valgrind.is_ok_and(|o| o.status.success()) {
        bail!("`valgrind` was not found; install Valgrind (for example `apt install valgrind`)");
    }
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).context("reading Cargo.lock")?;
    let want = locked_version(&lock, "gungraun").context("no `gungraun` package in Cargo.lock")?;
    let install = format!("cargo install gungraun-runner --version {want} --locked");
    let runner = std::env::var("GUNGRAUN_RUNNER").unwrap_or_else(|_| "gungraun-runner".to_owned());
    let output = Command::new(&runner)
        .arg("--version")
        .output()
        .with_context(|| format!("`{runner}` was not found; install it with `{install}`"))?;
    let have = String::from_utf8_lossy(&output.stdout);
    let have = have.split_whitespace().nth(1).unwrap_or_default();
    if have != want {
        bail!("gungraun-runner is {have}, the gungraun library is {want}; run `{install}`");
    }
    Ok(())
}

/// The version of `package` in a Cargo.lock.
fn locked_version<'a>(lock: &'a str, package: &str) -> Option<&'a str> {
    let name = format!("name = \"{package}\"");
    let mut lines = lock.lines();
    lines.find(|l| *l == name)?;
    lines
        .next()?
        .strip_prefix("version = \"")?
        .strip_suffix('"')
}

#[cfg(test)]
mod tests {
    use super::locked_version;

    #[test]
    fn reads_the_locked_version() {
        let lock = "[[package]]\nname = \"gungraun-runner\"\nversion = \"1.0.0\"\n\n\
                    [[package]]\nname = \"gungraun\"\nversion = \"0.20.0\"\n";
        assert_eq!(locked_version(lock, "gungraun"), Some("0.20.0"));
        assert_eq!(locked_version(lock, "gungraun-runner"), Some("1.0.0"));
        assert_eq!(locked_version(lock, "missing"), None);
    }
}
