// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Downscaling to display size with fast_image_resize (RGBA8 only).
//!
//! Catmull-Rom convolution: sharp enough for text in screenshots, without
//! Lanczos ringing; alpha is premultiplied for the filter and restored
//! afterwards, so transparent edges do not pick up dark fringes.

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use crate::MediaError;

/// `rgba` (`w × h`, straight RGBA8) resized to `tw × th`. Returns the input
/// unchanged when the size already matches.
pub(crate) fn resize_rgba(
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    tw: u32,
    th: u32,
) -> Result<Vec<u8>, MediaError> {
    if (w, h) == (tw, th) {
        return Ok(rgba);
    }
    let src = Image::from_vec_u8(w, h, rgba, PixelType::U8x4)
        .map_err(|e| MediaError::Malformed(format!("resize: {e}")))?;
    let mut dst = Image::new(tw, th, PixelType::U8x4);
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::CatmullRom));
    Resizer::new()
        .resize(&src, &mut dst, &options)
        .map_err(|e| MediaError::Malformed(format!("resize: {e}")))?;
    Ok(dst.into_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_a_checkerboard_to_grey() {
        let (w, h) = (64u32, 64u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                px.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let out = resize_rgba(px, w, h, 16, 16).expect("resize");
        assert_eq!(out.len(), 16 * 16 * 4);
        let centre = &out[(8 * 16 + 8) * 4..(8 * 16 + 8) * 4 + 4];
        assert!(centre[0].abs_diff(128) < 12, "{centre:?}");
        assert_eq!(centre[3], 255);
    }

    #[test]
    fn transparent_edges_keep_their_colour() {
        // Opaque red next to fully transparent black: the downscaled edge
        // must stay red (premultiplied filtering), not turn dark.
        let (w, h) = (8u32, 2u32);
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                if x < 4 {
                    px.extend_from_slice(&[255, 0, 0, 255]);
                } else {
                    px.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        let out = resize_rgba(px, w, h, 4, 1).expect("resize");
        let edge = &out[4..8];
        assert!(edge[3] > 0);
        assert!(edge[0] > 200, "edge colour darkened: {edge:?}");
    }
}
