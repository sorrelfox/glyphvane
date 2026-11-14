// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The autosave policy, as plain logic over a [`Clock`].
//!
//! Autosave saves 1 s after the last edit, on focus loss, on tab switch and
//! on quit. It is suppressed during IME composition, while a conflict banner
//! is open, in viewer mode and when the setting is off. The policy owns no
//! timer: the UI asks [`Autosave::next_deadline`] when to call
//! [`Autosave::poll`], runs the save a [`SaveRequest`] asks for, and reports
//! the result.
//!
//! Edits are counted in generations, so an edit made while a save runs keeps
//! the document dirty after that save finishes.

use std::time::{Duration, Instant};

use super::clock::{Clock, SystemClock};

/// Autosave tuning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutosaveSettings {
    /// Whether autosave runs at all (`files.autosave`).
    pub enabled: bool,
    /// Idle time after the last edit before saving.
    pub idle_delay: Duration,
    /// Wait after a failed save before the idle trigger tries again (focus
    /// loss, tab switch and quit still try at once).
    pub retry_delay: Duration,
}

impl Default for AutosaveSettings {
    /// On, 1 s idle, 5 s retry.
    fn default() -> Self {
        Self {
            enabled: true,
            idle_delay: Duration::from_secs(1),
            retry_delay: Duration::from_secs(5),
        }
    }
}

impl AutosaveSettings {
    /// The defaults with `enabled` from the `files` settings section.
    pub fn from_settings(files: &gv_settings::Files) -> Self {
        Self {
            enabled: files.autosave,
            ..Self::default()
        }
    }
}

/// What triggered a save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveReason {
    /// The idle delay passed after the last edit.
    Idle,
    /// The window lost focus.
    FocusLost,
    /// The user switched to another tab.
    TabSwitch,
    /// The app is quitting.
    Quit,
    /// The user asked (Mod+S).
    Manual,
}

/// A save the UI should run now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveRequest {
    /// Why.
    pub reason: SaveReason,
    /// The edit generation the save covers; pass the request back to
    /// [`Autosave::save_succeeded`] or [`Autosave::save_failed`].
    pub generation: u64,
}

/// Why autosave is not saving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suppressed {
    /// The setting is off.
    Disabled,
    /// Viewer mode never edits, so it never saves.
    Viewer,
    /// An IME composition is in progress.
    Composing,
    /// A conflict banner is open.
    Conflict,
}

/// What to do with a document when the app quits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitDecision {
    /// Nothing unsaved.
    Clean,
    /// Save first (end any IME composition before).
    Save(SaveRequest),
    /// A save is running; wait for it, then ask again.
    WaitForSave,
    /// Unsaved edits that autosave may not write: ask the user.
    Ask(Suppressed),
}

/// The autosave state of one document.
#[derive(Clone, Debug)]
pub struct Autosave<C: Clock = SystemClock> {
    settings: AutosaveSettings,
    clock: C,
    generation: u64,
    saved_generation: u64,
    last_edit: Option<Instant>,
    composing: bool,
    conflict: bool,
    viewer: bool,
    in_flight: Option<u64>,
    retry_at: Option<Instant>,
}

impl<C: Clock> Autosave<C> {
    /// A clean document.
    pub fn new(settings: AutosaveSettings, clock: C) -> Self {
        Self {
            settings,
            clock,
            generation: 0,
            saved_generation: 0,
            last_edit: None,
            composing: false,
            conflict: false,
            viewer: false,
            in_flight: None,
            retry_at: None,
        }
    }

    /// The settings.
    pub fn settings(&self) -> AutosaveSettings {
        self.settings
    }

    /// Applies changed settings (live reload).
    pub fn set_settings(&mut self, settings: AutosaveSettings) {
        self.settings = settings;
    }

    /// An edit was applied.
    pub fn edited(&mut self) {
        self.generation += 1;
        self.last_edit = Some(self.clock.now());
    }

    /// Whether there are edits no finished save covers.
    pub fn is_dirty(&self) -> bool {
        self.generation != self.saved_generation
    }

    /// The document now matches the file without a save through this policy
    /// (reloaded, reverted, or saved some other way).
    pub fn mark_clean(&mut self) {
        self.saved_generation = self.generation;
        self.retry_at = None;
    }

    /// An IME composition started or ended. Ending one restarts the idle
    /// delay.
    pub fn set_composing(&mut self, composing: bool) {
        if self.composing && !composing && self.is_dirty() {
            self.last_edit = Some(self.clock.now());
        }
        self.composing = composing;
    }

    /// A conflict banner opened or closed.
    pub fn set_conflict(&mut self, open: bool) {
        self.conflict = open;
    }

    /// Viewer mode turned on or off.
    pub fn set_viewer(&mut self, viewer: bool) {
        self.viewer = viewer;
    }

    /// Why autosave is held back right now, if it is.
    pub fn suppressed(&self) -> Option<Suppressed> {
        if self.viewer {
            Some(Suppressed::Viewer)
        } else if !self.settings.enabled {
            Some(Suppressed::Disabled)
        } else if self.composing {
            Some(Suppressed::Composing)
        } else if self.conflict {
            Some(Suppressed::Conflict)
        } else {
            None
        }
    }

    fn request(&mut self, reason: SaveReason) -> Option<SaveRequest> {
        if !self.is_dirty() || self.in_flight.is_some() {
            return None;
        }
        self.in_flight = Some(self.generation);
        Some(SaveRequest {
            reason,
            generation: self.generation,
        })
    }

    fn automatic(&mut self, reason: SaveReason) -> Option<SaveRequest> {
        if self.suppressed().is_some() {
            return None;
        }
        self.request(reason)
    }

    /// The timer fired: saves when the idle delay (and any retry delay) has
    /// passed.
    pub fn poll(&mut self) -> Option<SaveRequest> {
        let deadline = self.next_deadline()?;
        if self.clock.now() < deadline {
            return None;
        }
        self.automatic(SaveReason::Idle)
    }

    /// When to call [`Autosave::poll`] next; `None` while there is nothing
    /// to wait for.
    pub fn next_deadline(&self) -> Option<Instant> {
        if !self.is_dirty() || self.in_flight.is_some() || self.suppressed().is_some() {
            return None;
        }
        let idle = self.last_edit? + self.settings.idle_delay;
        Some(self.retry_at.map_or(idle, |retry| retry.max(idle)))
    }

    /// The window lost focus.
    pub fn focus_lost(&mut self) -> Option<SaveRequest> {
        self.automatic(SaveReason::FocusLost)
    }

    /// The user switched away from this document's tab.
    pub fn tab_switched(&mut self) -> Option<SaveRequest> {
        self.automatic(SaveReason::TabSwitch)
    }

    /// The user asked to save. Not held back by the autosave suppressions
    /// except viewer mode (the save itself still refuses to overwrite an
    /// external change).
    pub fn manual(&mut self) -> Option<SaveRequest> {
        if self.viewer {
            return None;
        }
        self.request(SaveReason::Manual)
    }

    /// The app is quitting.
    pub fn quitting(&mut self) -> QuitDecision {
        if !self.is_dirty() {
            return QuitDecision::Clean;
        }
        if self.in_flight.is_some() {
            return QuitDecision::WaitForSave;
        }
        match self.suppressed() {
            // An open composition is committed before quitting.
            None | Some(Suppressed::Composing) => self
                .request(SaveReason::Quit)
                .map_or(QuitDecision::Clean, QuitDecision::Save),
            Some(reason) => QuitDecision::Ask(reason),
        }
    }

    /// The requested save finished and wrote (or found unchanged) the
    /// requested generation.
    pub fn save_succeeded(&mut self, request: SaveRequest) {
        self.in_flight = None;
        self.saved_generation = self.saved_generation.max(request.generation);
        self.retry_at = None;
    }

    /// The requested save failed; the idle trigger waits for the retry
    /// delay.
    pub fn save_failed(&mut self, _request: SaveRequest) {
        self.in_flight = None;
        self.retry_at = Some(self.clock.now() + self.settings.retry_delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::clock::ManualClock;

    fn policy() -> (Autosave<ManualClock>, ManualClock) {
        let clock = ManualClock::new();
        (
            Autosave::new(AutosaveSettings::default(), clock.clone()),
            clock,
        )
    }

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn saves_after_one_second_idle() {
        let (mut autosave, clock) = policy();
        assert_eq!(autosave.poll(), None);
        assert_eq!(autosave.next_deadline(), None);
        autosave.edited();
        clock.advance(MS(600));
        autosave.edited();
        assert_eq!(autosave.next_deadline(), Some(clock.now() + MS(1000)));
        clock.advance(MS(999));
        assert_eq!(autosave.poll(), None);
        clock.advance(MS(1));
        let request = autosave.poll().expect("idle save");
        assert_eq!(request.reason, SaveReason::Idle);
        assert_eq!(autosave.poll(), None, "one save at a time");
        autosave.save_succeeded(request);
        assert!(!autosave.is_dirty());
        assert_eq!(autosave.next_deadline(), None);
    }

    #[test]
    fn edits_during_a_save_keep_the_document_dirty() {
        let (mut autosave, clock) = policy();
        autosave.edited();
        let request = autosave.focus_lost().expect("save");
        autosave.edited();
        autosave.save_succeeded(request);
        assert!(autosave.is_dirty());
        clock.advance(MS(1000));
        assert!(autosave.poll().is_some());
    }

    #[test]
    fn composition_conflict_and_viewer_suppress() {
        let (mut autosave, clock) = policy();
        autosave.edited();
        autosave.set_composing(true);
        clock.advance(MS(5000));
        assert_eq!(autosave.poll(), None);
        assert_eq!(autosave.focus_lost(), None);
        assert_eq!(autosave.suppressed(), Some(Suppressed::Composing));
        autosave.set_composing(false);
        assert_eq!(autosave.poll(), None, "idle delay restarts");
        clock.advance(MS(1000));
        autosave.set_conflict(true);
        assert_eq!(autosave.poll(), None);
        assert_eq!(autosave.quitting(), QuitDecision::Ask(Suppressed::Conflict));
        autosave.set_conflict(false);
        autosave.set_viewer(true);
        assert_eq!(autosave.tab_switched(), None);
        assert_eq!(autosave.manual(), None);
        autosave.set_viewer(false);
        assert!(autosave.tab_switched().is_some());
    }

    #[test]
    fn failure_backs_off_the_idle_trigger_only() {
        let (mut autosave, clock) = policy();
        autosave.edited();
        clock.advance(MS(1000));
        let request = autosave.poll().expect("save");
        autosave.save_failed(request);
        clock.advance(MS(4999));
        assert_eq!(autosave.poll(), None);
        let retry = autosave.focus_lost().expect("focus loss still saves");
        autosave.save_failed(retry);
        clock.advance(MS(5000));
        assert!(autosave.poll().is_some());
    }

    #[test]
    fn quitting() {
        let (mut autosave, _clock) = policy();
        assert_eq!(autosave.quitting(), QuitDecision::Clean);
        autosave.edited();
        let QuitDecision::Save(request) = autosave.quitting() else {
            panic!("expected a save");
        };
        assert_eq!(request.reason, SaveReason::Quit);
        assert_eq!(autosave.quitting(), QuitDecision::WaitForSave);
        autosave.save_succeeded(request);
        assert_eq!(autosave.quitting(), QuitDecision::Clean);

        let mut off = Autosave::new(
            AutosaveSettings {
                enabled: false,
                ..AutosaveSettings::default()
            },
            ManualClock::new(),
        );
        off.edited();
        assert_eq!(off.poll(), None);
        assert_eq!(off.quitting(), QuitDecision::Ask(Suppressed::Disabled));
        assert!(
            off.manual().is_some(),
            "manual saves work with autosave off"
        );
    }

    #[test]
    fn settings_come_from_the_files_section() {
        let files = gv_settings::Files {
            autosave: false,
            ..gv_settings::Files::default()
        };
        assert!(!AutosaveSettings::from_settings(&files).enabled);
    }
}
