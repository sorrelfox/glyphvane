// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Our own decode caps (ADR-019, plan 12 § Resource limits). They are
//! checked from the header before anything is allocated; `image::Limits`
//! is set as well, as a second line.

/// Caps on what is read, decoded and kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest file read, in bytes.
    pub max_file_bytes: u64,
    /// Largest image or animation canvas, in pixels (100 MP).
    pub max_pixels: u64,
    /// Largest decoded still image, in bytes at its native colour type
    /// (512 MB).
    pub max_decoded_bytes: u64,
    /// Longest side of an image, in pixels.
    pub max_side: u32,
    /// Memory held by the frames of one animation after resizing (200 MB).
    /// Decoding stops at the cap and the frames so far are kept.
    pub max_frame_bytes: u64,
    /// Most frames kept from one animation.
    pub max_frames: usize,
    /// Most canvas bytes decoded for one animation before resizing (bounds
    /// the time a long animation takes).
    pub max_animation_work: u64,
    /// Largest SVG source, in bytes (10 MB).
    pub max_svg_bytes: u64,
    /// Most XML nodes in an SVG.
    pub max_svg_nodes: u32,
    /// Deepest nesting of SVG images inside SVG images.
    pub max_svg_nesting: u8,
    /// Approximate bytes of decoded images kept in the cache.
    pub cache_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 256 << 20,
            max_pixels: 100_000_000,
            max_decoded_bytes: 512 << 20,
            max_side: 65_535,
            max_frame_bytes: 200 << 20,
            max_frames: 10_000,
            max_animation_work: 4 << 30,
            max_svg_bytes: 10 << 20,
            max_svg_nodes: 1_000_000,
            max_svg_nesting: 3,
            cache_bytes: 256 << 20,
        }
    }
}

impl Limits {
    /// Checks a canvas of `width × height` pixels decoding to `bytes`
    /// bytes against the caps.
    pub(crate) fn check_canvas(
        &self,
        width: u32,
        height: u32,
        bytes: u64,
    ) -> Result<(), crate::MediaError> {
        use crate::MediaError::TooLarge;
        if width == 0 || height == 0 {
            return Err(crate::MediaError::Malformed("empty image".into()));
        }
        if width.max(height) > self.max_side {
            return Err(TooLarge {
                what: "side length (px)",
                actual: u64::from(width.max(height)),
                max: u64::from(self.max_side),
            });
        }
        let pixels = u64::from(width) * u64::from(height);
        if pixels > self.max_pixels {
            return Err(TooLarge {
                what: "pixels",
                actual: pixels,
                max: self.max_pixels,
            });
        }
        if bytes > self.max_decoded_bytes {
            return Err(TooLarge {
                what: "decoded bytes",
                actual: bytes,
                max: self.max_decoded_bytes,
            });
        }
        Ok(())
    }

    /// The matching `image::Limits`.
    pub(crate) fn image_limits(&self) -> image::Limits {
        let mut l = image::Limits::default();
        l.max_image_width = Some(self.max_side);
        l.max_image_height = Some(self.max_side);
        l.max_alloc = Some(self.max_decoded_bytes);
        l
    }
}
