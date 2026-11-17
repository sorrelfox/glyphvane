// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Integration with the operating system beyond files and windows: recent
//! documents and jump lists (FR-PLAT-05, [`recent`]), the macOS command
//! that installs the `glyphvane` command ([`install_cli`]) and the offer
//! to become the default app for Markdown files ([`default_app`]).

pub mod default_app;
pub mod install_cli;
pub mod recent;

use gpui::App;

/// Installs the command handlers. Called by [`crate::init`].
pub(crate) fn init(cx: &mut App) {
    install_cli::init(cx);
}
