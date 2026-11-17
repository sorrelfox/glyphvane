// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `canvasColor` values: a preset `"1"`–`"6"` or a hex colour.
//!
//! The format leaves the preset shades to the application, so this module
//! only names them; the UI maps each [`Preset`] to a theme token.

/// One of the six preset colours of JSON Canvas 1.0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    /// `"1"`
    Red,
    /// `"2"`
    Orange,
    /// `"3"`
    Yellow,
    /// `"4"`
    Green,
    /// `"5"`
    Cyan,
    /// `"6"`
    Purple,
}

impl Preset {
    /// All presets, in number order.
    pub const ALL: [Self; 6] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Cyan,
        Self::Purple,
    ];

    /// The preset's number, `1`–`6`.
    pub fn number(self) -> u8 {
        match self {
            Self::Red => 1,
            Self::Orange => 2,
            Self::Yellow => 3,
            Self::Green => 4,
            Self::Cyan => 5,
            Self::Purple => 6,
        }
    }
}

/// A parsed `color` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CanvasColor {
    Preset(Preset),
    /// A hex colour (`#rgb`, `#rrggbb`; `#rgba` and `#rrggbbaa` are accepted
    /// too and keep their alpha).
    Rgba {
        r: u8,
        g: u8,
        b: u8,
        a: u8,
    },
}

impl CanvasColor {
    /// Parses a `color` string. Returns `None` for anything that is neither a
    /// preset number nor a hex colour.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            return parse_hex(hex);
        }
        let preset = match s {
            "1" => Preset::Red,
            "2" => Preset::Orange,
            "3" => Preset::Yellow,
            "4" => Preset::Green,
            "5" => Preset::Cyan,
            "6" => Preset::Purple,
            _ => return None,
        };
        Some(Self::Preset(preset))
    }
}

fn parse_hex(hex: &str) -> Option<CanvasColor> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let digit = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok();
    let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let (r, g, b, a) = match hex.len() {
        3 | 4 => {
            let r = digit(0)? * 17;
            let g = digit(1)? * 17;
            let b = digit(2)? * 17;
            let a = if hex.len() == 4 { digit(3)? * 17 } else { 255 };
            (r, g, b, a)
        }
        6 | 8 => {
            let a = if hex.len() == 8 { pair(6)? } else { 255 };
            (pair(0)?, pair(2)?, pair(4)?, a)
        }
        _ => return None,
    };
    Some(CanvasColor::Rgba { r, g, b, a })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets() {
        for p in Preset::ALL {
            assert_eq!(
                CanvasColor::parse(&p.number().to_string()),
                Some(CanvasColor::Preset(p))
            );
        }
        assert_eq!(CanvasColor::parse("0"), None);
        assert_eq!(CanvasColor::parse("7"), None);
        assert_eq!(CanvasColor::parse("red"), None);
    }

    #[test]
    fn hex() {
        assert_eq!(
            CanvasColor::parse("#FF0000"),
            Some(CanvasColor::Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            })
        );
        assert_eq!(
            CanvasColor::parse("#0f8"),
            Some(CanvasColor::Rgba {
                r: 0,
                g: 255,
                b: 136,
                a: 255
            })
        );
        assert_eq!(
            CanvasColor::parse("#11223380"),
            Some(CanvasColor::Rgba {
                r: 0x11,
                g: 0x22,
                b: 0x33,
                a: 0x80
            })
        );
        assert_eq!(CanvasColor::parse("#12345"), None);
        assert_eq!(CanvasColor::parse("#ggg"), None);
        assert_eq!(CanvasColor::parse("#é12"), None);
    }
}
