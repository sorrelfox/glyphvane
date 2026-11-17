// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Applying, undoing and recovering refactors, through the refactor
//! journal.
//!
//! A refactor runs as **steps**: note edits (each checked against the hash
//! the plan saw), then moves. Before the first step, the journal file
//! (`<journal dir>/<id>.toml`) records every step with the original texts;
//! it is removed when the steps are done. A failure undoes the steps done
//! so far, in reverse; so does [`recover`] for a journal left behind by a
//! crash.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{Move, RefactorError, RefactorPlan};
use crate::files::{Backup, ContentHash, WriteStrategy, write_file};
use crate::path::VaultPath;

/// One note edit of an applied refactor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEdit {
    /// Where the note was before the refactor.
    pub path: VaultPath,
    /// Where it is now.
    pub moved_to: VaultPath,
    /// Its text before the refactor.
    pub before: String,
    /// The hash of its bytes after the refactor.
    pub after_hash: ContentHash,
}

/// A refactor that was applied: what [`undo`] needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedRefactor {
    /// The vault root.
    pub root: PathBuf,
    /// The moves, in the order they were done.
    pub moves: Vec<Move>,
    /// The edited notes.
    pub edits: Vec<AppliedEdit>,
}

impl AppliedRefactor {
    /// The number of notes whose text changed.
    #[must_use]
    pub fn edited_notes(&self) -> usize {
        self.edits.len()
    }
}

/// A refactor rolled back by [`recover`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    /// The journal's id.
    pub id: String,
    /// Steps that could not be undone (a file changed meanwhile).
    pub problems: Vec<String>,
}

/// Applies `plan`: checks that every note is as planned, journals the
/// steps in `journal_dir` (none when `None`), writes the edited notes,
/// then moves the entries. On failure everything done is rolled back and
/// the error returned.
pub fn apply(
    plan: &RefactorPlan,
    journal_dir: Option<&Path>,
) -> Result<AppliedRefactor, RefactorError> {
    let steps = Steps {
        root: plan.root.clone(),
        edits: plan
            .edits
            .iter()
            .map(|e| EditStep {
                path: e.path.clone(),
                expect: e.before_hash,
                write: e.after.clone(),
                restore: e.before.clone(),
            })
            .collect(),
        moves: plan.moves.clone(),
    };
    run(&steps, journal_dir)?;
    Ok(AppliedRefactor {
        root: plan.root.clone(),
        moves: plan.moves.clone(),
        edits: plan
            .edits
            .iter()
            .map(|e| AppliedEdit {
                path: e.path.clone(),
                moved_to: e.moved_to.clone(),
                before: e.before.clone(),
                after_hash: ContentHash::of(e.after.as_bytes()),
            })
            .collect(),
    })
}

/// Reverts `applied` in one step: the moves go back and the notes get
/// their old text. Refused, with nothing changed, when a note was edited
/// or an entry moved since.
pub fn undo(applied: &AppliedRefactor, journal_dir: Option<&Path>) -> Result<(), RefactorError> {
    let root = &applied.root;
    let mut edits = Vec::new();
    for e in &applied.edits {
        let full = e.moved_to.to_fs_path(root);
        let now = fs::read(&full).map_err(|err| RefactorError::io(&full, err))?;
        // The text written by the refactor, needed to redo on rollback.
        let after =
            String::from_utf8(now).map_err(|_| RefactorError::Changed(e.moved_to.clone()))?;
        edits.push(EditStep {
            path: e.moved_to.clone(),
            expect: e.after_hash,
            write: e.before.clone(),
            restore: after,
        });
    }
    let moves: Vec<Move> = applied
        .moves
        .iter()
        .rev()
        .map(|m| Move {
            from: m.to.clone(),
            to: m.from.clone(),
        })
        .collect();
    for m in &moves {
        let m: &Move = m;
        if fs::symlink_metadata(m.from.to_fs_path(root)).is_err() {
            return Err(RefactorError::Missing(m.from.clone()));
        }
        if !m.is_case_only() && fs::symlink_metadata(m.to.to_fs_path(root)).is_ok() {
            return Err(RefactorError::Exists(m.to.clone()));
        }
    }
    run(
        &Steps {
            root: root.clone(),
            edits,
            moves,
        },
        journal_dir,
    )
}

/// Rolls back the refactors of the vault at `root` whose journals are
/// still in `journal_dir` (interrupted by a crash), and removes their
/// journals.
pub fn recover(root: &Path, journal_dir: &Path) -> Vec<Recovered> {
    let Ok(entries) = fs::read_dir(journal_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        // An unreadable journal is left in place for a person to look at.
        let Ok(journal) = toml::from_str::<Journal>(&text) else {
            continue;
        };
        if journal.root != root {
            continue;
        }
        let Some(steps) = journal.steps() else {
            continue;
        };
        let problems = rollback(&steps, steps.edits.len(), steps.moves.len());
        let _ = fs::remove_file(&path);
        out.push(Recovered {
            id: path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            problems,
        });
    }
    out
}

// ----- steps ------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(super) struct EditStep {
    path: VaultPath,
    /// The hash the file must have before the write.
    expect: ContentHash,
    write: String,
    /// What a rollback writes back.
    restore: String,
}

#[derive(Debug, Clone)]
pub(super) struct Steps {
    root: PathBuf,
    edits: Vec<EditStep>,
    moves: Vec<Move>,
}

/// The journal already holds the original: in-place writes need no other
/// copy.
struct Journaled;

impl Backup for Journaled {
    fn store(&mut self, _: &Path, _: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn clear(&mut self, _: &Path) -> io::Result<()> {
        Ok(())
    }
}

fn run(steps: &Steps, journal_dir: Option<&Path>) -> Result<(), RefactorError> {
    let root = &steps.root;
    for e in &steps.edits {
        let full = e.path.to_fs_path(root);
        let now = fs::read(&full).map_err(|err| RefactorError::io(&full, err))?;
        if ContentHash::of(&now) != e.expect {
            return Err(RefactorError::Changed(e.path.clone()));
        }
    }
    let journal = match journal_dir {
        Some(dir) => Some(write_journal(dir, steps)?),
        None => None,
    };
    let mut edited = 0;
    let mut result = Ok(());
    for e in &steps.edits {
        let full = e.path.to_fs_path(root);
        if let Err(err) = write_file(
            &full,
            e.write.as_bytes(),
            WriteStrategy::Auto,
            &mut Journaled,
            None,
        ) {
            result = Err(RefactorError::io(&full, io::Error::other(err.to_string())));
            break;
        }
        edited += 1;
    }
    let mut moved = 0;
    if result.is_ok() {
        for m in &steps.moves {
            if let Err(err) = move_entry(root, m) {
                result = Err(err);
                break;
            }
            moved += 1;
        }
    }
    if let Err(err) = &result {
        let problems = rollback(steps, edited, moved);
        if !problems.is_empty() {
            result = Err(RefactorError::Incomplete(format!(
                "{err}; not rolled back: {}",
                problems.join("; ")
            )));
        }
    }
    if let Some(journal) = journal {
        let _ = fs::remove_file(journal);
    }
    result
}

/// Undoes the first `moved` moves (in reverse), then the first `edited`
/// edits. Returns what could not be undone.
fn rollback(steps: &Steps, edited: usize, moved: usize) -> Vec<String> {
    let root = &steps.root;
    let mut problems = Vec::new();
    for m in steps.moves[..moved].iter().rev() {
        let back = Move {
            from: m.to.clone(),
            to: m.from.clone(),
        };
        let at_new = fs::symlink_metadata(back.from.to_fs_path(root)).is_ok();
        let at_old = fs::symlink_metadata(back.to.to_fs_path(root)).is_ok();
        if back.is_case_only() {
            // Both names reach the entry on a case-insensitive file
            // system: the name in the folder listing decides.
            if listed_name(root, &back.from)
                && let Err(err) = move_entry(root, &back)
            {
                problems.push(format!("“{}” was not moved back: {err}", m.to.as_str()));
            }
            continue;
        }
        if !at_new && at_old {
            // Never done (a journal left by a crash).
            continue;
        }
        if !at_new || at_old {
            problems.push(format!("“{}” was not moved back", m.to.as_str()));
            continue;
        }
        if let Err(err) = move_entry(root, &back) {
            problems.push(format!("“{}” was not moved back: {err}", m.to.as_str()));
        }
    }
    for e in steps.edits[..edited].iter().rev() {
        let full = e.path.to_fs_path(root);
        let Ok(now) = fs::read(&full) else {
            problems.push(format!("“{}” could not be read", e.path.as_str()));
            continue;
        };
        if now != e.write.as_bytes() {
            if now != e.restore.as_bytes() {
                problems.push(format!("“{}” changed meanwhile", e.path.as_str()));
            }
            continue;
        }
        if let Err(err) = write_file(
            &full,
            e.restore.as_bytes(),
            WriteStrategy::Auto,
            &mut Journaled,
            None,
        ) {
            problems.push(format!("“{}” was not restored: {err}", e.path.as_str()));
        }
    }
    problems
}

/// Renames one entry; a case-only rename goes through a temporary name,
/// which case-insensitive file systems need.
fn move_entry(root: &Path, m: &Move) -> Result<(), RefactorError> {
    let from = m.from.to_fs_path(root);
    let to = m.to.to_fs_path(root);
    if m.is_case_only() {
        let name = to
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = to.with_file_name(format!(".{name}.gv-tmp-{}", unique_suffix()));
        fs::rename(&from, &tmp).map_err(|e| RefactorError::io(&from, e))?;
        if let Err(e) = fs::rename(&tmp, &to) {
            let _ = fs::rename(&tmp, &from);
            return Err(RefactorError::io(&to, e));
        }
        return Ok(());
    }
    if fs::symlink_metadata(&to).is_ok() {
        return Err(RefactorError::Exists(m.to.clone()));
    }
    fs::rename(&from, &to).map_err(|e| RefactorError::io(&from, e))
}

/// Whether the folder of `path` lists an entry named exactly like it.
fn listed_name(root: &Path, path: &VaultPath) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    let dir = path.parent().unwrap_or_default().to_fs_path(root);
    fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_str() == Some(name))
    })
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{:x}{:x}", std::process::id(), nanos)
}

// ----- the journal file ---------------------------------------------------------

const JOURNAL_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    version: u32,
    root: PathBuf,
    edits: Vec<JournalEdit>,
    moves: Vec<JournalMove>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalEdit {
    path: String,
    /// The hash before the step, hexadecimal.
    expect: String,
    write: String,
    restore: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalMove {
    from: String,
    to: String,
}

impl Journal {
    fn of(steps: &Steps) -> Self {
        Self {
            version: JOURNAL_VERSION,
            root: steps.root.clone(),
            edits: steps
                .edits
                .iter()
                .map(|e| JournalEdit {
                    path: e.path.as_str().to_owned(),
                    expect: e.expect.to_hex(),
                    write: e.write.clone(),
                    restore: e.restore.clone(),
                })
                .collect(),
            moves: steps
                .moves
                .iter()
                .map(|m| JournalMove {
                    from: m.from.as_str().to_owned(),
                    to: m.to.as_str().to_owned(),
                })
                .collect(),
        }
    }

    fn steps(&self) -> Option<Steps> {
        if self.version != JOURNAL_VERSION {
            return None;
        }
        let edits = self
            .edits
            .iter()
            .map(|e| {
                Some(EditStep {
                    path: VaultPath::new(&e.path).ok()?,
                    expect: ContentHash::of(e.restore.as_bytes()),
                    write: e.write.clone(),
                    restore: e.restore.clone(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let moves = self
            .moves
            .iter()
            .map(|m| {
                Some(Move {
                    from: VaultPath::new(&m.from).ok()?,
                    to: VaultPath::new(&m.to).ok()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Steps {
            root: self.root.clone(),
            edits,
            moves,
        })
    }
}

fn write_journal(dir: &Path, steps: &Steps) -> Result<PathBuf, RefactorError> {
    fs::create_dir_all(dir).map_err(|e| RefactorError::Journal(e.to_string()))?;
    let text =
        toml::to_string(&Journal::of(steps)).map_err(|e| RefactorError::Journal(e.to_string()))?;
    let path = dir.join(format!("{}.toml", unique_suffix()));
    write_file(
        &path,
        text.as_bytes(),
        WriteStrategy::Auto,
        &mut Journaled,
        None,
    )
    .map_err(|e| RefactorError::Journal(e.to_string()))?;
    Ok(path)
}

#[cfg(test)]
pub(super) mod tests_support {
    use std::path::Path;

    use super::{EditStep, Steps, write_journal};
    use crate::refactor::RefactorPlan;

    /// The steps [`super::apply`] runs for `plan`.
    pub fn steps_of(plan: &RefactorPlan) -> Steps {
        Steps {
            root: plan.root.clone(),
            edits: plan
                .edits
                .iter()
                .map(|e| EditStep {
                    path: e.path.clone(),
                    expect: e.before_hash,
                    write: e.after.clone(),
                    restore: e.before.clone(),
                })
                .collect(),
            moves: plan.moves.clone(),
        }
    }

    /// Writes the journal of `steps`, as before the first step.
    pub fn journal(dir: &Path, steps: &Steps) {
        write_journal(dir, steps).expect("journal");
    }
}
