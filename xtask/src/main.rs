// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Repository automation, run as `cargo xtask <command>`.

mod ci;
mod deps;
mod fixtures;
mod fuzz_seed;
mod headers;
mod highlight_bundle;
mod icount;
mod latency;
mod package;
mod pathological;
mod spec;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, bail};

const HELP: &str = "\
Usage: cargo xtask <command> [options]

Commands:
  help                  Show this message.
  check-headers [--fix] Check that every Rust source file starts with the
                        SPDX/copyright header; --fix inserts missing headers.
  check-deps            Check that GUI-free crates do not depend on GPUI
                        (NFR-MAINT-01).
  ci                    Run the local equivalent of CI: fmt, check-headers,
                        check-deps, spec fixtures and ratchet, clippy, test.
  spec [options]        Run the CommonMark (or GFM) spec examples against
                        gv-syntax's HTML emitter. Options:
                          --suite commonmark|gfm   (default commonmark)
                          --section NAME           sections containing NAME
                          --example N              one example (repeatable)
                          --format text|json       report format
                          --diffs N                failures to show in full
                          --min N                  fail below N passes
                          --ratchet                fail if an example listed in
                                                   tests/fixtures/spec/passing*.txt
                                                   no longer passes
                          --update-ratchet         record the passing examples
  fetch-spec [--verify [--offline] | --update-sums]
                        Download the spec fixtures and check their SHA-256
                        sums; --verify checks the local files (and upstream,
                        unless --offline or the network is unreachable).
  pathological [--engine gv-syntax|comrak] [--filter TEXT] [--max-ratio R]
                        Linear-time checks on pathological inputs (release
                        build); --engine comrak validates the suite itself.
  fuzz-seed             Write the fuzz seed corpora (fuzz/corpus/) from the
                        spec examples.
  latency LOG [--warm-up N]
                        Percentiles of a keystroke latency log written with
                        GLYPHVANE_DEBUG_LATENCY=LOG (default warm-up 20).
  package --format F[,F...] [--target T] [--profile P] [--out DIR] [--no-build]
                        Build release artifacts (deb, rpm, tar.gz, appimage,
                        app, zip, dmg, msi) and SHA256SUMS; see
                        `cargo xtask package --help`.
  size-report --bin-dir DIR [--baseline FILE] [--json FILE]
                        Executable sizes against the NFR-SIZE budgets and a
                        baseline (fails on > 3 % growth).
  highlight-bundle [--source DIR] [--linguist FILE]
                        Build gv-highlight's licence-filtered grammar bundle,
                        its manifest and the grammar notices from
                        crates/gv-highlight/bundle/grammars.toml.
  bench-icount [--save-baseline NAME] [--baseline NAME [--limit PCT]]
                        Instruction-count benchmarks under Valgrind (Linux);
                        fails on > 3 % growth against the baseline; see
                        `cargo xtask bench-icount --help`.
";

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Dispatches the command. Returns `Ok(false)` when a check found problems.
fn run() -> Result<bool> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        print!("{HELP}");
        return Ok(true);
    };
    let root = workspace_root();
    match cmd.as_str() {
        "help" | "-h" | "--help" => {
            print!("{HELP}");
            Ok(true)
        }
        "check-headers" => {
            let fix = match rest {
                [] => false,
                [flag] if flag == "--fix" => true,
                _ => bail!("usage: cargo xtask check-headers [--fix]"),
            };
            headers::run(&root, fix)
        }
        "check-deps" => {
            no_args(cmd, rest)?;
            deps::run(&root)
        }
        "ci" => {
            no_args(cmd, rest)?;
            ci::run(&root)
        }
        "spec" => spec::run(&root, rest),
        "fetch-spec" => fixtures::run(&root, rest),
        "pathological" => pathological::run(&root, rest),
        "fuzz-seed" => fuzz_seed::run(&root, rest),
        "latency" => latency::run(rest),
        "package" => package::run(&root, rest),
        "size-report" => package::size::run(rest),
        "bench-icount" => icount::run(&root, rest),
        "highlight-bundle" => highlight_bundle::run(&root, rest),
        other => {
            eprint!("unknown command `{other}`\n\n{HELP}");
            Ok(false)
        }
    }
}

fn no_args(cmd: &str, rest: &[String]) -> Result<()> {
    if rest.is_empty() {
        Ok(())
    } else {
        bail!("`{cmd}` takes no arguments (got: {})", rest.join(" "))
    }
}

/// The workspace root: the parent of the xtask package directory.
fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map_or_else(|| manifest_dir.to_path_buf(), Path::to_path_buf)
}

/// The `cargo` executable to use for subprocesses.
fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}
