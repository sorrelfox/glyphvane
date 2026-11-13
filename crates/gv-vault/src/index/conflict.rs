// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Sync-conflict copies (Dropbox, Syncthing, OneDrive, iCloud).
//!
//! Conflict copies are excluded from basename resolution, so a stray
//! `Note (conflicted copy 2026-01-02).md` never makes `[[Note]]` ambiguous.
//! Dropbox and Syncthing names are distinctive and detected by pattern
//! alone. The OneDrive (`name-HOST.md`) and iCloud (`name 2.md`) patterns are
//! ordinary-looking names, so they only count when the original file exists
//! next to the copy.

use crate::path::{extension_of, stem_of};

/// The service whose conflict pattern a name matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncService {
    /// `name (conflicted copy YYYY-MM-DD).ext`, also `(… 's conflicted copy …)`.
    Dropbox,
    /// `name.sync-conflict-YYYYMMDD-HHMMSS-DEVICEID.ext`.
    Syncthing,
    /// `name-COMPUTERNAME.ext` (needs the original).
    OneDrive,
    /// `name N.ext` with N ≥ 2 (needs the original).
    ICloud,
}

/// A detected conflict copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConflict {
    /// The service pattern.
    pub service: SyncService,
    /// The file name of the original (`Note.md`).
    pub original: String,
}

/// Detects whether `name` is a sync-conflict copy. `original_exists` is
/// asked whether a sibling with the given file name exists; it is only
/// consulted for the heuristic OneDrive and iCloud patterns.
pub fn detect_sync_conflict(
    name: &str,
    original_exists: impl Fn(&str) -> bool,
) -> Option<SyncConflict> {
    let ext = extension_of(name);
    let stem = stem_of(name);
    let with_ext = |base: &str| match ext {
        Some(e) => format!("{base}.{e}"),
        None => base.to_owned(),
    };

    // Dropbox: "Note (conflicted copy 2026-01-02).md",
    // "Note (Jane's conflicted copy 2026-01-02).md", and a " (1)" counter
    // after either.
    if let Some(pos) = stem.find("conflicted copy ")
        && let Some(open) = stem[..pos].rfind(" (")
        && !stem[open + 2..pos].contains(')')
        && is_iso_date(&stem[pos + "conflicted copy ".len()..])
        && stem[pos + "conflicted copy ".len() + 10..].starts_with(')')
    {
        return Some(SyncConflict {
            service: SyncService::Dropbox,
            original: with_ext(&stem[..open]),
        });
    }

    // Syncthing: "Note.sync-conflict-20260102-030405-ABCDEFG.md".
    if let Some(pos) = stem.rfind(".sync-conflict-") {
        let tail = &stem[pos + ".sync-conflict-".len()..];
        let parts: Vec<&str> = tail.split('-').collect();
        if parts.len() == 3
            && parts[0].len() == 8
            && parts[0].bytes().all(|b| b.is_ascii_digit())
            && parts[1].len() == 6
            && parts[1].bytes().all(|b| b.is_ascii_digit())
            && !parts[2].is_empty()
            && parts[2].bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Some(SyncConflict {
                service: SyncService::Syncthing,
                original: with_ext(&stem[..pos]),
            });
        }
    }

    // OneDrive: "Note-DESKTOP1.md"; NetBIOS names are 1-15 characters.
    if let Some(dash) = stem.rfind('-') {
        let host = &stem[dash + 1..];
        let base = &stem[..dash];
        if !base.is_empty()
            && (1..=15).contains(&host.len())
            && host
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
            && host.bytes().any(|b| b.is_ascii_uppercase())
            && original_exists(&with_ext(base))
        {
            return Some(SyncConflict {
                service: SyncService::OneDrive,
                original: with_ext(base),
            });
        }
    }

    // iCloud: "Note 2.md".
    if let Some(space) = stem.rfind(' ') {
        let n = &stem[space + 1..];
        let base = &stem[..space];
        if !base.is_empty()
            && !n.is_empty()
            && n.len() <= 3
            && n.bytes().all(|b| b.is_ascii_digit())
            && n.parse::<u32>().is_ok_and(|v| v >= 2)
            && original_exists(&with_ext(base))
        {
            return Some(SyncConflict {
                service: SyncService::ICloud,
                original: with_ext(base),
            });
        }
    }
    None
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn dropbox_and_syncthing_by_pattern() {
        let d = detect_sync_conflict("Note (conflicted copy 2026-01-02).md", none).unwrap();
        assert_eq!(
            (d.service, d.original.as_str()),
            (SyncService::Dropbox, "Note.md")
        );
        let d = detect_sync_conflict("My Note (Jane's conflicted copy 2026-01-02) (1).md", none)
            .unwrap();
        assert_eq!(d.original, "My Note.md");
        let s =
            detect_sync_conflict("Note.sync-conflict-20260102-030405-ABC1234.md", none).unwrap();
        assert_eq!(
            (s.service, s.original.as_str()),
            (SyncService::Syncthing, "Note.md")
        );
        assert_eq!(detect_sync_conflict("Note (draft).md", none), None);
    }

    #[test]
    fn heuristic_patterns_need_the_original() {
        assert_eq!(detect_sync_conflict("Note-DESKTOP1.md", none), None);
        let o = detect_sync_conflict("Note-DESKTOP1.md", |n| n == "Note.md").unwrap();
        assert_eq!(o.service, SyncService::OneDrive);
        assert_eq!(detect_sync_conflict("Note 2.md", none), None);
        let i = detect_sync_conflict("Note 2.md", |n| n == "Note.md").unwrap();
        assert_eq!(i.service, SyncService::ICloud);
        // "Chapter 1" is not a copy of "Chapter".
        assert_eq!(detect_sync_conflict("Chapter 1.md", |_| true), None);
        assert_eq!(detect_sync_conflict("my-notes.md", |_| true), None);
    }
}
