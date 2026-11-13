// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! File-system naming rules per platform, as **data**.
//!
//! Resolution itself never depends on the OS it runs on (links resolve the
//! same everywhere). The rules here answer the questions around it, and they
//! are plain values so every platform's behavior is testable on any host:
//!
//! - which names a file system treats as the same file
//!   ([`PlatformRules::same_name`]; case twins cannot coexist on Windows or
//!   on a default macOS volume);
//! - which names are not portable ([`PlatformRules::name_issues`]: Windows
//!   reserved names, forbidden characters, trailing dots);
//! - how a name is stored ([`PlatformRules::stored_form`]: HFS+ stores NFD).

use crate::keys::{fold_key, nfc_key, nfd, simple_upper_key};

/// How a file system compares letter case in names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseRule {
    /// `A.md` and `a.md` are different files (Linux ext4/btrfs, case-sensitive APFS).
    Sensitive,
    /// Case-insensitive, case-preserving, with Unicode case folding (APFS default).
    InsensitiveUnicode,
    /// Case-insensitive, case-preserving, through a simple per-character
    /// uppercase table (NTFS).
    InsensitiveSimpleUpper,
}

/// How a file system treats canonically equivalent names (NFC vs NFD).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizationRule {
    /// Names are opaque bytes: an NFC and an NFD name are two files (Linux, NTFS).
    Preserving,
    /// Names are stored as given but compared normalization-insensitively (APFS).
    InsensitivePreserving,
    /// Names are converted to (a variant of) NFD when stored (HFS+).
    StoresNfd,
}

/// A portability problem with one path segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameIssue {
    /// A Windows reserved device name (`CON`, `NUL`, `COM1`, … with any extension).
    ReservedName(String),
    /// A character the file system does not allow in names.
    ForbiddenChar(char),
    /// A trailing `.` or space (silently stripped by Windows).
    TrailingDotOrSpace,
    /// The segment is longer than the file system allows.
    TooLong {
        /// Length in the unit the platform counts (UTF-16 units or bytes).
        len: usize,
        /// The limit.
        max: usize,
    },
}

/// The naming rules of one platform / file-system family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformRules {
    /// Short identifier (`linux`, `macos`, `windows`).
    pub name: &'static str,
    /// Case comparison.
    pub case: CaseRule,
    /// Normalization comparison and storage.
    pub normalization: NormalizationRule,
    /// Windows reserved device names are refused.
    pub reserved_device_names: bool,
    /// Characters that may not appear in a name (besides `/` and NUL, which
    /// no platform allows).
    pub forbidden_chars: &'static [char],
    /// Control characters U+0001..U+001F are refused.
    pub forbid_control_chars: bool,
    /// Names may not end in `.` or a space.
    pub forbid_trailing_dot_space: bool,
    /// Maximum name length in UTF-16 units (Windows, macOS) or bytes (Linux).
    pub max_name_len: usize,
    /// Whether `max_name_len` counts UTF-16 units (`true`) or UTF-8 bytes.
    pub name_len_utf16: bool,
}

const WINDOWS_FORBIDDEN: &[char] = &['<', '>', ':', '"', '\\', '|', '?', '*'];
const MAC_FORBIDDEN: &[char] = &[':'];

const RESERVED: &[&str] = &[
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
    "CONIN$",
    "CONOUT$",
];

impl PlatformRules {
    /// Linux (ext4, btrfs, xfs): case-sensitive, normalization-preserving.
    pub const LINUX: Self = Self {
        name: "linux",
        case: CaseRule::Sensitive,
        normalization: NormalizationRule::Preserving,
        reserved_device_names: false,
        forbidden_chars: &[],
        forbid_control_chars: false,
        forbid_trailing_dot_space: false,
        max_name_len: 255,
        name_len_utf16: false,
    };

    /// macOS on a default APFS volume: case-insensitive and
    /// normalization-insensitive, both preserving.
    pub const MACOS: Self = Self {
        name: "macos",
        case: CaseRule::InsensitiveUnicode,
        normalization: NormalizationRule::InsensitivePreserving,
        reserved_device_names: false,
        forbidden_chars: MAC_FORBIDDEN,
        forbid_control_chars: false,
        forbid_trailing_dot_space: false,
        max_name_len: 255,
        name_len_utf16: true,
    };

    /// macOS on HFS+ (older volumes, some external drives): case-insensitive,
    /// names stored in NFD.
    pub const MACOS_HFS: Self = Self {
        name: "macos-hfs",
        case: CaseRule::InsensitiveUnicode,
        normalization: NormalizationRule::StoresNfd,
        ..Self::MACOS
    };

    /// Windows (NTFS): case-insensitive through the upcase table,
    /// normalization-preserving, reserved device names, forbidden characters.
    pub const WINDOWS: Self = Self {
        name: "windows",
        case: CaseRule::InsensitiveSimpleUpper,
        normalization: NormalizationRule::Preserving,
        reserved_device_names: true,
        forbidden_chars: WINDOWS_FORBIDDEN,
        forbid_control_chars: true,
        forbid_trailing_dot_space: true,
        max_name_len: 255,
        name_len_utf16: true,
    };

    /// All built-in rule sets, for portability checks across platforms.
    pub const ALL: [Self; 4] = [Self::LINUX, Self::MACOS, Self::MACOS_HFS, Self::WINDOWS];

    /// The default rules of the platform this binary was built for.
    #[must_use]
    pub fn host() -> Self {
        if cfg!(windows) {
            Self::WINDOWS
        } else if cfg!(target_os = "macos") {
            Self::MACOS
        } else {
            Self::LINUX
        }
    }

    /// `true` if the file system compares names exactly (byte for byte).
    #[must_use]
    pub fn is_exact(&self) -> bool {
        self.case == CaseRule::Sensitive && self.normalization == NormalizationRule::Preserving
    }

    /// The key under which this file system considers two names (or paths)
    /// the same entry.
    #[must_use]
    pub fn collision_key(&self, name: &str) -> String {
        let normalized: String = match self.normalization {
            NormalizationRule::Preserving => name.to_owned(),
            NormalizationRule::InsensitivePreserving | NormalizationRule::StoresNfd => {
                nfc_key(name).into_owned()
            }
        };
        match self.case {
            CaseRule::Sensitive => normalized,
            CaseRule::InsensitiveUnicode => fold_key(&normalized),
            CaseRule::InsensitiveSimpleUpper => simple_upper_key(&normalized),
        }
    }

    /// `true` if `a` and `b` name the same entry on this file system.
    #[must_use]
    pub fn same_name(&self, a: &str, b: &str) -> bool {
        a == b || self.collision_key(a) == self.collision_key(b)
    }

    /// The form a newly created name is stored in (NFD on HFS+, unchanged
    /// elsewhere).
    #[must_use]
    pub fn stored_form(&self, name: &str) -> String {
        match self.normalization {
            NormalizationRule::StoresNfd => nfd(name),
            _ => name.to_owned(),
        }
    }

    /// Portability problems of one path segment under these rules.
    #[must_use]
    pub fn name_issues(&self, segment: &str) -> Vec<NameIssue> {
        let mut out = Vec::new();
        if self.reserved_device_names && is_reserved_device_name(segment) {
            out.push(NameIssue::ReservedName(segment.to_owned()));
        }
        for c in segment.chars() {
            let control = self.forbid_control_chars && ('\u{1}'..='\u{1f}').contains(&c);
            if (control || self.forbidden_chars.contains(&c))
                && !out.contains(&NameIssue::ForbiddenChar(c))
            {
                out.push(NameIssue::ForbiddenChar(c));
            }
        }
        if self.forbid_trailing_dot_space && (segment.ends_with('.') || segment.ends_with(' ')) {
            out.push(NameIssue::TrailingDotOrSpace);
        }
        let len = if self.name_len_utf16 {
            segment.encode_utf16().count()
        } else {
            segment.len()
        };
        if len > self.max_name_len {
            out.push(NameIssue::TooLong {
                len,
                max: self.max_name_len,
            });
        }
        out
    }
}

/// `true` for Windows reserved device names, with or without an extension
/// and trailing spaces/dots (`con`, `NUL.md`, `com1 .txt`).
#[must_use]
pub fn is_reserved_device_name(segment: &str) -> bool {
    let base = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .trim_end_matches([' ', '.']);
    RESERVED
        .iter()
        .any(|r| r.eq_ignore_ascii_case(base) || simple_upper_key(base) == *r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_twins_collide_only_where_case_insensitive() {
        assert!(!PlatformRules::LINUX.same_name("Case.md", "case.md"));
        assert!(PlatformRules::MACOS.same_name("Case.md", "case.md"));
        assert!(PlatformRules::WINDOWS.same_name("Case.md", "case.md"));
    }

    #[test]
    fn nfc_nfd_twins() {
        let nfc = "Caf\u{e9}.md";
        let nfd = "Cafe\u{301}.md";
        assert!(!PlatformRules::LINUX.same_name(nfc, nfd));
        assert!(PlatformRules::MACOS.same_name(nfc, nfd));
        assert!(!PlatformRules::WINDOWS.same_name(nfc, nfd));
        assert_eq!(PlatformRules::MACOS_HFS.stored_form(nfc), nfd);
    }

    #[test]
    fn ntfs_does_not_fold_sharp_s() {
        assert!(!PlatformRules::WINDOWS.same_name("Stra\u{df}e", "STRASSE"));
        assert!(PlatformRules::MACOS.same_name("Stra\u{df}e", "STRASSE"));
    }

    #[test]
    fn windows_names() {
        let w = PlatformRules::WINDOWS;
        assert_eq!(
            w.name_issues("con.md"),
            vec![NameIssue::ReservedName("con.md".into())]
        );
        assert_eq!(
            w.name_issues("Pipe|name.md"),
            vec![NameIssue::ForbiddenChar('|')]
        );
        assert_eq!(w.name_issues("dot."), vec![NameIssue::TrailingDotOrSpace]);
        assert!(w.name_issues("console.md").is_empty());
        assert!(
            w.name_issues("COM\u{b9}.txt")
                .contains(&NameIssue::ReservedName("COM\u{b9}.txt".into()))
        );
        assert!(PlatformRules::LINUX.name_issues("con.md").is_empty());
        assert_eq!(
            PlatformRules::MACOS.name_issues("a:b"),
            vec![NameIssue::ForbiddenChar(':')]
        );
        let long = "\u{e9}".repeat(200);
        assert!(
            PlatformRules::LINUX
                .name_issues(&long)
                .iter()
                .any(|i| matches!(i, NameIssue::TooLong { .. }))
        );
        assert!(PlatformRules::WINDOWS.name_issues(&long).is_empty());
    }
}
