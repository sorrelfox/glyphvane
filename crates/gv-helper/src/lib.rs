// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Helper-process entry points and IPC protocol for risky or non-cancellable engines.
//!
//! `glyphvane-helper` is a separate executable (no GUI framework) that
//! runs the engines that may hang, crash or use too much memory: Typst
//! PDF export, and PDF page raster and the text of PDF pages (hayro). The
//! app talks to it through [`client::Helper`], which starts it on demand,
//! enforces a deadline per job, kills it on timeout or cancellation, and
//! starts a new one for the next job.
//!
//! - [`protocol`]: the frames both sides exchange.
//! - [`client`]: the app's side (no engine; always built).
//! - `engine`, `raster`, `text` and `serve`: the helper's side (the
//!   `engine` feature).

pub mod client;
pub mod protocol;

#[cfg(feature = "engine")]
pub mod engine;
#[cfg(feature = "engine")]
pub mod fonts;
#[cfg(feature = "engine")]
pub mod raster;
#[cfg(feature = "engine")]
pub mod serve;
#[cfg(feature = "engine")]
pub mod text;

pub use client::{Helper, HelperConfig, HelperError, locate};
pub use protocol::{
    DocInfo, OpenJob, PageImage, PageJob, PageSize, PageText, PdfJob, PdfOutput, RenderJob,
    TextBox, TextJob,
};
