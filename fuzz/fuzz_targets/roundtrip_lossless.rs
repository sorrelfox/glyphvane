// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Checks the CST is lossless: the concatenated token texts equal the input.
//!
//! Built by cargo-fuzz (`--cfg fuzzing`) this is a libFuzzer target; built
//! normally it is a standalone runner (see `fuzz/README.md`).

#![cfg_attr(fuzzing, no_main)]

#[cfg(fuzzing)]
libfuzzer_sys::fuzz_target!(|data: &[u8]| glyphvane_fuzz::roundtrip_lossless(data));

#[cfg(not(fuzzing))]
fn main() -> std::process::ExitCode {
    glyphvane_fuzz::standalone::main("roundtrip_lossless", glyphvane_fuzz::roundtrip_lossless)
}
