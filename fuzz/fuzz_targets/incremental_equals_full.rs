// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Parses a document, applies an edit script incrementally, and checks the result equals a full parse of the edited text.
//!
//! Built by cargo-fuzz (`--cfg fuzzing`) this is a libFuzzer target; built
//! normally it is a standalone runner (see `fuzz/README.md`).

#![cfg_attr(fuzzing, no_main)]

#[cfg(fuzzing)]
libfuzzer_sys::fuzz_target!(|data: &[u8]| glyphvane_fuzz::incremental_equals_full(data));

#[cfg(not(fuzzing))]
fn main() -> std::process::ExitCode {
    glyphvane_fuzz::standalone::main(
        "incremental_equals_full",
        glyphvane_fuzz::incremental_equals_full,
    )
}
