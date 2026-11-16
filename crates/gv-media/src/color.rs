// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! ICC to sRGB with moxcms, in place on RGBA8.

use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};

/// Converts `rgba` from the colour space of `icc` to sRGB. RGB profiles
/// only: grey and CMYK profiles are left alone (CMYK JPEG is shown with
/// naive colours in 1.0, ADR-019). A broken profile is ignored.
pub(crate) fn to_srgb(rgba: &mut Vec<u8>, icc: &[u8]) {
    let Ok(profile) = ColorProfile::new_from_slice(icc) else {
        log::debug!("gv-media: unreadable ICC profile ignored");
        return;
    };
    if profile.color_space != DataColorSpace::Rgb {
        return;
    }
    let srgb = ColorProfile::new_srgb();
    let Ok(transform) = profile.create_transform_8bit(
        Layout::Rgba,
        &srgb,
        Layout::Rgba,
        TransformOptions::default(),
    ) else {
        log::debug!("gv-media: ICC profile without a usable transform ignored");
        return;
    };
    let mut out = vec![0u8; rgba.len()];
    if transform.transform(rgba, &mut out).is_ok() {
        *rgba = out;
    }
}
