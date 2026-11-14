// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A debug benchmark of the Reading view in a real window.
//!
//! With `GLYPHVANE_DEBUG_READER_BENCH=STEP:FRAMES` (for example `24:600`)
//! the first document that shows content scrolls `STEP` px per frame for
//! `FRAMES` frames; then the reader prints the time from the open request to
//! the first frame with content, where loading spent its time, and frame
//! CPU percentiles (from the reader's `render` to the end of its paint), and
//! quits the application.

use std::time::Duration;

use super::load::Timings;

/// The environment variable that starts the benchmark.
pub const DEBUG_READER_BENCH_ENV: &str = "GLYPHVANE_DEBUG_READER_BENCH";

/// Frames skipped before timing (window creation, first shaping).
const WARM_UP: usize = 10;

/// A running benchmark.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Bench {
    pub step: f32,
    pub frames: usize,
    pub done: usize,
}

impl Bench {
    /// The benchmark asked for in the environment, if any.
    pub fn from_env() -> Option<Self> {
        Self::parse(&std::env::var(DEBUG_READER_BENCH_ENV).ok()?)
    }

    pub fn parse(spec: &str) -> Option<Self> {
        let (step, frames) = spec.split_once(':')?;
        Some(Self {
            step: step.trim().parse().ok()?,
            frames: frames.trim().parse().ok()?,
            done: 0,
        })
    }

    /// Counts a frame; `true` when the run is over.
    pub fn tick(&mut self) -> bool {
        self.done += 1;
        self.done > self.frames + WARM_UP
    }

    pub fn timing(&self) -> bool {
        self.done > WARM_UP
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// `p50 … p95 … p99 … max …` of `samples`, in ms.
pub(crate) fn percentiles(samples: &[Duration]) -> String {
    if samples.is_empty() {
        return "no samples".to_owned();
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    let at = |q: f64| s[((s.len() - 1) as f64 * q).round() as usize];
    format!(
        "p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, max {:.2} ms ({} frames)",
        ms(at(0.5)),
        ms(at(0.95)),
        ms(at(0.99)),
        ms(at(1.0)),
        s.len()
    )
}

/// One line on where loading spent its time.
pub(crate) fn load_line(t: &Timings) -> String {
    format!(
        "{} bytes: read {:.1} ms, decode {:.1} ms, parse {:.1} ms, model {:.1} ms, render tree {:.1} ms",
        t.bytes,
        ms(t.read),
        ms(t.decode),
        ms(t.parse),
        ms(t.model),
        ms(t.tree)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_spec_and_summarises() {
        assert_eq!(
            Bench::parse("24:600"),
            Some(Bench {
                step: 24.0,
                frames: 600,
                done: 0
            })
        );
        assert_eq!(Bench::parse("x"), None);
        let samples: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        let line = percentiles(&samples);
        assert!(line.starts_with("p50 51.00 ms"), "{line}");
        assert!(line.contains("max 100.00 ms"), "{line}");
    }
}
