// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The session file: `session.json` in the data folder, holding the windows
//! a plain launch restores and the recent files and folders.
//!
//! JSON rather than TOML: the file is state the app writes and reads back,
//! not something people edit. It nests deeply (windows, tabs, history) and
//! needs no comments or format-preserving edits, and `serde_json` writes and
//! reads it in one call.
//!
//! The file carries `"version": N` like the settings files:
//!
//! - an older version is upgraded through the migration chain, and the old
//!   file is kept as `session.json.vN.bak`;
//! - a newer version (a newer Glyphvane ran here) is not restored, and this
//!   run never writes the file, so the newer state survives a downgrade;
//! - a file that cannot be parsed is moved aside to
//!   `session.json.corrupt-<unix seconds>` and the session starts empty;
//! - a file that cannot be read is left alone and not written this run.
//!
//! Writes are atomic ([`gv_settings::atomic_write`]).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gv_settings::{Locations, atomic_write, backup_path};
use gv_ui::shell::session::{SESSION_VERSION, Session, SessionSink, SessionStore};
use serde_json::Value;

/// One step of the chain: upgrades a session from `from` to `from + 1`.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub from: u32,
    pub apply: fn(&mut Value) -> Result<(), String>,
}

/// The real chain. Version 1 is the first version, so it is empty.
const MIGRATIONS: &[Migration] = &[];

/// Why a session file is not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Written by a newer Glyphvane, with this version.
    Newer(u32),
    /// Not a session this build understands.
    Damaged(String),
}

/// Parses a session file, upgrading it through `chain` to `target`. Returns
/// the session and the version the file had.
fn parse(text: &str, chain: &[Migration], target: u32) -> Result<(Session, u32), Problem> {
    let mut value: Value =
        serde_json::from_str(text).map_err(|e| Problem::Damaged(e.to_string()))?;
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v >= 1)
        .ok_or_else(|| Problem::Damaged("no valid `version`".to_owned()))?;
    if version > target {
        return Err(Problem::Newer(version));
    }
    for from in version..target {
        let step = chain
            .iter()
            .find(|m| m.from == from)
            .ok_or_else(|| Problem::Damaged(format!("no upgrade from session version {from}")))?;
        (step.apply)(&mut value)
            .map_err(|e| Problem::Damaged(format!("upgrading from session version {from}: {e}")))?;
        value["version"] = Value::from(from + 1);
    }
    let session = serde_json::from_value(value).map_err(|e| Problem::Damaged(e.to_string()))?;
    Ok((session, version))
}

/// Writes the session to a file as pretty JSON, atomically.
#[derive(Debug)]
pub struct JsonFile {
    path: PathBuf,
}

impl SessionSink for JsonFile {
    fn save(&self, session: &Session) -> io::Result<()> {
        let mut text = serde_json::to_vec_pretty(session).map_err(io::Error::other)?;
        text.push(b'\n');
        atomic_write(&self.path, &text)
    }
}

/// What loading found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No session yet.
    Missing,
    Loaded,
    /// Upgraded from an older version; the old file was kept there.
    Migrated {
        from: u32,
        backup: PathBuf,
    },
    /// Newer than this build: not restored, not written.
    Newer {
        found: u32,
    },
    /// Moved aside there (`None` when that failed: then it is not written).
    Damaged {
        backup: Option<PathBuf>,
    },
    /// Could not be read: not written.
    Unreadable,
}

/// Loads the session at `path` by the rules in the module docs. Returns the
/// session, where to save it (`None`: do not write the file this run) and
/// what was found. Logs what it did.
pub fn load(path: &Path) -> (Session, Option<Arc<dyn SessionSink>>, Outcome) {
    load_to(path, MIGRATIONS, SESSION_VERSION)
}

fn load_to(
    path: &Path,
    chain: &[Migration],
    target: u32,
) -> (Session, Option<Arc<dyn SessionSink>>, Outcome) {
    let sink = || -> Option<Arc<dyn SessionSink>> {
        Some(Arc::new(JsonFile {
            path: path.to_path_buf(),
        }))
    };
    let shown = path.display();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            tracing::info!("session: none saved yet at {shown}");
            return (Session::default(), sink(), Outcome::Missing);
        }
        Err(err) => {
            tracing::warn!(
                "session: cannot read {shown}, so the session is not restored or saved: {err}"
            );
            return (Session::default(), None, Outcome::Unreadable);
        }
    };
    match parse(&text, chain, target) {
        Ok((session, from)) if from == target => {
            tracing::info!(
                "session: loaded {shown} ({} window(s))",
                session.windows.len()
            );
            (session, sink(), Outcome::Loaded)
        }
        Ok((session, from)) => {
            let backup = backup_path(path, from);
            match atomic_write(&backup, text.as_bytes()) {
                Ok(()) => {
                    tracing::info!(
                        "session: upgraded {shown} from version {from}; the old file is {}",
                        backup.display()
                    );
                    (session, sink(), Outcome::Migrated { from, backup })
                }
                Err(err) => {
                    tracing::warn!(
                        "session: cannot keep a backup of {shown} before upgrading it, so it \
                         is not saved: {err}"
                    );
                    (session, None, Outcome::Migrated { from, backup })
                }
            }
        }
        Err(Problem::Newer(found)) => {
            tracing::warn!(
                "session: {shown} was written by a newer version of Glyphvane (session version \
                 {found}; this version reads up to {SESSION_VERSION}). It is not restored, and \
                 it is not changed while this version runs"
            );
            (Session::default(), None, Outcome::Newer { found })
        }
        Err(Problem::Damaged(why)) => {
            let backup = corrupt_path(path);
            match std::fs::rename(path, &backup) {
                Ok(()) => {
                    tracing::warn!(
                        "session: {shown} is damaged ({why}); it was moved to {} and the \
                         session starts empty",
                        backup.display()
                    );
                    (
                        Session::default(),
                        sink(),
                        Outcome::Damaged {
                            backup: Some(backup),
                        },
                    )
                }
                Err(err) => {
                    tracing::warn!(
                        "session: {shown} is damaged ({why}) and cannot be moved aside ({err}); \
                         the session starts empty and is not saved"
                    );
                    (Session::default(), None, Outcome::Damaged { backup: None })
                }
            }
        }
    }
}

/// `session.json.corrupt-<unix seconds>`, with `-2`, `-3`… when taken.
fn corrupt_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session.json".to_owned());
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let mut candidate = path.with_file_name(format!("{name}.corrupt-{stamp}"));
    let mut n = 2;
    while candidate.exists() {
        candidate = path.with_file_name(format!("{name}.corrupt-{stamp}-{n}"));
        n += 1;
    }
    candidate
}

/// The session store of this run: loaded from the data folder, or empty and
/// kept in memory when there is none.
pub fn store(locations: Option<&Locations>) -> SessionStore {
    match locations {
        Some(locations) => {
            let (session, sink, _) = load(&locations.session_file());
            SessionStore::new(session, sink)
        }
        None => {
            tracing::warn!("session: no home directory, so the session is not saved");
            SessionStore::new(Session::default(), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gv_ui::shell::ViewMode;
    use gv_ui::shell::session::{Anchor, TabState, WindowState};

    use super::*;

    fn sample() -> Session {
        Session {
            windows: vec![WindowState {
                bounds: None,
                tabs: vec![TabState {
                    path: PathBuf::from("/n/a.md"),
                    mode: ViewMode::Edit,
                    anchor: Some(Anchor::Offset(42)),
                    back: Vec::new(),
                    forward: Vec::new(),
                    split_ratio: Some(0.4),
                    folds: vec![[10, 20]],
                    reading_folds: Vec::new(),
                    split_reading_folds: Vec::new(),
                    live_preview: false,
                    viewer_mode: None,
                    pdf: None,
                }],
                active_tab: 0,
                sidebar_open: true,
                panel_open: false,
                sidebar_panel: Some("search".into()),
                vault: Some(PathBuf::from("/n")),
                base: None,
                files_sort: None,
            }],
            recent_files: vec![PathBuf::from("/n/a.md")],
            ..Session::default()
        }
    }

    #[test]
    fn a_saved_session_loads_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("data").join("session.json");
        let (_, sink, outcome) = load(&path);
        assert_eq!(outcome, Outcome::Missing);
        sink.expect("writable").save(&sample()).expect("saved");
        let text = std::fs::read_to_string(&path).expect("written");
        assert!(text.starts_with("{\n  \"version\": 1,"), "{text}");

        let (session, sink, outcome) = load(&path);
        assert_eq!(outcome, Outcome::Loaded);
        assert_eq!(session, sample());
        assert!(sink.is_some());
    }

    #[test]
    fn a_damaged_file_is_moved_aside() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.json");
        for bad in [
            "{not json",
            "{\"windows\": []}",
            "{\"version\": 1, \"windows\": 3}",
        ] {
            std::fs::write(&path, bad).expect("written");
            let (session, sink, outcome) = load(&path);
            let Outcome::Damaged {
                backup: Some(backup),
            } = outcome
            else {
                panic!("{bad}: {outcome:?}");
            };
            assert!(
                backup
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("session.json.corrupt-")),
                "{backup:?}"
            );
            assert_eq!(std::fs::read_to_string(&backup).expect("kept"), bad);
            assert!(!path.exists());
            assert_eq!(session, Session::default());
            assert!(sink.is_some(), "a fresh session is saved again");
        }
        // Three backups, none overwritten.
        let backups = std::fs::read_dir(dir.path()).expect("listed").count();
        assert_eq!(backups, 3);
    }

    #[test]
    fn a_newer_file_is_neither_restored_nor_written() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.json");
        let newer = r#"{"version": 99, "windows": [{"tabs": [{"path": "/n/a.md"}]}]}"#;
        std::fs::write(&path, newer).expect("written");
        let (session, sink, outcome) = load(&path);
        assert_eq!(outcome, Outcome::Newer { found: 99 });
        assert_eq!(session, Session::default());
        assert!(sink.is_none());
        assert_eq!(std::fs::read_to_string(&path).expect("kept"), newer);
    }

    fn add_recent(value: &mut Value) -> Result<(), String> {
        let files = value
            .get("old_recent")
            .cloned()
            .ok_or_else(|| "no old_recent".to_owned())?;
        value["recent_files"] = files;
        Ok(())
    }

    #[test]
    fn an_older_file_is_upgraded_with_a_backup() {
        // This build has no older version yet; upgrade to a pretend
        // version 2 through a pretend step.
        let chain = [Migration {
            from: 1,
            apply: add_recent,
        }];
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.json");
        let old = r#"{"version": 1, "old_recent": ["/n/a.md"]}"#;
        std::fs::write(&path, old).expect("written");
        let (session, sink, outcome) = load_to(&path, &chain, 2);
        let backup = dir.path().join("session.json.v1.bak");
        assert_eq!(
            outcome,
            Outcome::Migrated {
                from: 1,
                backup: backup.clone()
            }
        );
        assert_eq!(session.version, 2);
        assert_eq!(session.recent_files, [PathBuf::from("/n/a.md")]);
        assert_eq!(std::fs::read_to_string(&backup).expect("kept"), old);
        assert!(sink.is_some());

        // A missing step or a broken one leaves the file damaged.
        assert!(matches!(
            parse(old, &chain, 3),
            Err(Problem::Damaged(why)) if why.contains("version 2")
        ));
        assert!(matches!(
            parse(r#"{"version": 1}"#, &chain, 2),
            Err(Problem::Damaged(why)) if why.contains("no old_recent")
        ));
        // Version 0 is not a version.
        assert!(matches!(
            parse(r#"{"version": 0}"#, &chain, 2),
            Err(Problem::Damaged(_))
        ));
    }

    #[test]
    fn unknown_keys_from_a_later_minor_change_are_ignored() {
        let (session, _) = parse(
            r#"{"version": 1, "recent_files": ["/n/a.md"], "something_new": {"x": 1}}"#,
            MIGRATIONS,
            SESSION_VERSION,
        )
        .expect("parses");
        assert_eq!(session.recent_files, [PathBuf::from("/n/a.md")]);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_left_alone() {
        let dir = tempfile::tempdir().expect("temp dir");
        // A folder where the file should be cannot be read as one.
        let path = dir.path().join("session.json");
        std::fs::create_dir(&path).expect("folder");
        let (_, sink, outcome) = load(&path);
        assert_eq!(outcome, Outcome::Unreadable);
        assert!(sink.is_none());
        assert!(path.is_dir());
    }
}
