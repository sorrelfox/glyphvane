// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Image and SVG probing, decoding, orientation, colour and resizing, with
//! our own caps and a cache (ADR-019).
//!
//! - Raster: PNG/APNG, JPEG, GIF, WebP (still and animated), BMP and ICO
//!   through `image` (pure Rust); TIFF behind the `tiff` feature. EXIF
//!   orientation applied, ICC profiles converted to sRGB with moxcms.
//! - SVG: usvg/resvg; no network, external files only inside an allowed
//!   root, embedded images checked against our caps.
//! - AVIF, HEIC and JPEG XL are recognised and refused with
//!   [`MediaError::Unsupported`] (the helper process and OS codecs come in
//!   M8; the Reading view shows a card).
//! - Our caps are checked from the header before any pixel buffer exists;
//!   animations keep frames under a memory budget.
//! - Remote images (the `remote` feature): [`remote::RemoteFetcher`]
//!   downloads into a disk cache with caps; the caller applies the
//!   remote-content policy.
//! - Results are fitted to the display box with fast_image_resize and
//!   delivered as straight RGBA8 frames.
//!
//! ```no_run
//! # use std::{path::Path, sync::Arc};
//! # use gv_media::*;
//! let engine = Arc::new(MediaEngine::new(Limits::default(), SvgContext::default()));
//! let req = DecodeRequest { fit: Some((800, 600)), animate: true };
//! // UI thread:
//! let key = MediaKey::for_path(Path::new("a.png"), req)?;
//! if engine.cached(&key).is_none() {
//!     let cancel = CancelFlag::new();
//!     let fut = Arc::clone(&engine).load_path_future("a.png".into(), req, cancel.clone());
//!     // cx.background_spawn(fut) ... cancel.cancel() when the image leaves.
//! #   let _ = fut;
//! }
//! # Ok::<(), MediaError>(())
//! ```

mod color;
mod engine;
mod error;
mod format;
mod limits;
mod raster;
#[cfg(feature = "remote")]
pub mod remote;
mod resize;
mod svg;
mod types;

pub use engine::{CacheStats, LoadResult, MediaEngine, MediaKey};
pub use error::MediaError;
pub use format::ImageFormat;
pub use limits::Limits;
pub use svg::SvgContext;
pub use types::{CancelFlag, DecodeRequest, DecodedImage, Frame, Probe};
