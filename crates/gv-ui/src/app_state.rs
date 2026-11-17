// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The application state file ([`gv_settings::AppState`]) as a GPUI
//! global: read once at start-up, written after each change. Without a
//! data folder (no home folder, tests) it lives in memory only. Safe mode
//! reads it but writes nothing.

use std::path::PathBuf;

use gpui::{App, Global};
use gv_settings::AppState;

use crate::settings::AppSettings;

/// The state and where it is saved.
#[derive(Debug, Default)]
pub struct StateStore {
    path: Option<PathBuf>,
    state: AppState,
}

impl Global for StateStore {}

impl StateStore {
    /// Reads the state at `path` (`None`: in memory only).
    pub fn load(path: Option<PathBuf>) -> Self {
        let state = path.as_deref().map(AppState::load).unwrap_or_default();
        Self { path, state }
    }
}

/// Installs the store read from the app's data folder. Called once at
/// start-up, after [`crate::init`]; later calls keep the first store.
pub fn install(cx: &mut App) {
    if cx.has_global::<StateStore>() {
        return;
    }
    let path = cx
        .try_global::<AppSettings>()
        .and_then(|s| s.locations())
        .map(|l| l.app_state_file());
    cx.set_global(StateStore::load(path));
}

/// Uses `store` (tests).
pub fn set_store(cx: &mut App, store: StateStore) {
    cx.set_global(store);
}

/// The current state (the default before [`install`]).
pub fn get(cx: &App) -> AppState {
    cx.try_global::<StateStore>()
        .map(|s| s.state.clone())
        .unwrap_or_default()
}

/// Changes the state and saves it (not in safe mode).
pub fn update(cx: &mut App, change: impl FnOnce(&mut AppState)) {
    let safe_mode = cx
        .try_global::<AppSettings>()
        .is_some_and(AppSettings::safe_mode);
    if !cx.has_global::<StateStore>() {
        cx.set_global(StateStore::default());
    }
    let store = cx.global_mut::<StateStore>();
    change(&mut store.state);
    if safe_mode {
        return;
    }
    if let Some(path) = &store.path
        && let Err(err) = store.state.save(path)
    {
        tracing::warn!("state: cannot save {}: {err}", path.display());
    }
}
