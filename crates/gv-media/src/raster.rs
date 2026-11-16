// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Raster images through `image`: probe, caps, decode, orientation, colour,
//! fit; animation frames under our frame budget.

use std::io::Cursor;

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::metadata::Orientation;
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageReader};

use crate::format::ImageFormat;
use crate::limits::Limits;
use crate::types::{CancelFlag, DecodeRequest, DecodedImage, Frame, Probe, fitted};
use crate::{MediaError, color, resize};

fn swaps_sides(o: Orientation) -> bool {
    matches!(
        o,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

fn decoder<'a>(
    bytes: &'a [u8],
    format: ImageFormat,
    limits: &Limits,
) -> Result<impl ImageDecoder + 'a, MediaError> {
    let fmt = format
        .to_image()
        .filter(|_| format.is_supported())
        .ok_or(MediaError::Unsupported(format))?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), fmt);
    reader.limits(limits.image_limits());
    Ok(reader.into_decoder()?)
}

/// Header facts for a raster image.
pub(crate) fn probe(
    bytes: &[u8],
    format: ImageFormat,
    limits: &Limits,
) -> Result<Probe, MediaError> {
    let mut dec = decoder(bytes, format, limits)?;
    let (w, h) = dec.dimensions();
    let o = dec.orientation().unwrap_or(Orientation::NoTransforms);
    let (width, height) = if swaps_sides(o) { (h, w) } else { (w, h) };
    Ok(Probe {
        format,
        width,
        height,
    })
}

/// Decodes a raster image (all frames when animated and asked for).
pub(crate) fn decode(
    bytes: &[u8],
    format: ImageFormat,
    req: DecodeRequest,
    limits: &Limits,
    cancel: &CancelFlag,
) -> Result<DecodedImage, MediaError> {
    if req.animate
        && let Some(anim) = decode_animation(bytes, format, req, limits, cancel)?
    {
        return Ok(anim);
    }
    decode_still(bytes, format, req, limits, cancel)
}

fn decode_still(
    bytes: &[u8],
    format: ImageFormat,
    req: DecodeRequest,
    limits: &Limits,
    cancel: &CancelFlag,
) -> Result<DecodedImage, MediaError> {
    let mut dec = decoder(bytes, format, limits)?;
    let (w, h) = dec.dimensions();
    // Our caps, from the header, before any pixel buffer exists.
    limits.check_canvas(w, h, dec.total_bytes())?;
    let orientation = dec.orientation().unwrap_or(Orientation::NoTransforms);
    let icc = dec.icc_profile().ok().flatten();
    cancel.check()?;
    let mut img = DynamicImage::from_decoder(dec)?;
    cancel.check()?;
    img.apply_orientation(orientation);
    let (sw, sh) = (img.width(), img.height());
    let mut rgba = img.into_rgba8().into_raw();
    if let Some(icc) = icc {
        color::to_srgb(&mut rgba, &icc);
    }
    cancel.check()?;
    let (tw, th) = fitted(sw, sh, req.fit);
    let rgba = resize::resize_rgba(rgba, sw, sh, tw, th)?;
    Ok(DecodedImage {
        format,
        source_width: sw,
        source_height: sh,
        frames: vec![Frame {
            width: tw,
            height: th,
            rgba,
            delay_ms: 0,
        }],
        truncated: false,
    })
}

/// Decodes every frame of an animated GIF, APNG or WebP. `None` when the
/// image is not animated (or has a single frame): decode it as a still.
fn decode_animation(
    bytes: &[u8],
    format: ImageFormat,
    req: DecodeRequest,
    limits: &Limits,
    cancel: &CancelFlag,
) -> Result<Option<DecodedImage>, MediaError> {
    let image_limits = limits.image_limits();
    let (w, h, frames): (u32, u32, image::Frames<'_>) = match format {
        ImageFormat::Gif => {
            let mut d = GifDecoder::new(Cursor::new(bytes))?;
            d.set_limits(image_limits)?;
            let (w, h) = d.dimensions();
            limits.check_canvas(w, h, u64::from(w) * u64::from(h) * 4)?;
            (w, h, d.into_frames())
        }
        ImageFormat::Png => {
            let mut d = PngDecoder::new(Cursor::new(bytes))?;
            if !d.is_apng()? {
                return Ok(None);
            }
            d.set_limits(image_limits)?;
            let (w, h) = d.dimensions();
            limits.check_canvas(w, h, u64::from(w) * u64::from(h) * 4)?;
            (w, h, d.apng()?.into_frames())
        }
        ImageFormat::WebP => {
            let mut d = WebPDecoder::new(Cursor::new(bytes))?;
            if !d.has_animation() {
                return Ok(None);
            }
            d.set_limits(image_limits)?;
            let (w, h) = d.dimensions();
            limits.check_canvas(w, h, u64::from(w) * u64::from(h) * 4)?;
            (w, h, d.into_frames())
        }
        _ => return Ok(None),
    };
    let (tw, th) = fitted(w, h, req.fit);
    let canvas_bytes = u64::from(w) * u64::from(h) * 4;
    let mut out: Vec<Frame> = Vec::new();
    let mut held: u64 = 0;
    let mut work: u64 = 0;
    let mut truncated = false;
    for frame in frames {
        cancel.check()?;
        let frame = match frame {
            Ok(f) => f,
            // A broken frame after good ones ends the animation there.
            Err(e) if !out.is_empty() => {
                log::debug!("gv-media: animation cut at frame {}: {e}", out.len());
                truncated = true;
                break;
            }
            Err(e) => return Err(e.into()),
        };
        work += canvas_bytes;
        if out.len() >= limits.max_frames || work > limits.max_animation_work {
            truncated = true;
            break;
        }
        let (num, den) = frame.delay().numer_denom_ms();
        let delay_ms = num.checked_div(den).unwrap_or(0);
        let buf = frame.into_buffer();
        let (fw, fh) = buf.dimensions();
        let rgba = resize::resize_rgba(buf.into_raw(), fw, fh, tw, th)?;
        held += rgba.len() as u64;
        if held > limits.max_frame_bytes {
            truncated = true;
            break;
        }
        out.push(Frame {
            width: tw,
            height: th,
            rgba,
            delay_ms,
        });
    }
    match out.len() {
        0 => Err(MediaError::Malformed("animation without frames".into())),
        1 if !truncated => Ok(None),
        _ => Ok(Some(DecodedImage {
            format,
            source_width: w,
            source_height: h,
            frames: out,
            truncated,
        })),
    }
}
