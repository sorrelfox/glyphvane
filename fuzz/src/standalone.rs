// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The standalone runner used when the targets are built without
//! cargo-fuzz (stable toolchain, no libFuzzer): it replays a corpus and can
//! run a simple mutation loop as a smoke test.
//!
//! ```text
//! <target> [--iterations N] [--seed S] [--timeout-ms T] [PATH...]
//! ```
//!
//! PATHs are files or directories (default: `fuzz/corpus/<target>`). A
//! panicking or slow input is written to `fuzz/artifacts/<target>/` and the
//! runner exits with status 1. This is not coverage-guided; use cargo-fuzz
//! for real fuzzing.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// Markdown tokens the mutator inserts.
const DICTIONARY: &[&[u8]] = &[
    b"\n", b"\n\n", b"> ", b"- ", b"1. ", b"* ", b"# ", b"```", b"~~~", b"    ", b"\t", b"*",
    b"**", b"_", b"`", b"[", b"]", b"(", b")", b"![", b"]: ", b"<", b">", b"<!--", b"-->",
    b"<div>", b"&amp;", b"&#", b"\\", b"---", b"===", b"|", b"\r\n", b"\xff",
];

struct Opts {
    iterations: u64,
    seed: u64,
    timeout: Duration,
    paths: Vec<PathBuf>,
}

fn parse_args(name: &str) -> Result<Opts, String> {
    let mut o = Opts {
        iterations: 0,
        seed: 1,
        timeout: Duration::from_millis(2_000),
        paths: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut num = |what: &str| -> Result<u64, String> {
            args.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{what} needs a number"))
        };
        match a.as_str() {
            "--iterations" => o.iterations = num("--iterations")?,
            "--seed" => o.seed = num("--seed")?,
            "--timeout-ms" => o.timeout = Duration::from_millis(num("--timeout-ms")?),
            "-h" | "--help" => {
                return Err(format!(
                    "usage: {name} [--iterations N] [--seed S] [--timeout-ms T] [PATH...]"
                ));
            }
            _ => o.paths.push(PathBuf::from(a)),
        }
    }
    if o.paths.is_empty() {
        o.paths.push(fuzz_dir().join("corpus").join(name));
    }
    Ok(o)
}

fn fuzz_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            collect(&p, out);
        }
    } else if path.is_file() {
        out.push(path.to_path_buf());
    }
}

/// FNV-1a, for artifact names.
fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

fn save_artifact(name: &str, kind: &str, data: &[u8]) -> PathBuf {
    let dir = fuzz_dir().join("artifacts").join(name);
    let path = dir.join(format!("{kind}-{:016x}", fnv(data)));
    if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&path, data).is_ok() {
        path
    } else {
        PathBuf::from("<could not write artifact>")
    }
}

/// Runs one input; on a panic or timeout saves it and returns a message.
fn run_one(name: &str, target: fn(&[u8]), data: &[u8], timeout: Duration) -> Result<(), String> {
    let start = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| target(data)));
    let elapsed = start.elapsed();
    if result.is_err() {
        return Err(format!(
            "panic; input saved to {}",
            save_artifact(name, "crash", data).display()
        ));
    }
    if elapsed > timeout {
        let path = save_artifact(name, "slow", data);
        return Err(format!(
            "took {elapsed:?} (limit {timeout:?}); input saved to {}",
            path.display()
        ));
    }
    Ok(())
}

/// xorshift64*.
fn next(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

fn below(state: &mut u64, n: usize) -> usize {
    usize::try_from(next(state) % n.max(1) as u64).unwrap_or(0)
}

fn mutate(state: &mut u64, input: &[u8], corpus: &[Vec<u8>]) -> Vec<u8> {
    let mut data = input.to_vec();
    for _ in 0..=below(state, 4) {
        let pos = below(state, data.len() + 1);
        match below(state, 6) {
            0 if !data.is_empty() => {
                let i = below(state, data.len());
                data[i] ^= 1 << below(state, 8);
            }
            1 => {
                let token = DICTIONARY[below(state, DICTIONARY.len())];
                data.splice(pos..pos, token.iter().copied());
            }
            2 if !data.is_empty() => {
                let end = (pos + below(state, 16)).min(data.len());
                data.drain(pos..end);
            }
            3 if !data.is_empty() => {
                let start = below(state, data.len());
                let end = (start + below(state, 64)).min(data.len());
                let chunk = data[start..end].to_vec();
                data.splice(pos..pos, chunk);
            }
            4 if !corpus.is_empty() => {
                let other = &corpus[below(state, corpus.len())];
                let cut = below(state, other.len() + 1);
                data.truncate(pos);
                data.extend_from_slice(&other[cut..]);
            }
            _ => data.insert(pos, b"\n- >*_`[]#"[below(state, 10)]),
        }
    }
    data
}

/// Entry point of a standalone target binary.
pub fn main(name: &str, target: fn(&[u8])) -> ExitCode {
    let opts = match parse_args(name) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };
    let mut files = Vec::new();
    for p in &opts.paths {
        collect(p, &mut files);
    }
    let corpus: Vec<Vec<u8>> = files.iter().filter_map(|f| std::fs::read(f).ok()).collect();
    eprintln!(
        "{name}: replaying {} inputs (run `cargo xtask fuzz-seed` to create the corpus)",
        corpus.len()
    );
    for (file, data) in files.iter().zip(&corpus) {
        if let Err(e) = run_one(name, target, data, opts.timeout) {
            eprintln!("{name}: {}: {e}", file.display());
            return ExitCode::FAILURE;
        }
    }
    if opts.iterations > 0 {
        eprintln!("{name}: {} mutations (seed {})", opts.iterations, opts.seed);
        let mut state = opts.seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let empty = Vec::new();
        for _ in 0..opts.iterations {
            let base = if corpus.is_empty() {
                &empty
            } else {
                &corpus[below(&mut state, corpus.len())]
            };
            let data = mutate(&mut state, base, &corpus);
            if let Err(e) = run_one(name, target, &data, opts.timeout) {
                eprintln!("{name}: mutated input: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    eprintln!("{name}: ok");
    ExitCode::SUCCESS
}
