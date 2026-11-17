// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `data:` image URLs (RFC 2397): the bytes of an allowed raster image,
//! with a cap on their size.
//!
//! Only `image/png`, `image/gif`, `image/jpeg` (`image/jpg`),
//! `image/webp` and `image/avif` are taken; everything else (SVG
//! included) is refused. The payload is base64 (whitespace ignored) or
//! percent-encoded. The cap applies to the decoded bytes and, before any
//! decoding, to what they can at most be.

use crate::error::MediaError;

/// The largest `data:` image taken, decoded (12 § Remote content: 10 MB).
pub const MAX_DATA_IMAGE_BYTES: u64 = 10 << 20;

/// The image types a `data:` URL may carry.
const IMAGE_TYPES: &[&str] = &[
    "image/png",
    "image/gif",
    "image/jpeg",
    "image/jpg",
    "image/webp",
    "image/avif",
];

/// Whether `url` is a `data:` URL (any type).
pub fn is_data_url(url: &str) -> bool {
    url.trim_start()
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("data:"))
}

/// The bytes of `data:` image URL `url`, at most `max` of them.
pub fn decode_data_url(url: &str, max: u64) -> Result<Vec<u8>, MediaError> {
    let url = url.trim();
    if !is_data_url(url) {
        return Err(MediaError::Malformed("not a data: URL".into()));
    }
    let rest = &url[5..];
    let (head, payload) = rest
        .split_once(',')
        .ok_or_else(|| MediaError::Malformed("data: URL without a comma".into()))?;
    let mut params = head.split(';').map(str::trim);
    let mime = params.next().unwrap_or("").to_ascii_lowercase();
    if !IMAGE_TYPES.contains(&mime.as_str()) {
        return Err(MediaError::Malformed(if mime.is_empty() {
            "data: URL without an image type".into()
        } else {
            format!("data: URL of type {mime}, not an image")
        }));
    }
    let base64 = params.any(|p| p.eq_ignore_ascii_case("base64"));
    let too_large = |actual: u64| MediaError::TooLarge {
        what: "bytes",
        actual,
        max,
    };
    // An upper bound of the decoded size, before decoding anything.
    let bound = if base64 {
        (payload.len() as u64).div_ceil(4) * 3
    } else {
        payload.len() as u64
    };
    if bound > max {
        // The exact size when it is cheap to know (base64 without
        // whitespace); else the bound.
        return Err(too_large(bound));
    }
    let bytes = if base64 {
        decode_base64(payload)?
    } else {
        percent_decode(payload)
    };
    if bytes.len() as u64 > max {
        return Err(too_large(bytes.len() as u64));
    }
    if bytes.is_empty() {
        return Err(MediaError::Malformed("empty data: URL".into()));
    }
    Ok(bytes)
}

/// Standard or URL-safe base64; whitespace and percent-encoded padding
/// are skipped, padding is optional.
fn decode_base64(s: &str) -> Result<Vec<u8>, MediaError> {
    fn value(b: u8) -> Option<u32> {
        match b {
            b'A'..=b'Z' => Some(u32::from(b - b'A')),
            b'a'..=b'z' => Some(u32::from(b - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(b - b'0') + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let s = s.replace("%3D", "=").replace("%3d", "=");
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut ended = false;
    for b in s.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            ended = true;
            continue;
        }
        let v = value(b)
            .filter(|_| !ended)
            .ok_or_else(|| MediaError::Malformed("invalid base64 in a data: URL".into()))?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // Six leftover bits cannot come from whole bytes.
    if bits >= 6 {
        return Err(MediaError::Malformed(
            "truncated base64 in a data: URL".into(),
        ));
    }
    Ok(out)
}

/// `%XX` escapes decoded; anything else taken as it is.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(h) = b.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(h).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_payloads_decode() {
        assert_eq!(
            decode_data_url("data:image/png;base64,aGVsbG8=", 100),
            Ok(b"hello".to_vec())
        );
        // Whitespace, missing padding, URL-safe letters, upper-case type.
        assert_eq!(
            decode_data_url(" DATA:Image/PNG;base64,aGVs\n bG8 ", 100),
            Ok(b"hello".to_vec())
        );
        assert_eq!(
            decode_data_url("data:image/gif;base64,-_8", 100),
            Ok(vec![0xfb, 0xff])
        );
        assert!(decode_data_url("data:image/png;base64,a$b=", 100).is_err());
        assert!(decode_data_url("data:image/png;base64,aGVsbG8=x", 100).is_err());
        assert!(decode_data_url("data:image/png;base64,a", 100).is_err());
    }

    #[test]
    fn percent_payloads_decode() {
        assert_eq!(
            decode_data_url("data:image/png,%89PNG%zz", 100),
            Ok(b"\x89PNG%zz".to_vec())
        );
    }

    #[test]
    fn only_raster_image_types_are_taken() {
        for bad in [
            "data:image/svg+xml;base64,PHN2Zz4=",
            "data:text/html,<b>",
            "data:,x",
            "data:image/png;base64",
            "https://x.test/a.png",
        ] {
            assert!(
                matches!(decode_data_url(bad, 100), Err(MediaError::Malformed(_))),
                "{bad}"
            );
        }
        assert!(is_data_url("data:x"));
        assert!(!is_data_url("dat"));
    }

    #[test]
    fn the_cap_holds_before_decoding() {
        let big = format!("data:image/png;base64,{}", "A".repeat(4000));
        assert!(matches!(
            decode_data_url(&big, 1000),
            Err(MediaError::TooLarge {
                actual: 3000,
                max: 1000,
                ..
            })
        ));
        let small = format!("data:image/png;base64,{}", "A".repeat(4000));
        assert_eq!(decode_data_url(&small, 3000).map(|b| b.len()), Ok(3000));
    }
}
