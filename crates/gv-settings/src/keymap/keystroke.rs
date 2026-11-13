// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Keystrokes and key sequences: parsing, the canonical text form and labels
//! for the interface.
//!
//! The text form follows GPUI's keystroke syntax so the UI can convert
//! bindings one to one: modifiers joined to the key with `-` (`ctrl-shift-p`),
//! keystrokes of a chord separated by spaces (`cmd-k cmd-t`), and
//! `secondary-` for Cmd on macOS and Ctrl elsewhere. A single upper-case
//! letter means Shift plus that letter, as in GPUI.

use std::fmt;
use std::ops::Range;

/// A desktop platform with its own default bindings and key labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

impl Platform {
    /// Every platform, in the order of the keymap file's sections.
    pub const ALL: [Self; 3] = [Self::MacOs, Self::Linux, Self::Windows];

    /// The platform this build runs on. BSDs count as Linux.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    /// The section name in `keymap.toml`: `macos`, `linux` or `windows`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Linux => "linux",
            Self::Windows => "windows",
        }
    }

    /// The platform with this section name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MacOs => "macOS",
            Self::Linux => "Linux",
            Self::Windows => "Windows",
        })
    }
}

/// The modifier keys held for a keystroke. The fields match GPUI's
/// `Modifiers`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Modifiers {
    /// Control (⌃ on macOS).
    pub control: bool,
    /// Alt, Option on macOS (⌥).
    pub alt: bool,
    /// Shift (⇧ on macOS).
    pub shift: bool,
    /// Command on macOS (⌘), the Windows key, Super on Linux.
    pub platform: bool,
    /// The Fn key.
    pub function: bool,
}

impl Modifiers {
    /// No modifier.
    pub const NONE: Self = Self {
        control: false,
        alt: false,
        shift: false,
        platform: false,
        function: false,
    };

    /// Whether no modifier is held.
    pub fn is_empty(self) -> bool {
        self == Self::NONE
    }

    /// The "secondary" modifier: Cmd on macOS, Ctrl elsewhere.
    pub fn secondary(platform: Platform) -> Self {
        match platform {
            Platform::MacOs => Self {
                platform: true,
                ..Self::NONE
            },
            Platform::Linux | Platform::Windows => Self {
                control: true,
                ..Self::NONE
            },
        }
    }
}

/// Named keys, in their canonical (GPUI) spelling, besides `f1`–`f35` and
/// single characters.
pub const NAMED_KEYS: &[&str] = &[
    "enter",
    "escape",
    "tab",
    "backspace",
    "delete",
    "insert",
    "space",
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "pageup",
    "pagedown",
    "menu",
    "back",
    "forward",
];

/// Accepted other spellings of named keys.
const KEY_ALIASES: &[(&str, &str)] = &[
    ("esc", "escape"),
    ("return", "enter"),
    ("del", "delete"),
    ("ins", "insert"),
    ("pgup", "pageup"),
    ("pgdn", "pagedown"),
];

/// The highest function key GPUI reports.
const MAX_FUNCTION_KEY: u32 = 35;

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .filter(|n| !n.starts_with('0'))
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|n| (1..=MAX_FUNCTION_KEY).contains(&n))
}

/// Why a keystroke or key sequence could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct KeyParseError {
    /// A sentence for the user.
    pub message: String,
    /// The byte range of the offending part in the parsed text.
    pub range: Range<usize>,
}

impl KeyParseError {
    fn new(message: impl Into<String>, range: Range<usize>) -> Self {
        Self {
            message: message.into(),
            range,
        }
    }

    fn shifted(mut self, by: usize) -> Self {
        self.range = self.range.start + by..self.range.end + by;
        self
    }
}

/// One key press with its modifiers. The fields match GPUI's `Keystroke`
/// (without `key_char`), so the UI converts in both directions field by
/// field. Keystrokes from [`Keystroke::parse`] are normalized: the key is a
/// name from [`NAMED_KEYS`], `f1`–`f35` or one character, lower-case for
/// ASCII letters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Keystroke {
    pub modifiers: Modifiers,
    pub key: String,
}

impl Keystroke {
    /// Parses one keystroke such as `ctrl-shift-p`, `secondary-,` or
    /// `alt--`. `secondary` becomes Cmd on macOS and Ctrl elsewhere.
    ///
    /// Modifier names (any case): `ctrl`/`control`, `alt`/`option`,
    /// `shift`, `cmd`/`command`/`super`/`win`, `fn`, `secondary`/`mod`.
    pub fn parse(text: &str, platform: Platform) -> Result<Self, KeyParseError> {
        let all = 0..text.len();
        if text.is_empty() {
            return Err(KeyParseError::new("expected a key", all));
        }
        if text.chars().any(char::is_whitespace) {
            return Err(KeyParseError::new(
                "a keystroke cannot contain spaces; separate the keystrokes of a chord with one space",
                all,
            ));
        }
        // The `-` key: `-` alone, or after the last separator (`ctrl--`).
        let (prefix, key_text, key_start) = if text == "-" {
            ("", "-", 0)
        } else if let Some(prefix) = text.strip_suffix("--") {
            (prefix, "-", text.len() - 1)
        } else {
            match text.rfind('-') {
                Some(i) => (&text[..i], &text[i + 1..], i + 1),
                None => ("", text, 0),
            }
        };
        let mut modifiers = Modifiers::NONE;
        if !prefix.is_empty() || key_start > 0 {
            let mut offset = 0;
            for part in prefix.split('-') {
                let range = offset..offset + part.len();
                offset += part.len() + 1;
                add_modifier(&mut modifiers, part, platform, range)?;
            }
        }
        let key_range = key_start..text.len();
        if key_text.is_empty() {
            return Err(KeyParseError::new(
                "expected a key after the modifiers",
                key_range,
            ));
        }
        let key = normalize_key(key_text, &mut modifiers)
            .map_err(|message| KeyParseError::new(message, key_range))?;
        Ok(Self { modifiers, key })
    }

    /// Whether the keystroke types a character: a character key or Space
    /// without Ctrl, Cmd or Fn. Alt counts as typing on macOS, where Option
    /// produces characters. Such keys must not be bound where the editor
    /// takes text input (IME commits arrive as key presses on Wayland).
    pub fn is_printable(&self, platform: Platform) -> bool {
        let m = self.modifiers;
        let typing_modifiers =
            !m.control && !m.platform && !m.function && (!m.alt || platform == Platform::MacOs);
        typing_modifiers && (self.key == "space" || self.key.chars().count() == 1)
    }

    /// The label for menus, the palette and key hints: `⌘⇧P` on macOS,
    /// `Ctrl+Shift+P` elsewhere.
    pub fn label(&self, platform: Platform) -> String {
        let m = self.modifiers;
        let mut out = String::new();
        if platform == Platform::MacOs {
            // Apple's order: Control, Option, Shift, Command.
            if m.function {
                out.push_str("fn ");
            }
            for (on, symbol) in [
                (m.control, '⌃'),
                (m.alt, '⌥'),
                (m.shift, '⇧'),
                (m.platform, '⌘'),
            ] {
                if on {
                    out.push(symbol);
                }
            }
            out.push_str(&key_label(&self.key, platform));
        } else {
            let platform_name = if platform == Platform::Windows {
                "Win"
            } else {
                "Super"
            };
            for (on, name) in [
                (m.function, "Fn"),
                (m.platform, platform_name),
                (m.control, "Ctrl"),
                (m.alt, "Alt"),
                (m.shift, "Shift"),
            ] {
                if on {
                    out.push_str(name);
                    out.push('+');
                }
            }
            out.push_str(&key_label(&self.key, platform));
        }
        out
    }
}

impl fmt::Display for Keystroke {
    /// The canonical text form, in GPUI's order: `fn-ctrl-alt-cmd-shift-key`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.modifiers;
        for (on, name) in [
            (m.function, "fn-"),
            (m.control, "ctrl-"),
            (m.alt, "alt-"),
            (m.platform, "cmd-"),
            (m.shift, "shift-"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        f.write_str(&self.key)
    }
}

fn add_modifier(
    modifiers: &mut Modifiers,
    name: &str,
    platform: Platform,
    range: Range<usize>,
) -> Result<(), KeyParseError> {
    let lower = name.to_ascii_lowercase();
    let added = match lower.as_str() {
        "ctrl" | "control" => Modifiers {
            control: true,
            ..Modifiers::NONE
        },
        "alt" | "option" => Modifiers {
            alt: true,
            ..Modifiers::NONE
        },
        "shift" => Modifiers {
            shift: true,
            ..Modifiers::NONE
        },
        "cmd" | "command" | "super" | "win" => Modifiers {
            platform: true,
            ..Modifiers::NONE
        },
        "fn" => Modifiers {
            function: true,
            ..Modifiers::NONE
        },
        "secondary" | "mod" => Modifiers::secondary(platform),
        "" => {
            return Err(KeyParseError::new(
                "expected a modifier between the dashes",
                range,
            ));
        }
        _ => {
            return Err(KeyParseError::new(
                format!(
                    "`{name}` is not a modifier; use ctrl, alt, shift, cmd, fn or secondary \
                     (Cmd on macOS, Ctrl elsewhere)"
                ),
                range,
            ));
        }
    };
    let twice = (modifiers.control && added.control)
        || (modifiers.alt && added.alt)
        || (modifiers.shift && added.shift)
        || (modifiers.platform && added.platform)
        || (modifiers.function && added.function);
    if twice {
        return Err(KeyParseError::new(
            format!("the modifier `{name}` is given twice"),
            range,
        ));
    }
    modifiers.control |= added.control;
    modifiers.alt |= added.alt;
    modifiers.shift |= added.shift;
    modifiers.platform |= added.platform;
    modifiers.function |= added.function;
    Ok(())
}

fn normalize_key(text: &str, modifiers: &mut Modifiers) -> Result<String, String> {
    let mut chars = text.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_control() || c.is_whitespace() {
            return Err("this character cannot be bound; use a key name such as `space`".into());
        }
        if c.is_ascii_uppercase() {
            // GPUI reads `A` as `shift-a`.
            modifiers.shift = true;
            return Ok(c.to_ascii_lowercase().to_string());
        }
        return Ok(c.to_string());
    }
    let lower = text.to_ascii_lowercase();
    if let Some((_, canonical)) = KEY_ALIASES.iter().find(|(alias, _)| *alias == lower) {
        return Ok((*canonical).to_owned());
    }
    if NAMED_KEYS.contains(&lower.as_str()) || is_function_key(&lower) {
        return Ok(lower);
    }
    if lower == "shift" || lower == "ctrl" || lower == "alt" || lower == "cmd" {
        return Err(format!("expected a key after `{text}`"));
    }
    Err(format!(
        "`{text}` is not a key; use a single character or a key name such as enter, escape, \
         tab, space, backspace, delete, up, pageup, home or f1"
    ))
}

fn key_label(key: &str, platform: Platform) -> String {
    let mac = platform == Platform::MacOs;
    let named = match key {
        "enter" if mac => "↩",
        "enter" => "Enter",
        "escape" if mac => "⎋",
        "escape" => "Esc",
        "tab" if mac => "⇥",
        "tab" => "Tab",
        "backspace" if mac => "⌫",
        "backspace" => "Backspace",
        "delete" if mac => "⌦",
        "delete" => "Delete",
        "insert" => "Insert",
        "space" => "Space",
        "up" => "↑",
        "down" => "↓",
        "left" => "←",
        "right" => "→",
        "home" if mac => "↖",
        "home" => "Home",
        "end" if mac => "↘",
        "end" => "End",
        "pageup" if mac => "⇞",
        "pageup" => "PgUp",
        "pagedown" if mac => "⇟",
        "pagedown" => "PgDn",
        "menu" => "Menu",
        "back" => "Back",
        "forward" => "Forward",
        "+" if !mac => "Plus",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_owned();
    }
    if is_function_key(key) {
        return key.to_ascii_uppercase();
    }
    key.to_uppercase()
}

/// A key binding's keys: one keystroke, or a chord of several pressed one
/// after the other.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeySequence(Vec<Keystroke>);

impl KeySequence {
    /// The longest chord accepted.
    pub const MAX_LEN: usize = 4;

    /// Parses keystrokes separated by spaces, such as `cmd-k cmd-t`.
    pub fn parse(text: &str, platform: Platform) -> Result<Self, KeyParseError> {
        let mut keystrokes = Vec::new();
        let mut offset = 0;
        for part in text.split(' ') {
            let start = offset;
            offset += part.len() + 1;
            if part.is_empty() {
                continue;
            }
            keystrokes.push(Keystroke::parse(part, platform).map_err(|e| e.shifted(start))?);
        }
        if keystrokes.is_empty() {
            return Err(KeyParseError::new("expected keys", 0..text.len()));
        }
        if keystrokes.len() > Self::MAX_LEN {
            return Err(KeyParseError::new(
                format!("a chord can have at most {} keystrokes", Self::MAX_LEN),
                0..text.len(),
            ));
        }
        Ok(Self(keystrokes))
    }

    /// A sequence of the given keystrokes. `None` when empty.
    pub fn new(keystrokes: Vec<Keystroke>) -> Option<Self> {
        (!keystrokes.is_empty()).then_some(Self(keystrokes))
    }

    /// The keystrokes in order.
    pub fn keystrokes(&self) -> &[Keystroke] {
        &self.0
    }

    /// The number of keystrokes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always false: a sequence has at least one keystroke.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `prefix` is a strict prefix of this sequence.
    pub fn has_strict_prefix(&self, prefix: &[Keystroke]) -> bool {
        prefix.len() < self.0.len() && self.0.starts_with(prefix)
    }

    /// The label for the interface: `⌘K ⌘T` on macOS, `Ctrl+K Ctrl+T`
    /// elsewhere.
    pub fn label(&self, platform: Platform) -> String {
        self.0
            .iter()
            .map(|k| k.label(platform))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// One label per keystroke, for drawing each as a keycap.
    pub fn keycaps(&self, platform: Platform) -> Vec<String> {
        self.0.iter().map(|k| k.label(platform)).collect()
    }
}

impl fmt::Display for KeySequence {
    /// The canonical text form, which [`KeySequence::parse`] reads back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, k) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{k}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(text: &str, platform: Platform) -> Keystroke {
        Keystroke::parse(text, platform).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn secondary_depends_on_the_platform() {
        assert_eq!(ks("secondary-p", Platform::MacOs).to_string(), "cmd-p");
        assert_eq!(ks("secondary-p", Platform::Linux).to_string(), "ctrl-p");
        assert_eq!(ks("mod-p", Platform::Windows).to_string(), "ctrl-p");
    }

    #[test]
    fn dash_key_and_uppercase_letters() {
        assert_eq!(ks("ctrl--", Platform::Linux).key, "-");
        assert_eq!(ks("-", Platform::Linux).key, "-");
        let k = ks("ctrl-A", Platform::Linux);
        assert_eq!(k.to_string(), "ctrl-shift-a");
        assert_eq!(
            ks("Shift-Escape", Platform::Linux).to_string(),
            "shift-escape"
        );
        assert_eq!(ks("ctrl-esc", Platform::Linux).key, "escape");
    }

    #[test]
    fn errors_point_at_the_bad_part() {
        let err = Keystroke::parse("ctrl-hyper-p", Platform::Linux).err();
        assert_eq!(err.map(|e| e.range), Some(5..10));
        let err = KeySequence::parse("ctrl-k ctrl-bogus", Platform::Linux).err();
        assert_eq!(err.map(|e| e.range), Some(12..17));
        assert!(Keystroke::parse("ctrl-", Platform::Linux).is_err());
        assert!(Keystroke::parse("ctrl-ctrl-p", Platform::Linux).is_err());
        assert!(Keystroke::parse("f0", Platform::Linux).is_err());
        assert!(Keystroke::parse("f36", Platform::Linux).is_err());
        assert!(Keystroke::parse("ctrl", Platform::Linux).is_err());
        assert!(KeySequence::parse("   ", Platform::Linux).is_err());
    }
}
