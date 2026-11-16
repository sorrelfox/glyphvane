// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Why a formula did not render.

use std::fmt;
use std::ops::Range;

/// Why a formula did not render. The Reading view shows the source with
/// this message in the error style.
#[derive(Debug, Clone, PartialEq)]
pub enum MathError {
    /// RaTeX rejected the source (an undefined command, a missing brace, a
    /// recursion or expansion limit). `span` is a byte range in the source
    /// when RaTeX knows where.
    Parse {
        message: String,
        span: Option<Range<usize>>,
    },
    /// The source is longer than the cap (`chemistry`: the tighter cap for
    /// `\ce` and `\pu`, whose time grows quadratically).
    TooLong {
        len: usize,
        max: usize,
        chemistry: bool,
    },
    /// The layout has more drawing items than the cap.
    TooComplex { items: usize, max: usize },
    /// The formula is wider or taller than the cap (for example
    /// `\rule{10000em}{1em}`), in em.
    TooLarge { width: f32, height: f32, max: f32 },
    /// The engine failed unexpectedly (a panic, caught).
    Internal,
}

impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse { message, .. } => f.write_str(message),
            Self::TooLong {
                len,
                max,
                chemistry,
            } => {
                let what = if *chemistry {
                    "Chemistry formula"
                } else {
                    "Formula"
                };
                write!(
                    f,
                    "{what} too long to render ({len} bytes; the limit is {max})"
                )
            }
            Self::TooComplex { items, max } => write!(
                f,
                "Formula too complex to render ({items} drawing items; the limit is {max})"
            ),
            Self::TooLarge { width, height, max } => write!(
                f,
                "Formula too large to render ({width:.0} × {height:.0} em; the limit is {max:.0} em)"
            ),
            Self::Internal => f.write_str("The math engine failed on this formula"),
        }
    }
}

impl std::error::Error for MathError {}
