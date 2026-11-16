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
    NotText,
    /// "Explain detection" from the profile chip.
    ProfileExplained,
    /// The document looks like another dialect (FR-PROF-05).
    DialectHint,
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
    /// Save the text to a path chosen in the save dialog.
    SaveAs,
    /// Close the tab.
    Close,
    /// Open a sync-conflict copy in a new tab.
    OpenCopy(PathBuf),
    /// Open the profile chip's menu.
    SwitchProfile,
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

    /// A save the file's permissions refused: Save As can write the text
    /// elsewhere (not designed).
    pub fn read_only(detail: String) -> Self {
        Self::save_failed(detail).with(NoticeButton::new(NoticeAction::SaveAs, "Save as…"))
    }

    /// Save As chose a name that `.md` was added to, and a file with that
    /// name exists; the dialog did not ask about replacing it (not
    /// designed).
    pub fn save_as_exists(name: &str) -> Self {
        Self::save_failed(format!(
            "A file named “{name}” already exists. To replace it, choose it in the save dialog."
        ))
        .with(NoticeButton::new(NoticeAction::SaveAs, "Save as…"))
    }

    /// Save As chose a file another tab edits (not designed).
    pub fn save_as_open_elsewhere(name: &str) -> Self {
        Self::save_failed(format!(
            "“{name}” is open in another tab. Close it there first, or choose another name."
        ))
        .with(NoticeButton::new(NoticeAction::SaveAs, "Save as…"))
    }

    /// The save dialog could not be shown (not designed).
    pub fn save_dialog_failed(reason: &str) -> Self {
        let mut detail = format!("The save dialog could not be opened: {reason}");
        if !detail.ends_with('.') {
            detail.push('.');
        }
        Self::save_failed(detail)
    }

    /// The designed hint infobar: the document looks like another dialect
    /// (FR-PROF-05). `message` is "This looks like X. Switch profile? …":
    /// its first sentence is the title.
    pub fn dialect_hint(message: &str) -> Self {
        let (title, detail) = match message.split_once(". ") {
            Some((title, rest)) => (format!("{title}."), Some(rest.to_owned())),
            None => (message.to_owned(), None),
        };
        Self {
            topic: Topic::DialectHint,
            kind: NoticeKind::Information,
            title: title.into(),
            detail: detail.map(Into::into),
            buttons: vec![NoticeButton::new(
                NoticeAction::SwitchProfile,
                "Switch profile",
            )],
        }
    }

    /// "Explain detection" from the profile chip's menu (not designed: an
    /// information infobar). `lines` come from the detection: the profile
    /// first, then the reason and notes.
    pub fn profile_explained(lines: &[String]) -> Self {
        let mut lines = lines.iter();
        let title = lines.next().map_or_else(String::new, |l| format!("{l}."));
        let rest: Vec<&str> = lines.map(String::as_str).collect();
        Self {
            topic: Topic::ProfileExplained,
            kind: NoticeKind::Information,
            title: title.into(),
            detail: (!rest.is_empty()).then(|| rest.join(" ").into()),
            buttons: vec![NoticeButton::new(
                NoticeAction::SwitchProfile,
                "Switch profile",
            )],
        }
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
            notice
                .with(NoticeButton::new(NoticeAction::Recreate, "Save"))
                .with(NoticeButton::new(NoticeAction::SaveAs, "Save as…"))
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
