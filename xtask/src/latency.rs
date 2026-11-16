// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `cargo xtask latency LOG [--warm-up N]`: percentiles of a keystroke
//! latency log written with `GLYPHVANE_DEBUG_LATENCY=LOG` (the columns are
//! in `gv_ui::latency::HEADER`; times are ms after the keystroke).

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};

const USAGE: &str = "usage: cargo xtask latency LOG [--warm-up N]";

/// The timed columns: index and name.
const COLUMNS: [(usize, &str); 5] = [
    (4, "input -> handled"),
    (5, "input -> drawn"),
    (6, "input -> submitted"),
    (7, "input -> frame callback"),
    (8, "input -> presented"),
];

pub(crate) fn run(rest: &[String]) -> Result<bool> {
    let mut log = None;
    let mut warm_up = 20;
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--warm-up" => {
                warm_up = args
                    .next()
                    .context(USAGE)?
                    .parse()
                    .context("--warm-up takes a number")?;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(true);
            }
            other if log.is_none() => log = Some(other.to_owned()),
            other => bail!("unexpected argument `{other}`\n{USAGE}"),
        }
    }
    let log = log.context(USAGE)?;
    let text =
        std::fs::read_to_string(Path::new(&log)).with_context(|| format!("reading {log}"))?;
    print!("{}", summary(&text, warm_up));
    Ok(true)
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn summary(text: &str, warm_up: usize) -> String {
    let rows: Vec<Vec<&str>> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|f| f.len() >= 10)
        .skip(warm_up)
        .collect();
    let mut out = String::new();
    let inactive = rows.iter().filter(|f| f[2] == "0").count();
    let _ = writeln!(
        out,
        "{} keystrokes after {warm_up} warm-up; {inactive} while the window was not active",
        rows.len()
    );
    for (col, name) in COLUMNS {
        let mut v: Vec<f64> = rows.iter().filter_map(|f| f[col].parse().ok()).collect();
        if v.is_empty() {
            let _ = writeln!(out, "{name}: no samples");
            continue;
        }
        v.sort_by(f64::total_cmp);
        let _ = writeln!(
            out,
            "{name}: p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, max {:.2} ms (n {})",
            percentile(&v, 0.5),
            percentile(&v, 0.95),
            percentile(&v, 0.99),
            percentile(&v, 1.0),
            v.len()
        );
    }
    let pairs: Vec<(f64, f64)> = rows
        .iter()
        .filter_map(|f| Some((f[8].parse().ok()?, f[9].parse().ok()?)))
        .collect();
    if let Some(&(_, refresh)) = pairs.first() {
        let within = pairs.iter().filter(|(p, r)| p <= r).count();
        let mut frames: Vec<f64> = pairs.iter().map(|(p, r)| p / r).collect();
        frames.sort_by(f64::total_cmp);
        let _ = writeln!(
            out,
            "input -> presented in refreshes of {refresh:.3} ms: p50 {:.2}, p95 {:.2}, p99 {:.2}; \
             within 1 refresh: {within} of {} ({:.1} %)",
            percentile(&frames, 0.5),
            percentile(&frames, 0.95),
            percentile(&frames, 0.99),
            pairs.len(),
            100.0 * within as f64 / pairs.len() as f64
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarises_a_log() {
        let mut text = String::from("# seq\tkey\n");
        for i in 0..100 {
            let p = 5.0 + f64::from(i) * 0.1;
            let _ = writeln!(
                text,
                "{i}\ta\t1\t{i}.000\t0.050\t0.900\t1.000\t{:.3}\t{p:.3}\t13.333",
                p - 1.0
            );
        }
        text.push_str("100\tb\t0\t100.000\t-\t-\t-\t-\t20.000\t13.333\n");
        let s = summary(&text, 0);
        assert!(
            s.starts_with("101 keystrokes after 0 warm-up; 1 while"),
            "{s}"
        );
        assert!(s.contains("input -> handled: p50 0.05 ms"), "{s}");
        assert!(s.contains("within 1 refresh: 84 of 101"), "{s}");
        assert!(summary(&text, 200).contains("input -> drawn: no samples"));
    }
}
