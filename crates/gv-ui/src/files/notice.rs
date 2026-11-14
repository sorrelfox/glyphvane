// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the infobar says about a document's file (SPEC § Infobars).
//!
//! Only "Changed on disk with unsaved edits" is designed (an error infobar
//! with Compare, Keep mine and Take theirs). The others are built from the
//! same component and flagged where they are made: save errors use the
//! error kind; recovery, a file deleted on disk, a sync-conflict copy and
//! an untitled document that cannot be saved yet use the information kind.

use std::path::{Path, PathBuf};

use gpui::SharedString;
use gv_text::Encoding;
use gv_vault::files::journal::DiskComparison;
use gv_vault::index::conflict::SyncService;

/// The infobar kinds and their fills (SPEC § Infobars).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Warning,
    Information,
    Error,
    Neutral,
}

/// What a notice is about; a newer notice on the same topic replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    SaveFailed,
    Conflict,
    Deleted,
    Recovered,
    SyncConflict,
    Untitled,
    NotText,
}

/// What an infobar button does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeAction {
    /// Save again, converting the text to UTF-8.
    SaveAsUtf8,
    /// The conflict compare and merge view (not designed yet: disabled).
    Compare,
    /// Keep the edits; the next save overwrites the version on disk.
    KeepMine,
    /// Reload the version on disk; the edits stay in the undo history.
    TakeTheirs,
    /// Keep the recovered text.
    KeepRecovered,
    /// Drop the recovered text and go back to the file on disk.
    DiscardRecovered,
    /// Write the text to the deleted file's path again.
    Recreate,
    /// Close the tab.
    Close,
    /// Open a sync-conflict copy in a new tab.
    OpenCopy(PathBuf),
}

/// A button of an infobar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeButton {
    pub action: NoticeAction,
    pub label: SharedString,
    /// Why the button cannot be used, shown as its tooltip; `None` when it
    /// can.
    pub disabled: Option<SharedString>,
}

impl NoticeButton {
    fn new(action: NoticeAction, label: &'static str) -> Self {
        Self {
            action,
            label: label.into(),
            disabled: None,
        }
    }

    fn disabled(action: NoticeAction, label: &'static str, why: &'static str) -> Self {
        Self {
            action,
            label: label.into(),
            disabled: Some(why.into()),
        }
    }
}

/// An infobar: a kind, a message whose first sentence is drawn in weight
/// 600 (`title`) and the rest after it (`detail`), and actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub topic: Topic,
    pub kind: NoticeKind,
    pub title: SharedString,
    pub detail: Option<SharedString>,
    pub buttons: Vec<NoticeButton>,
}

impl Notice {
    fn new(topic: Topic, kind: NoticeKind, title: &'static str, detail: Option<String>) -> Self {
        Self {
            topic,
            kind,
            title: title.into(),
            detail: detail.map(Into::into),
            buttons: Vec::new(),
        }
    }

    fn with(mut self, button: NoticeButton) -> Self {
        self.buttons.push(button);
        self
    }

    /// Whether the infobar shows a button for `action`.
    pub fn offers(&self, action: &NoticeAction) -> bool {
        self.buttons.iter().any(|b| &b.action == action)
    }

    /// The designed infobar: the file changed on disk while the buffer has
    /// unsaved edits (FR-FILE-06).
    pub fn conflict() -> Self {
        Self::new(
            Topic::Conflict,
            NoticeKind::Error,
            "Changed on disk with unsaved edits.",
            Some("Another program changed this file while you were editing it.".to_owned()),
        )
        .with(NoticeButton::disabled(
            NoticeAction::Compare,
            "Compare",
            "Compare and merge is not available yet",
        ))
        .with(NoticeButton::new(NoticeAction::KeepMine, "Keep mine"))
        .with(NoticeButton::new(NoticeAction::TakeTheirs, "Take theirs"))
    }

    /// A character the file's encoding cannot hold (not designed).
    pub fn unmappable(line: usize, column: usize, ch: char, encoding: Encoding) -> Self {
        let code = u32::from(ch);
        Self::new(
            Topic::SaveFailed,
            NoticeKind::Error,
            "Not saved.",
            Some(format!(
                "Line {line}, column {column}: “{ch}” (U+{code:04X}) cannot be written in {encoding}."
            )),
        )
        .with(NoticeButton::new(NoticeAction::SaveAsUtf8, "Save as UTF-8"))
    }

    /// A file whose bytes do not survive its encoding (not designed).
    pub fn lossy(encoding: Encoding) -> Self {
        Self::new(
            Topic::SaveFailed,
            NoticeKind::Error,
            "Not saved.",
            Some(format!(
                "The file is not valid {encoding}, so saving it in {encoding} would change bytes you did not edit."
            )),
        )
        .with(NoticeButton::new(NoticeAction::SaveAsUtf8, "Save as UTF-8"))
    }

    /// Any other failed save (not designed).
    pub fn save_failed(detail: String) -> Self {
        Self::new(
            Topic::SaveFailed,
            NoticeKind::Error,
            "Not saved.",
            Some(detail),
        )
    }

    /// The file was deleted or moved away on disk (not designed).
    /// `can_save`: the tab holds the text, so it can be written again.
    pub fn deleted(can_save: bool) -> Self {
        let detail = if can_save {
            "The file was deleted or moved by another program. Save writes this text to it again."
        } else {
            "The file was deleted or moved by another program."
        };
        let notice = Self::new(
            Topic::Deleted,
            NoticeKind::Information,
            "Deleted on disk.",
            Some(detail.to_owned()),
        );
        let notice = if can_save {
            notice.with(NoticeButton::new(NoticeAction::Recreate, "Save"))
        } else {
            notice
        };
        notice.with(NoticeButton::new(NoticeAction::Close, "Close"))
    }

    /// Unsaved edits were rebuilt from the crash journal (not designed).
    pub fn recovered(disk: DiskComparison) -> Self {
        let detail = match disk {
            DiskComparison::Changed => {
                "Edits from the last session were restored. The file also changed on disk since then."
            }
            DiskComparison::Missing => {
                "Edits from the last session were restored. The file no longer exists on disk."
            }
            DiskComparison::MatchesBase | DiskComparison::NoFile | DiskComparison::Unreadable => {
                "Edits from the last session were restored."
            }
        };
        Self::new(
            Topic::Recovered,
            NoticeKind::Information,
            "Recovered unsaved changes.",
            Some(detail.to_owned()),
        )
        .with(NoticeButton::new(NoticeAction::KeepRecovered, "Keep"))
        .with(NoticeButton::new(NoticeAction::DiscardRecovered, "Discard"))
    }

    /// A sync client left a conflict copy next to the file (not designed).
    pub fn sync_conflict(copy: &Path, service: SyncService) -> Self {
        let name = copy.file_name().map_or_else(
            || copy.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let service = match service {
            SyncService::Dropbox => "Dropbox",
            SyncService::Syncthing => "Syncthing",
            SyncService::OneDrive => "OneDrive",
            SyncService::ICloud => "iCloud",
        };
        Self::new(
            Topic::SyncConflict,
            NoticeKind::Information,
            "A sync conflict copy appeared.",
            Some(format!(
                "{service} saved another version of this file as “{name}”."
            )),
        )
        .with(NoticeButton::new(
            NoticeAction::OpenCopy(copy.to_path_buf()),
            "Open copy",
        ))
    }

    /// Save on a document that has no file yet (not designed).
    pub fn untitled() -> Self {
        Self::new(
            Topic::Untitled,
            NoticeKind::Information,
            "Not saved yet.",
            Some(
                "Save as is not available yet. The text is kept in the recovery journal and comes back on the next start."
                    .to_owned(),
            ),
        )
    }

    /// The file on disk is no longer text (not designed).
    pub fn not_text() -> Self {
        Self::new(
            Topic::NotText,
            NoticeKind::Error,
            "Changed on disk.",
            Some("The file is no longer text, so it was not reloaded.".to_owned()),
        )
        .with(NoticeButton::new(NoticeAction::Close, "Close"))
    }
}
