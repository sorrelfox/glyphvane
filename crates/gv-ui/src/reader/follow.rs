// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Timing of navigation: a followed link, Back and Forward, from the click
//! (or key) to the first frame that paints the target with content. One
//! INFO line per navigation (`gv_ui::reader` target), for example:
//!
//! ```text
//! link followed: other.md#usage → /notes/other.md#usage, target painted 9.4 ms after the click
//! back: /notes/a.md at byte 1840, target painted 6.1 ms after the click
//! ```
//!
//! The input that starts a navigation is noted here ([`begin`]); the
//! [`Reader::open`](super::Reader::open) or
//! [`Reader::scroll_to`](super::Reader::scroll_to) that the window calls
//! while it handles that input takes it ([`take`]), and the Reading view
//! logs it at its next frame with content. A note that no view takes at
//! once (an external or refused link, nothing to go back to) is ignored.
//!
//! Noted: link clicks in a Reading view, and every Back and Forward (keys,
//! menus, mouse buttons and the toolbar buttons) where the window does it.

use std::cell::RefCell;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::ScrollTarget;

/// What started a navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Cause {
    /// A link to `destination` (as written) was followed; `background` when
    /// it opens in a tab behind the active one.
    Link {
        destination: Arc<str>,
        background: bool,
    },
    Back,
    Forward,
}

/// A navigation noted and not yet taken by a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    pub cause: Cause,
    pub at: Instant,
}

thread_local! {
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    static NOTED: RefCell<Vec<Cause>> = const { RefCell::new(Vec::new()) };
    static TAKEN: RefCell<Vec<Cause>> = const { RefCell::new(Vec::new()) };
}

/// The navigations noted and those taken by a view since the last call.
#[cfg(test)]
pub(crate) fn drain_records() -> (Vec<Cause>, Vec<Cause>) {
    (
        NOTED.with_borrow_mut(std::mem::take),
        TAKEN.with_borrow_mut(std::mem::take),
    )
}

/// Notes the input that starts a navigation now.
pub(crate) fn begin(cause: Cause) {
    #[cfg(test)]
    NOTED.with_borrow_mut(|noted| noted.push(cause.clone()));
    PENDING.with_borrow_mut(|pending| {
        *pending = Some(Pending {
            cause,
            at: Instant::now(),
        });
    });
}

/// The window shows a navigation's target within this time of the input,
/// in the same turn of the event loop; an older note started nothing.
const FRESH: Duration = Duration::from_millis(250);

/// The navigation noted, for the view that shows its target.
pub(crate) fn take() -> Option<Pending> {
    let pending = PENDING
        .with_borrow_mut(Option::take)
        .filter(|p| p.at.elapsed() <= FRESH);
    #[cfg(test)]
    if let Some(p) = &pending {
        TAKEN.with_borrow_mut(|taken| taken.push(p.cause.clone()));
    }
    pending
}

/// A navigation that a view took: what to log once its target is painted.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Navigation {
    pub cause: Cause,
    pub at: Instant,
    pub target: Option<ScrollTarget>,
}

impl Navigation {
    pub fn new(pending: Pending, target: Option<ScrollTarget>) -> Self {
        Self {
            cause: pending.cause,
            at: pending.at,
            target,
        }
    }

    /// Whether the target shows in a tab behind the active one, so that
    /// nothing is painted for it now.
    pub fn in_background(&self) -> bool {
        matches!(
            self.cause,
            Cause::Link {
                background: true,
                ..
            }
        )
    }

    /// The log line for the target `path`, painted `elapsed` after the
    /// input (`None`: not painted, in a background tab).
    pub fn line(&self, path: &Path, elapsed: Option<Duration>) -> String {
        let place = Place {
            path,
            target: self.target.as_ref(),
        };
        let what = match &self.cause {
            Cause::Link { destination, .. } => format!("link followed: {destination} → {place}"),
            Cause::Back => format!("back: {place}"),
            Cause::Forward => format!("forward: {place}"),
        };
        match elapsed {
            Some(elapsed) => format!(
                "{what}, target painted {:.1} ms after the click",
                elapsed.as_secs_f64() * 1e3
            ),
            None => format!("{what}, in a background tab"),
        }
    }
}

struct Place<'a> {
    path: &'a Path,
    target: Option<&'a ScrollTarget>,
}

impl fmt::Display for Place<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        match self.target {
            Some(ScrollTarget::Fragment(fragment)) => write!(f, "#{}", fragment.raw),
            Some(ScrollTarget::Line(line)) => write!(f, ":{line}"),
            Some(ScrollTarget::Offset(offset)) => write!(f, " at byte {offset}"),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_is_taken_once_and_old_notes_are_ignored() {
        assert_eq!(take(), None);
        begin(Cause::Back);
        assert_eq!(take().map(|p| p.cause), Some(Cause::Back));
        assert_eq!(take(), None);
        PENDING.with_borrow_mut(|pending| {
            *pending = Some(Pending {
                cause: Cause::Forward,
                at: Instant::now() - Duration::from_secs(1),
            });
        });
        assert_eq!(take(), None);
    }

    #[test]
    fn the_lines() {
        let at = Instant::now();
        let link = |background| Navigation {
            cause: Cause::Link {
                destination: Arc::from("b.md#usage"),
                background,
            },
            at,
            target: Some(ScrollTarget::Offset(0)),
        };
        let path = Path::new("/notes/b.md");
        assert_eq!(
            link(false).line(path, Some(Duration::from_micros(9_440))),
            "link followed: b.md#usage → /notes/b.md at byte 0, target painted 9.4 ms after the click"
        );
        assert!(link(true).in_background());
        assert_eq!(
            link(true).line(path, None),
            "link followed: b.md#usage → /notes/b.md at byte 0, in a background tab"
        );
        let back = Navigation {
            cause: Cause::Back,
            at,
            target: Some(ScrollTarget::Line(12)),
        };
        assert!(!back.in_background());
        assert_eq!(
            back.line(path, Some(Duration::from_millis(6))),
            "back: /notes/b.md:12, target painted 6.0 ms after the click"
        );
        let forward = Navigation {
            cause: Cause::Forward,
            at,
            target: None,
        };
        assert_eq!(
            forward.line(path, Some(Duration::from_millis(1))),
            "forward: /notes/b.md, target painted 1.0 ms after the click"
        );
    }
}
