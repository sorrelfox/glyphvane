// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Requests and results.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::format::ImageFormat;

/// What to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DecodeRequest {
    /// The box, in device pixels, the image is shown in. The result is
    /// downscaled to fit it (aspect ratio kept, never upscaled). `None`
    /// keeps the native size (the image viewer at 100 %).
    pub fit: Option<(u32, u32)>,
    /// Decode every frame of an animation. When `false` (reduced motion, or
    /// a thumbnail), only the first frame.
    pub animate: bool,
}

impl Default for DecodeRequest {
    fn default() -> Self {
        Self {
            fit: None,
            animate: true,
        }
    }
}

/// Header facts, read without decoding pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub format: ImageFormat,
    /// Size as displayed (after EXIF orientation), in pixels. For SVG, the
    /// intrinsic size in CSS pixels.
    pub width: u32,
    pub height: u32,
}

/// One frame: straight (not premultiplied) RGBA8, sRGB, rows top to
/// bottom without padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// How long the frame shows, in milliseconds (0 for stills).
    pub delay_ms: u32,
}

impl Frame {
    /// Swaps red and blue in place, for hosts that upload BGRA (GPUI's
    /// `RenderImage`). Do it on the background thread.
    pub fn rgba_to_bgra(&mut self) {
        for px in self.rgba.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
    }
}

/// A decoded image or animation, oriented, in sRGB and fitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub format: ImageFormat,
    /// Size of the source as displayed (after orientation), before fitting.
    pub source_width: u32,
    pub source_height: u32,
    /// One frame for stills; every kept frame for animations.
    pub frames: Vec<Frame>,
    /// An animation was cut short by the frame caps.
    pub truncated: bool,
}

impl DecodedImage {
    /// Whether there is more than one frame.
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    /// Memory held by the pixels.
    pub fn bytes(&self) -> usize {
        self.frames.iter().map(|f| f.rgba.len()).sum()
    }
}

/// A shared cancellation flag, checked between decoding stages and frames.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the decode to stop; it ends with
    /// [`MediaError::Cancelled`](crate::MediaError::Cancelled).
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub(crate) fn check(&self) -> Result<(), crate::MediaError> {
        if self.is_cancelled() {
            Err(crate::MediaError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// The size that fits `w × h` into `fit`, keeping the aspect ratio and never
/// upscaling. At least 1 × 1.
pub(crate) fn fitted(w: u32, h: u32, fit: Option<(u32, u32)>) -> (u32, u32) {
    let Some((fw, fh)) = fit else {
        return (w, h);
    };
    if w <= fw && h <= fh {
        return (w, h);
    }
    let scale = (f64::from(fw) / f64::from(w)).min(f64::from(fh) / f64::from(h));
    let tw = (f64::from(w) * scale).round().max(1.0);
    let th = (f64::from(h) * scale).round().max(1.0);
    // In range: both are at most the source size.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (tw as u32, th as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_and_never_upscales() {
        assert_eq!(fitted(4000, 3000, Some((800, 800))), (800, 600));
        assert_eq!(fitted(300, 3000, Some((800, 800))), (80, 800));
        assert_eq!(fitted(100, 50, Some((800, 800))), (100, 50));
        assert_eq!(fitted(100, 50, None), (100, 50));
        assert_eq!(fitted(10_000, 1, Some((10, 10))), (10, 1));
    }
}
