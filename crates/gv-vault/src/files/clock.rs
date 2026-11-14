// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A monotonic clock that tests can drive.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The time source of the autosave policy and the journal's sync timer.
pub trait Clock {
    /// The current instant.
    fn now(&self) -> Instant;
}

/// [`Instant::now`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A clock that only moves when told to. Clones share the same time.
#[derive(Clone, Debug)]
pub struct ManualClock {
    start: Instant,
    offset_nanos: Arc<AtomicU64>,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualClock {
    /// A clock standing at an arbitrary instant.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            offset_nanos: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        let nanos = u64::try_from(by.as_nanos()).unwrap_or(u64::MAX);
        self.offset_nanos.fetch_add(nanos, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.start + Duration::from_nanos(self.offset_nanos.load(Ordering::SeqCst))
    }
}

impl<C: Clock + ?Sized> Clock for &C {
    fn now(&self) -> Instant {
        (**self).now()
    }
}

impl<C: Clock + ?Sized> Clock for Arc<C> {
    fn now(&self) -> Instant {
        (**self).now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_moves_only_when_told() {
        let clock = ManualClock::new();
        let shared = clock.clone();
        let t0 = clock.now();
        assert_eq!(clock.now(), t0);
        shared.advance(Duration::from_millis(1500));
        assert_eq!(clock.now() - t0, Duration::from_millis(1500));
    }
}
