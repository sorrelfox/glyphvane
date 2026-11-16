// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Why an image is not shown.

use crate::format::ImageFormat;

/// Why loading failed. Every variant maps to an image card in the Reading
/// view ([`MediaError::card_message`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaError {
    /// Over one of our caps: shown as a card, never decoded.
    TooLarge {
        what: &'static str,
        actual: u64,
        max: u64,
    },
    /// A format we do not decode here (AVIF and HEIC go through the helper
    /// or the OS codec; JPEG XL; TIFF without the feature).
    Unsupported(ImageFormat),
    /// Not an image, or a broken one.
    Malformed(String),
    /// The file could not be read.
    Io(String),
    /// Cancelled through a [`CancelFlag`](crate::CancelFlag).
    Cancelled,
    /// A decoder panicked; the panic was caught.
    Panicked,
}

impl MediaError {
    /// One line for the image card (English-only UI in 1.0).
    pub fn card_message(&self) -> String {
        match self {
            MediaError::TooLarge { what, actual, max } => {
                format!("Image too large to show: {actual} {what} (limit {max}).")
            }
            MediaError::Unsupported(f) => format!("{} images cannot be shown here.", f.name()),
            MediaError::Malformed(m) => format!("The image could not be read: {m}"),
            MediaError::Io(m) => format!("The file could not be opened: {m}"),
            MediaError::Cancelled => "Loading was cancelled.".into(),
            MediaError::Panicked => "The image decoder failed on this file.".into(),
        }
    }

    /// Whether the card should offer *Open externally* (formats another
    /// program can show).
    pub fn offers_open_externally(&self) -> bool {
        matches!(
            self,
            MediaError::Unsupported(_) | MediaError::TooLarge { .. }
        )
    }
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.card_message())
    }
}

impl std::error::Error for MediaError {}

impl From<image::ImageError> for MediaError {
    fn from(e: image::ImageError) -> Self {
        match e {
            image::ImageError::Limits(l) => MediaError::TooLarge {
                what: match l.kind() {
                    image::error::LimitErrorKind::DimensionError => "side length (px)",
                    _ => "decoded bytes",
                },
                actual: 0,
                max: 0,
            },
            image::ImageError::IoError(io) => MediaError::Malformed(io.to_string()),
            other => MediaError::Malformed(other.to_string()),
        }
    }
}
