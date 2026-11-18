// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the infobar says about a document's file (SPEC § Infobars).
//!
//! "Changed on disk with unsaved edits" (an error infobar with Compare,
//! Keep mine and Take theirs) and "Large-file mode" (a neutral infobar
//! with Details) are designed. The others are built from the same
//! component and flagged where they are made: save errors use the error
//! kind; recovery, a file deleted on disk, a sync-conflict copy and a
//! read-only file use the information kind; the question before closing
//! with unsaved changes uses the warning kind, like the other questions.

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
    /// A question about something the user asked for: revert, follow a
    /// link that leaves Glyphvane, open a file with another app.
    Confirm,
    /// A file or link was not opened (a program, a failed dialog).
    NotOpened,
    /// Whether to index a folder too large to index without asking
    /// (FR-VAULT-02).
    IndexAsk,
    /// Whether to save documents with unsaved changes before closing a
    /// tab or a window, or quitting.
    CloseUnsaved,
    /// The document is in large-file mode (FR-EDIT-16).
    LargeFile,
    /// The file may not be edited: its permissions, its size, or bytes
    /// its encoding cannot hold.
    ReadOnly,
    /// Remote images wait for approval (FR-SET-08).
    RemoteContent,
    /// A followed link matched several notes (FR-NAV-07).
    AmbiguousLink,
    /// A file operation of the Files panel was done or refused (rename,
    /// move, duplicate, trash, new folder).
    FileOp,
    /// The note holds Git conflict markers (plan 10 § Git awareness).
    ConflictMarkers,
}

/// What an infobar button does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeAction {
    /// Save again, converting the text to UTF-8.
    SaveAsUtf8,
    /// The compare and merge view: the changed file on disk, or the
    /// recovered text against the file.
    Compare,
    /// The compare and merge view against a sync-conflict copy.
    CompareCopy(PathBuf),
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
    /// Revert the document to the file on disk (confirmed).
    Revert,
    /// Hand a URL to the system (confirmed).
    OpenUrl(String),
    /// Open a file with the system's default app (confirmed).
    OpenExternally(PathBuf),
    /// Show a file in the system's file manager.
    Reveal(PathBuf),
    /// Index the window's folder although it is large.
    IndexAnyway,
    /// Index this smaller folder instead of the large one.
    IndexFolder(PathBuf),
    /// Close the question without doing anything.
    Cancel,
    /// Save the documents the close question lists, then close.
    SaveAndClose,
    /// Close without saving the documents the close question lists.
    DiscardAndClose,
    /// Say what large-file mode turns off.
    Details,
    /// Remote content: load the waiting images of this note, this session.
    RemoteLoadOnce,
    /// Remote content: add the waiting sites to the allow list.
    RemoteAlwaysAllow,
    /// Remote content: switch this vault to Allow.
    RemoteAllowVault,
    /// Remote content: add the waiting sites to the block list.
    RemoteBlock,
    /// An ambiguous link was followed: pick another of its candidates.
    ChooseLinkTarget,
    /// An ambiguous link was followed: rewrite it to the shortest form
    /// that names the note it opened.
    RewriteLink,
    /// Undo the last rename or move, with its link updates.
    UndoFileOp,
    /// Split the note at its conflict markers into the compare and merge
    /// view.
    ResolveConflictMarkers,
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
    fn new(action: NoticeAction, label: impl Into<SharedString>) -> Self {
        Self {
            action,
            label: label.into(),
            disabled: None,
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

    /// The designed infobar: remote content waiting for approval
    /// (FR-SET-08), for the sites in `hosts` (at least one). *Allow for
    /// this vault* is offered when a vault is open.
    pub fn remote_content(hosts: &[String], vault: bool) -> Self {
        let (title, many) = match hosts {
            [one] => (format!("This note references content from {one}."), false),
            _ => (
                format!("This note references content from {} sites.", hosts.len()),
                true,
            ),
        };
        let detail = if many {
            format!("Nothing was loaded from {}.", hosts.join(", "))
        } else {
            "Nothing was loaded.".to_owned()
        };
        let mut notice = Self {
            topic: Topic::RemoteContent,
            kind: NoticeKind::Information,
            title: title.into(),
            detail: Some(detail.into()),
            buttons: Vec::new(),
        }
        .with(NoticeButton::new(NoticeAction::RemoteLoadOnce, "Load once"))
        .with(NoticeButton::new(
            NoticeAction::RemoteAlwaysAllow,
            if many {
                "Always allow these sites"
            } else {
                "Always allow this site"
            },
        ));
        if vault {
            notice = notice.with(NoticeButton::new(
                NoticeAction::RemoteAllowVault,
                "Allow for this vault",
            ));
        }
        notice.with(NoticeButton::new(
            NoticeAction::RemoteBlock,
            if many {
                "Block these sites"
            } else {
                "Block this site"
            },
        ))
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
        .with(NoticeButton::new(NoticeAction::Compare, "Compare"))
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

    /// Unsaved edits were rebuilt from the crash journal (not designed):
    /// Restore keeps them, Compare shows them against the file, Discard
    /// goes back to the file.
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
        .with(NoticeButton::new(NoticeAction::KeepRecovered, "Restore"))
        .with(NoticeButton::new(NoticeAction::Compare, "Compare"))
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
            NoticeAction::CompareCopy(copy.to_path_buf()),
            "Compare",
        ))
        .with(NoticeButton::new(
            NoticeAction::OpenCopy(copy.to_path_buf()),
            "Open copy",
        ))
    }

    /// The designed large-file infobar (FR-EDIT-16): neutral, with
    /// Details.
    pub fn large_file() -> Self {
        Self::new(
            Topic::LargeFile,
            NoticeKind::Neutral,
            "Large-file mode.",
            Some(
                "Some features are reduced to keep this file fast. Editing works as usual."
                    .to_owned(),
            ),
        )
        .with(NoticeButton::new(NoticeAction::Details, "Details"))
    }

    /// The large-file infobar after Details: what is reduced (the
    /// wording is not designed).
    pub fn large_file_details() -> Self {
        Self::new(
            Topic::LargeFile,
            NoticeKind::Neutral,
            "Large-file mode.",
            Some(
                "Soft wrap starts off, code is highlighted and folds are found only where you look, and spell check is off. Editing works as usual."
                    .to_owned(),
            ),
        )
    }

    /// The file may not be edited (not designed): editing is off until
    /// the reason goes away. Permissions offer Save as; bytes the encoding
    /// cannot hold offer saving as UTF-8.
    pub fn read_only_file(reason: &gv_vault::files::ReadOnlyReason, encoding: Encoding) -> Self {
        use gv_vault::files::ReadOnlyReason;
        match reason {
            ReadOnlyReason::Permissions => Self::new(
                Topic::ReadOnly,
                NoticeKind::Information,
                "Read-only.",
                Some(
                    "You do not have permission to change this file, so editing is off. Save as writes a copy you can edit."
                        .to_owned(),
                ),
            )
            .with(NoticeButton::new(NoticeAction::SaveAs, "Save as…")),
            ReadOnlyReason::TooLarge { .. } => Self::new(
                Topic::ReadOnly,
                NoticeKind::Information,
                "Read-only.",
                Some("This file is too large to edit safely, so it opened read-only.".to_owned()),
            ),
            ReadOnlyReason::Lossy(_) => Self::new(
                Topic::ReadOnly,
                NoticeKind::Information,
                "Read-only.",
                Some(format!(
                    "The file is not valid {encoding}, so editing is off: saving it in {encoding} would change bytes you did not edit."
                )),
            )
            .with(NoticeButton::new(NoticeAction::SaveAsUtf8, "Save as UTF-8")),
        }
    }

    /// The question before closing a tab or a window, or quitting, with
    /// documents that are not saved on their own (untitled, autosave off
    /// or held back, a failed save). Not designed: a warning infobar like
    /// the other questions, naming every document.
    pub fn close_unsaved(names: &[String]) -> Self {
        let quoted: Vec<String> = names.iter().map(|n| format!("“{n}”")).collect();
        let (title, detail) = match quoted.as_slice() {
            [one] => (
                format!("Save changes to {one}?"),
                "Changes you do not save are lost.".to_owned(),
            ),
            _ => (
                format!("Save changes to {} documents?", quoted.len()),
                format!("{}. Changes you do not save are lost.", quoted.join(", ")),
            ),
        };
        Self::titled(
            Topic::CloseUnsaved,
            NoticeKind::Warning,
            title,
            Some(detail),
        )
        .with(NoticeButton::new(NoticeAction::SaveAndClose, "Save"))
        .with(NoticeButton::new(
            NoticeAction::DiscardAndClose,
            "Don't save",
        ))
        .with(NoticeButton::new(NoticeAction::Cancel, "Cancel"))
    }

    /// The compare view could not read the other version (not designed).
    pub fn compare_failed(reason: &str) -> Self {
        Self::titled(
            Topic::NotOpened,
            NoticeKind::Error,
            "Not compared.".to_owned(),
            Some(format!("The other version could not be read: {reason}.")),
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

    /// A notice with a title made at run time.
    fn titled(topic: Topic, kind: NoticeKind, title: String, detail: Option<String>) -> Self {
        Self {
            topic,
            kind,
            title: title.into(),
            detail: detail.map(Into::into),
            buttons: Vec::new(),
        }
    }

    /// Revert with unsaved edits asks first (not designed).
    pub fn confirm_revert() -> Self {
        Self::new(
            Topic::Confirm,
            NoticeKind::Warning,
            "Revert to the saved file?",
            Some(
                "Your unsaved edits are replaced by the file on disk. Undo brings them back."
                    .to_owned(),
            ),
        )
        .with(NoticeButton::new(NoticeAction::Revert, "Revert"))
        .with(NoticeButton::new(NoticeAction::Cancel, "Cancel"))
    }

    /// A link whose scheme needs confirmation: it is handed to another
    /// app, so the full URL is shown first (not designed).
    pub fn confirm_url(url: &str, why: &str) -> Self {
        Self::titled(
            Topic::Confirm,
            NoticeKind::Warning,
            "Open this link in another app?".to_owned(),
            Some(format!("{why} {}", shown_url(url))),
        )
        .with(NoticeButton::new(
            NoticeAction::OpenUrl(url.to_owned()),
            "Open",
        ))
        .with(NoticeButton::new(NoticeAction::Cancel, "Cancel"))
    }

    /// A file Glyphvane does not show itself: opening it hands it to the
    /// system's app for it, after confirmation (not designed). `html`
    /// names the browser.
    pub fn confirm_open_file(path: &Path, html: bool) -> Self {
        let name = file_name(path);
        let (title, detail) = if html {
            (
                format!("Open “{name}” in your browser?"),
                "Web pages open in the system's default browser, where their scripts run.",
            )
        } else {
            (
                format!("Open “{name}” with another app?"),
                "Glyphvane does not show this kind of file. It opens in the app your system uses for it.",
            )
        };
        Self::titled(
            Topic::Confirm,
            NoticeKind::Warning,
            title,
            Some(detail.to_owned()),
        )
        .with(NoticeButton::new(
            NoticeAction::OpenExternally(path.to_path_buf()),
            "Open",
        ))
        .with(NoticeButton::new(
            NoticeAction::Reveal(path.to_path_buf()),
            "Reveal in file manager",
        ))
        .with(NoticeButton::new(NoticeAction::Cancel, "Cancel"))
    }

    /// A program or launcher: never started, only revealed (not
    /// designed).
    pub fn executable(path: &Path) -> Self {
        Self::titled(
            Topic::NotOpened,
            NoticeKind::Information,
            format!("“{}” is a program.", file_name(path)),
            Some("Glyphvane never starts programs from notes or files.".to_owned()),
        )
        .with(NoticeButton::new(
            NoticeAction::Reveal(path.to_path_buf()),
            "Reveal in file manager",
        ))
    }

    /// The open dialog could not be shown (not designed).
    pub fn open_dialog_failed(reason: &str) -> Self {
        let mut detail = format!("The open dialog could not be shown: {reason}");
        if !detail.ends_with('.') {
            detail.push('.');
        }
        Self::new(
            Topic::NotOpened,
            NoticeKind::Error,
            "Not opened.",
            Some(detail),
        )
    }

    /// An HTML export could not be made or written (not designed).
    pub fn export_failed(reason: &str) -> Self {
        Self::titled(
            Topic::NotOpened,
            NoticeKind::Error,
            "Not exported.".to_owned(),
            Some(format!("The HTML file could not be written: {reason}.")),
        )
    }

    /// An export left `count` local images as links: missing, too large,
    /// outside the vault or not an image (not designed).
    pub fn export_images_skipped(count: usize) -> Self {
        let what = if count == 1 {
            "1 image was".to_owned()
        } else {
            format!("{count} images were")
        };
        Self::titled(
            Topic::NotOpened,
            NoticeKind::Information,
            "Exported.".to_owned(),
            Some(format!(
                "{what} not embedded (missing, too large, outside the vault or not an image)."
            )),
        )
    }

    /// *Copy as rich text* fell back to plain text: the clipboard's HTML
    /// format is not available, as on Wayland without the data-control
    /// protocol (not designed).
    pub fn rich_copy_plain(reason: &str) -> Self {
        Self::titled(
            Topic::NotOpened,
            NoticeKind::Information,
            "Copied as plain text.".to_owned(),
            Some(format!(
                "Rich text could not be put on the clipboard here ({reason})."
            )),
        )
    }

    /// The designed infobar after following an ambiguous link (FR-NAV-07):
    /// "*n* notes match *X*." with Choose… and Rewrite link. `rewrite` is
    /// why the link cannot be rewritten, if it cannot.
    pub fn ambiguous_link(count: usize, name: &str, opened: &str, rewrite: Option<String>) -> Self {
        let mut rewrite_button = NoticeButton::new(NoticeAction::RewriteLink, "Rewrite link");
        rewrite_button.disabled = rewrite.map(Into::into);
        Self::titled(
            Topic::AmbiguousLink,
            NoticeKind::Warning,
            format!("{count} notes match “{name}”."),
            Some(format!("This is “{opened}”.")),
        )
        .with(NoticeButton::new(NoticeAction::ChooseLinkTarget, "Choose…"))
        .with(rewrite_button)
    }

    /// A link was not followed: no note to create, the index still
    /// building (not designed).
    pub fn link_not_followed(title: String, detail: String) -> Self {
        Self::titled(
            Topic::NotOpened,
            NoticeKind::Information,
            title,
            Some(detail),
        )
    }

    /// A new note could not be created (not designed).
    pub fn new_note_failed(folder: &Path, reason: &str) -> Self {
        Self::titled(
            Topic::NotOpened,
            NoticeKind::Error,
            "No new note.".to_owned(),
            Some(format!(
                "A note could not be created in “{}”: {reason}.",
                folder.display()
            )),
        )
    }

    /// A file operation was done (not designed: an information infobar).
    /// `undo` offers Undo.
    pub fn file_op_done(title: String, detail: Option<String>, undo: bool) -> Self {
        let notice = Self::titled(Topic::FileOp, NoticeKind::Information, title, detail);
        if undo {
            notice.with(NoticeButton::new(NoticeAction::UndoFileOp, "Undo"))
        } else {
            notice
        }
    }

    /// The note holds Git conflict markers (plan 10: "files containing
    /// conflict markers get a warning banner"). Not designed: a warning
    /// infobar whose *Resolve…* opens the compare and merge view with the
    /// two sides; it cannot be used where the note may not be edited.
    pub fn conflict_markers(can_edit: bool) -> Self {
        let mut resolve = NoticeButton::new(NoticeAction::ResolveConflictMarkers, "Resolve…");
        if !can_edit {
            resolve.disabled = Some("This note cannot be edited here (viewer mode).".into());
        }
        Self::new(
            Topic::ConflictMarkers,
            NoticeKind::Warning,
            "This note has merge conflict markers.",
            Some("A merge left both versions of some lines in the text.".to_owned()),
        )
        .with(resolve)
    }

    /// A file operation was refused or failed (not designed: an error
    /// infobar).
    pub fn file_op_failed(title: &str, reason: &str) -> Self {
        Self::titled(
            Topic::FileOp,
            NoticeKind::Error,
            title.to_owned(),
            Some(sentence(reason)),
        )
    }

    /// Revert could not read the file (not designed).
    pub fn revert_failed(reason: &str) -> Self {
        Self::titled(
            Topic::NotOpened,
            NoticeKind::Error,
            "Not reverted.".to_owned(),
            Some(format!("The file could not be read: {reason}.")),
        )
    }

    /// The window's folder is too large to index without asking
    /// (FR-VAULT-02; the plan's wording, not designed as a screen).
    /// `size` describes it ("more than 50,000 files", "your home
    /// folder"); `smaller` is the folder "Only this folder" indexes.
    pub fn index_ask(root: &Path, size: &str, smaller: Option<&Path>) -> Self {
        let notice = Self::titled(
            Topic::IndexAsk,
            NoticeKind::Information,
            format!("Index {} ({size})?", shown_folder(root)),
            Some(
                "The file tree and links need an index of the folder. A folder this large takes a while to index."
                    .to_owned(),
            ),
        )
        .with(NoticeButton::new(NoticeAction::IndexAnyway, "Index"));
        let notice = match smaller {
            Some(folder) => notice.with(NoticeButton::new(
                NoticeAction::IndexFolder(folder.to_path_buf()),
                "Only this folder",
            )),
            None => notice,
        };
        notice.with(NoticeButton::new(NoticeAction::Cancel, "Cancel"))
    }
}

/// The last component of `path`, for messages.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The longest URL a message shows in full.
const URL_SHOWN_CHARS: usize = 200;

/// `url` for a message: control and text-direction characters shown
/// escaped (they could make the URL read as another), and a very long one
/// shortened in the middle.
fn shown_url(url: &str) -> String {
    let clean: String = url
        .chars()
        .flat_map(|c| {
            let direction = matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
            if c.is_control() || direction {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect();
    let count = clean.chars().count();
    if count <= URL_SHOWN_CHARS {
        return clean;
    }
    let half = URL_SHOWN_CHARS / 2;
    let head: String = clean.chars().take(half).collect();
    let tail: String = clean.chars().skip(count - half).collect();
    format!("{head}\u{2026}{tail}")
}

/// A folder for messages, the home folder written `~`, with a trailing
/// separator.
pub(crate) fn shown_folder(folder: &Path) -> String {
    let home = gv_vault::root::home_dir();
    let shown = match home.as_deref().and_then(|h| folder.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => folder.display().to_string(),
    };
    if shown.ends_with(std::path::MAIN_SEPARATOR) {
        shown
    } else {
        format!("{shown}{}", std::path::MAIN_SEPARATOR)
    }
}

/// `reason` as a sentence: a capital first letter and a full stop.
fn sentence(reason: &str) -> String {
    let mut chars = reason.chars();
    let mut out: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// Checks an infobar drawn in a window `width` wide: its detail
/// (`detail`) and every control in `controls` stay inside the window, the
/// detail never runs under a control, and it wraps once the window is 800
/// wide or less.
#[cfg(test)]
pub(crate) fn assert_clear_of_buttons(
    vcx: &mut gpui::VisualTestContext,
    width: f32,
    detail: &'static str,
    controls: &[&'static str],
) {
    use gpui::px;
    let edge = px(width);
    let text = vcx
        .debug_bounds(detail)
        .unwrap_or_else(|| panic!("{detail} drawn"));
    assert!(
        text.left() >= px(0.0) && text.right() <= edge,
        "{width}: {detail} {text:?} leaves the window"
    );
    for &id in controls {
        let control = vcx.debug_bounds(id).unwrap_or_else(|| panic!("{id} drawn"));
        assert!(
            !text.intersects(&control),
            "{width}: {detail} {text:?} runs under {id} {control:?}"
        );
        assert!(
            control.left() >= px(0.0) && control.right() <= edge,
            "{width}: {id} {control:?} leaves the window"
        );
    }
    if width <= 800.0 {
        assert!(
            text.size.height > px(crate::metrics::text::UI.line * 1.5),
            "{width}: {detail} {text:?} does not wrap"
        );
    }
}

#[cfg(test)]
mod shown_tests {
    use super::shown_url;

    #[test]
    fn urls_are_shown_safely() {
        assert_eq!(shown_url("ssh://host"), "ssh://host");
        assert_eq!(shown_url("tel:1\n2"), "tel:1\\n2");
        assert_eq!(shown_url("x:a\u{202e}b"), "x:a\\u{202e}b");
        let long = format!("ftp://{}", "a".repeat(400));
        let shown = shown_url(&long);
        assert_eq!(shown.chars().count(), 201);
        assert!(shown.starts_with("ftp://"));
    }
}
