// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Format detection from the first bytes (never from the extension alone).

/// An image format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
    Bmp,
    Ico,
    Tiff,
    Svg,
    /// Decoded in the helper process (M8), not here.
    Avif,
    /// OS codecs only (M8), not here.
    Heic,
    JpegXl,
}

impl ImageFormat {
    /// A display name.
    pub fn name(self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::Gif => "GIF",
            ImageFormat::WebP => "WebP",
            ImageFormat::Bmp => "BMP",
            ImageFormat::Ico => "ICO",
            ImageFormat::Tiff => "TIFF",
            ImageFormat::Svg => "SVG",
            ImageFormat::Avif => "AVIF",
            ImageFormat::Heic => "HEIC",
            ImageFormat::JpegXl => "JPEG XL",
        }
    }

    /// Whether this build decodes the format.
    pub fn is_supported(self) -> bool {
        match self {
            ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::Gif
            | ImageFormat::WebP
            | ImageFormat::Bmp
            | ImageFormat::Ico
            | ImageFormat::Svg => true,
            ImageFormat::Tiff => cfg!(feature = "tiff"),
            ImageFormat::Avif | ImageFormat::Heic | ImageFormat::JpegXl => false,
        }
    }

    pub(crate) fn to_image(self) -> Option<image::ImageFormat> {
        Some(match self {
            ImageFormat::Png => image::ImageFormat::Png,
            ImageFormat::Jpeg => image::ImageFormat::Jpeg,
            ImageFormat::Gif => image::ImageFormat::Gif,
            ImageFormat::WebP => image::ImageFormat::WebP,
            ImageFormat::Bmp => image::ImageFormat::Bmp,
            ImageFormat::Ico => image::ImageFormat::Ico,
            ImageFormat::Tiff => image::ImageFormat::Tiff,
            _ => return None,
        })
    }

    /// The format of `bytes`, from their signature. `None` for anything
    /// else (including text that is not SVG).
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        let starts = |sig: &[u8]| bytes.starts_with(sig);
        if starts(b"\x89PNG\r\n\x1a\n") {
            return Some(ImageFormat::Png);
        }
        if starts(b"\xFF\xD8\xFF") {
            return Some(ImageFormat::Jpeg);
        }
        if starts(b"GIF87a") || starts(b"GIF89a") {
            return Some(ImageFormat::Gif);
        }
        if bytes.len() >= 12 && starts(b"RIFF") && &bytes[8..12] == b"WEBP" {
            return Some(ImageFormat::WebP);
        }
        if starts(b"BM") && bytes.len() >= 14 {
            return Some(ImageFormat::Bmp);
        }
        if starts(b"\0\0\x01\0") || starts(b"\0\0\x02\0") {
            return Some(ImageFormat::Ico);
        }
        if starts(b"II*\0") || starts(b"MM\0*") {
            return Some(ImageFormat::Tiff);
        }
        if starts(b"\xFF\x0A") || starts(b"\0\0\0\x0CJXL \r\n\x87\n") {
            return Some(ImageFormat::JpegXl);
        }
        if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
            return iso_brand(bytes);
        }
        if starts(b"\x1f\x8b") || looks_like_svg(bytes) {
            // Gzip is only taken as SVGZ; anything else gzipped fails to
            // parse later.
            return Some(ImageFormat::Svg);
        }
        None
    }
}

/// AVIF or HEIC from the ISO-BMFF `ftyp` major and compatible brands.
fn iso_brand(bytes: &[u8]) -> Option<ImageFormat> {
    let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let end = size.clamp(16, bytes.len().min(256));
    let mut brands = vec![&bytes[8..12]];
    let mut i = 16;
    while i + 4 <= end {
        brands.push(&bytes[i..i + 4]);
        i += 4;
    }
    if brands.iter().any(|b| *b == b"avif" || *b == b"avis") {
        Some(ImageFormat::Avif)
    } else if brands.iter().any(|b| {
        matches!(
            *b,
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"mif1" | b"msf1"
        )
    }) {
        Some(ImageFormat::Heic)
    } else {
        None
    }
}

/// An `<svg` root within the first kilobyte, after an optional BOM, XML
/// declaration, comments and doctype.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(4096)];
    let Ok(text) =
        std::str::from_utf8(head).or_else(|e| std::str::from_utf8(&head[..e.valid_up_to()]))
    else {
        return false;
    };
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if !text.starts_with('<') {
        return false;
    }
    text.contains("<svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_signatures() {
        assert_eq!(
            ImageFormat::sniff(b"\x89PNG\r\n\x1a\n...."),
            Some(ImageFormat::Png)
        );
        assert_eq!(ImageFormat::sniff(b"GIF89a.."), Some(ImageFormat::Gif));
        assert_eq!(
            ImageFormat::sniff(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(ImageFormat::WebP)
        );
        assert_eq!(
            ImageFormat::sniff(b"\0\0\0\x1cftypavif\0\0\0\0mif1miaf"),
            Some(ImageFormat::Avif)
        );
        assert_eq!(
            ImageFormat::sniff(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"),
            Some(ImageFormat::Heic)
        );
        assert_eq!(
            ImageFormat::sniff(
                b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>"
            ),
            Some(ImageFormat::Svg)
        );
        assert_eq!(ImageFormat::sniff(b"hello world"), None);
        assert_eq!(ImageFormat::sniff(b"<html><body></body></html>"), None);
    }
}
