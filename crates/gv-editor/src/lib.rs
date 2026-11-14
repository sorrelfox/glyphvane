// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Buffer model, selections, transactions, undo, Markdown commands and the display map.
//!
//! The crate is GUI-free: the editor view in `gv-ui` reads its snapshots
//! and injects what needs a font system (see
//! [`display_map::TextMeasure`]).
//!
//! So far it holds the first [`display_map`] layers: the replace layer
//! (an identity layer for now) and the tab layer, ported from Zed.

pub mod display_map;
