// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Repository automation, run as `cargo xtask <command>`.

mod ci;
mod deps;
mod headers;

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
                        check-deps, clippy, test.
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
