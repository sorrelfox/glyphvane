// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Time for undo grouping, behind a trait so tests control it.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A monotonic clock. The editor asks it for the time of every edit, to
/// decide whether consecutive edits form one undo group.
pub trait Clock: fmt::Debug {
    /// The time since a fixed, arbitrary origin. Never decreases.
    fn now(&self) -> Duration;
}

/// The real clock: time since the clock was created.
#[derive(Clone, Copy, Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// A clock whose origin is now.
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// A clock that only moves when told to. Clones share the same time, so a
/// test keeps one clone and hands another to the editor.
#[derive(Clone, Debug, Default)]
pub struct ManualClock {
    nanos: Arc<AtomicU64>,
}

impl ManualClock {
    /// A clock at time zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves the clock forward by `by`.
    pub fn advance(&self, by: Duration) {
        let by = u64::try_from(by.as_nanos()).unwrap_or(u64::MAX);
        let _ = self
            .nanos
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |now| {
                Some(now.saturating_add(by))
            });
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.nanos.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_is_shared_between_clones() {
        let clock = ManualClock::new();
        let other = clock.clone();
        assert_eq!(other.now(), Duration::ZERO);
        clock.advance(Duration::from_millis(1500));
        assert_eq!(other.now(), Duration::from_millis(1500));
        clock.advance(Duration::MAX);
        assert_eq!(other.now(), Duration::from_nanos(u64::MAX));
    }

    #[test]
    fn system_clock_does_not_go_back() {
        let clock = SystemClock::new();
        let a = clock.now();
        let b = clock.now();
        assert!(b >= a);
    }
}
