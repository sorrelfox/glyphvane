// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Parses arbitrary bytes (decoded lossily as UTF-8) and renders them; must not panic.
//!
//! Built by cargo-fuzz (`--cfg fuzzing`) this is a libFuzzer target; built
//! normally it is a standalone runner (see `fuzz/README.md`).

#![cfg_attr(fuzzing, no_main)]

#[cfg(fuzzing)]
libfuzzer_sys::fuzz_target!(|data: &[u8]| glyphvane_fuzz::parse_full(data));

#[cfg(not(fuzzing))]
fn main() -> std::process::ExitCode {
    glyphvane_fuzz::standalone::main("parse_full", glyphvane_fuzz::parse_full)
}
