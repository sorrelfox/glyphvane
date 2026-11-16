// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `glyphvane-helper`: runs the engines that may hang or crash (Typst PDF
//! export) for `glyphvane`, which starts it with `--serve` and talks to it
//! over standard input and output.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut serve = false;
    let mut memory = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--serve" => serve = true,
            "--memory-limit-mb" => match args.next().and_then(|v| v.parse::<u64>().ok()) {
                Some(mb) => memory = Some(mb),
                None => return usage(),
            },
            "--version" => {
                println!("glyphvane-helper {}", gv_helper::protocol::VERSION);
                return ExitCode::SUCCESS;
            }
            _ => return usage(),
        }
    }
    if !serve {
        return usage();
    }
    if let Some(mb) = memory
        && let Err(e) = gv_helper::serve::limit_memory(mb)
    {
        eprintln!("glyphvane-helper: cannot limit memory: {e}");
        return ExitCode::FAILURE;
    }
    match gv_helper::serve::serve() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glyphvane-helper: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "glyphvane-helper is started by glyphvane.\nUsage: glyphvane-helper --serve [--memory-limit-mb N]"
    );
    ExitCode::from(2)
}
