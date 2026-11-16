// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The binary's side of the keystroke latency log (`gv_ui::latency`).
//!
//! - `GLYPHVANE_DEBUG_LATENCY=FILE` writes one record per keystroke to
//!   `FILE` (`-` or `1`: standard error). Type in a Source view; the summary
//!   is printed when the application quits. `cargo xtask latency FILE`
//!   summarises a log.
//! - `GLYPHVANE_DEBUG_LATENCY_TYPE=COUNT[:INTERVAL_MS]` also types `COUNT`
//!   keystrokes into the first window, one every `INTERVAL_MS` (default 47)
//!   plus up to a quarter of that at random, once a Source view has painted
//!   (open a file with `GLYPHVANE_DEBUG_EDIT=1`), then prints the summary
//!   and quits. The keystrokes go through the window's normal key dispatch,
//!   not through the platform, so the time the platform takes to deliver a
//!   key is not in the numbers.
//!
//! On Wayland the compositor's presentation feedback (`wp_presentation`)
//! gives the time each frame was presented, on GPUI's own connection. On
//! X11 and elsewhere the log has no presentation time; the frame callback
//! column is the closest stand-in.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::time::Duration;

use gpui::{AnyWindowHandle, App, Keystroke};

/// The environment variable that turns the log on.
pub const DEBUG_LATENCY_ENV: &str = "GLYPHVANE_DEBUG_LATENCY";
/// The environment variable that types keystrokes and quits.
pub const DEBUG_LATENCY_TYPE_ENV: &str = "GLYPHVANE_DEBUG_LATENCY_TYPE";

/// Records left out of the summary: the first frames shape new glyphs.
const WARM_UP: usize = 20;
/// How often the presentation answers are collected.
const POLL: Duration = Duration::from_millis(50);
/// The keys typed, in turn.
const KEYS: [&str; 8] = ["a", "b", "c", "space", "x", "y", "z", "space"];

/// An automatic typing run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Typing {
    count: usize,
    interval: Duration,
}

impl Typing {
    fn parse(spec: &str) -> Option<Self> {
        let (count, interval) = match spec.split_once(':') {
            Some((c, i)) => (c, i.trim().parse().ok()?),
            None => (spec, 47),
        };
        Some(Self {
            count: count.trim().parse().ok()?,
            interval: Duration::from_millis(interval),
        })
    }
}

/// Starts the log when the environment asks for it. Call once, inside
/// `Application::run`, before opening a window.
pub fn install(cx: &mut App) {
    let typing = std::env::var(DEBUG_LATENCY_TYPE_ENV)
        .ok()
        .and_then(|s| Typing::parse(&s));
    let target = match std::env::var_os(DEBUG_LATENCY_ENV) {
        Some(t) if !t.is_empty() && t != "0" => Some(t),
        _ if typing.is_some() => Some("-".into()),
        _ => None,
    };
    let Some(target) = target else {
        return;
    };
    let out: Box<dyn Write> = if target == "-" || target == "1" {
        Box::new(io::stderr())
    } else {
        match File::create(&target) {
            Ok(file) => Box::new(BufWriter::new(file)),
            Err(err) => {
                eprintln!(
                    "glyphvane: {DEBUG_LATENCY_ENV}: cannot create {}: {err}",
                    target.to_string_lossy()
                );
                return;
            }
        }
    };
    gv_ui::latency::start(out, cx);
    cx.spawn(async |cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            cx.update(|_| gv_ui::latency::poll());
        }
    })
    .detach();
    cx.on_app_quit(|_| {
        if std::env::var_os(DEBUG_LATENCY_TYPE_ENV).is_none() {
            eprint!("{}", gv_ui::latency::finish(WARM_UP));
        }
        async {}
    })
    .detach();
}

/// Attaches the presentation probe to `window` and starts the typing run,
/// when the log is on.
pub fn attach(window: AnyWindowHandle, cx: &mut App) {
    if !gv_ui::latency::enabled() {
        return;
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let probe = window
            .update(cx, |_, window, _| crate::present::Probe::new(window))
            .ok()
            .flatten();
        match probe {
            Some(probe) => gv_ui::latency::set_probe(Box::new(probe)),
            None => eprintln!(
                "glyphvane: {DEBUG_LATENCY_ENV}: no Wayland presentation feedback; \
                 the log has no presentation times"
            ),
        }
    }
    let Some(typing) = std::env::var(DEBUG_LATENCY_TYPE_ENV)
        .ok()
        .and_then(|s| Typing::parse(&s))
    else {
        return;
    };
    cx.spawn(async move |cx| {
        let mut waited = Duration::ZERO;
        while !cx.update(|_| gv_ui::latency::editor_painted()) {
            cx.background_executor().timer(POLL).await;
            waited += POLL;
            if waited > Duration::from_secs(10) {
                eprintln!(
                    "glyphvane: {DEBUG_LATENCY_TYPE_ENV}: no Source view painted; \
                     open a file with GLYPHVANE_DEBUG_EDIT=1"
                );
                cx.update(|cx| cx.quit());
                return;
            }
        }
        cx.background_executor()
            .timer(Duration::from_millis(500))
            .await;
        // A fixed seed: runs type the same keys at the same offsets.
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let quarter = u64::try_from(typing.interval.as_micros() / 4).unwrap_or(0);
        for i in 0..typing.count {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let jitter = Duration::from_micros(seed % quarter.max(1));
            cx.background_executor()
                .timer(typing.interval + jitter)
                .await;
            let Ok(key) = Keystroke::parse(KEYS[i % KEYS.len()]) else {
                continue;
            };
            let typed = window.update(cx, |_, window, cx| {
                window.dispatch_keystroke(key, cx);
            });
            if typed.is_err() {
                return;
            }
        }
        cx.background_executor()
            .timer(Duration::from_millis(500))
            .await;
        let summary = cx.update(|_| gv_ui::latency::finish(WARM_UP));
        eprint!("{summary}");
        cx.update(|cx| cx.quit());
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_typing_spec() {
        assert_eq!(
            Typing::parse("400"),
            Some(Typing {
                count: 400,
                interval: Duration::from_millis(47)
            })
        );
        assert_eq!(
            Typing::parse("10:100"),
            Some(Typing {
                count: 10,
                interval: Duration::from_millis(100)
            })
        );
        assert_eq!(Typing::parse("x"), None);
    }
}
