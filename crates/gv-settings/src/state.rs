// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Small application state that is not a setting: whether first-run
//! onboarding was done, the update check's bookkeeping, and which local
//! crash reports the user has seen. Kept as JSON in the data dir
//! ([`crate::Locations::app_state_file`]).
//!
//! A missing or unreadable file reads as the default state (onboarding not
//! done, nothing checked yet). Unknown fields are ignored, so an older
//! build reads a newer build's file.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The state file's format version.
pub const STATE_VERSION: u32 = 1;

/// The application state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    pub version: u32,
    /// First-run onboarding was finished or closed.
    pub onboarding_done: bool,
    /// The update check (FR-PLAT-07).
    pub updates: UpdateState,
    /// The newest crash report the user has been told about, by file name.
    pub crash_report_seen: Option<String>,
}

/// The update check's bookkeeping. Times are Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateState {
    /// When the last request was made, successful or not.
    pub last_attempt: Option<u64>,
    /// When the last request got an answer.
    pub last_success: Option<u64>,
    /// Failed requests since the last answer, for the backoff.
    pub failures: u32,
    /// The `ETag` of the last answer, for a conditional request.
    pub etag: Option<String>,
    /// The newest release the last answer named.
    pub latest: Option<ReleaseRecord>,
    /// A version the user chose to skip: no notice for it.
    pub skipped_version: Option<String>,
}

/// A release as the update check last saw it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReleaseRecord {
    /// The tag, such as `v1.2.0`.
    pub tag: String,
    /// The release page on GitHub.
    pub url: String,
    /// The release notes (Markdown), capped.
    pub notes: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            onboarding_done: false,
            updates: UpdateState::default(),
            crash_report_seen: None,
        }
    }
}

impl AppState {
    /// Reads the state at `path`; the default when the file is missing or
    /// unreadable.
    pub fn load(path: &Path) -> Self {
        match std::fs::read(path) {
            // An unreadable file is replaced by the default on the next save.
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| Self::new()),
            Err(_) => Self::new(),
        }
    }

    /// A fresh state of the current version.
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes the state to `path` atomically, creating its folder.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut state = self.clone();
        state.version = STATE_VERSION;
        let bytes = serde_json::to_vec_pretty(&state).map_err(io::Error::other)?;
        crate::file::atomic_write(path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_defaults() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sub").join("state.json");
        assert_eq!(AppState::load(&path), AppState::new());
        let mut state = AppState::new();
        state.onboarding_done = true;
        state.updates.failures = 2;
        state.updates.latest = Some(ReleaseRecord {
            tag: "v1.0.0".into(),
            url: "https://github.com/sorrelfox/glyphvane/releases/tag/v1.0.0".into(),
            notes: "Notes".into(),
        });
        state.crash_report_seen = Some("crash-1.txt".into());
        state.save(&path).expect("saved");
        assert_eq!(AppState::load(&path), state);
    }

    #[test]
    fn unreadable_files_and_unknown_fields() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.json");
        std::fs::write(&path, b"{not json").expect("write");
        assert_eq!(AppState::load(&path), AppState::new());
        std::fs::write(&path, br#"{"version":7,"onboarding_done":true,"future":1}"#)
            .expect("write");
        assert!(AppState::load(&path).onboarding_done);
    }
}
