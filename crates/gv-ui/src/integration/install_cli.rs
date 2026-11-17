// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! *Install glyphvane command…* on macOS (`app.install_cli`, plan 14 § CLI
//! and single instance): a symbolic link `/usr/local/bin/glyphvane` to the
//! executable inside `Glyphvane.app`, so `glyphvane` works in Terminal
//! (opening files, `--wait` for Git, `export`, `check-links`).
//!
//! 1. A dialog says what will be created (and what it replaces) and asks.
//!    Nothing happens without *Install*.
//! 2. The link is made directly when the folder is writable.
//! 3. Otherwise macOS's own administrator dialog asks for a password
//!    (`osascript … with administrator privileges`); the user sees it and
//!    may cancel it. Nothing ever runs with `sudo` behind the user's back.
//! 4. A dialog says what happened.
//!
//! A file at that path that is not a symbolic link is never replaced.
//! Windows' installer and the Linux packages put the command on the PATH
//! themselves, so the command exists on macOS only.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::{Path, PathBuf};

use gpui::App;

/// Where the link goes.
pub const LINK: &str = "/usr/local/bin/glyphvane";

/// What installing would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Nothing is at the path yet.
    Create,
    /// The link is there and points at this executable.
    AlreadyInstalled,
    /// A link to something else is there; it would be replaced.
    Replace { old: PathBuf },
    /// A file or folder that is not a link is there; it is left alone.
    Occupied,
}

/// What installing a link at `link` to `exe` would do.
pub fn plan(link: &Path, exe: &Path) -> Plan {
    let Ok(meta) = std::fs::symlink_metadata(link) else {
        return Plan::Create;
    };
    if !meta.file_type().is_symlink() {
        return Plan::Occupied;
    }
    match std::fs::read_link(link) {
        Ok(old) if old == exe => Plan::AlreadyInstalled,
        Ok(old) => {
            let resolved = std::fs::canonicalize(link).ok();
            if resolved.as_deref() == Some(exe) {
                Plan::AlreadyInstalled
            } else {
                Plan::Replace { old }
            }
        }
        Err(_) => Plan::Replace {
            old: PathBuf::new(),
        },
    }
}

/// Why the link could not be made directly.
#[derive(Debug, PartialEq, Eq)]
pub enum DirectError {
    /// The folder is not writable (or missing and its parent not writable):
    /// administrator rights are needed.
    NeedsAdmin,
    /// Something else failed.
    Failed(String),
}

/// Makes the link without asking for rights. Replaces an existing link,
/// never anything else.
#[cfg(unix)]
pub fn install_direct(link: &Path, exe: &Path) -> Result<(), DirectError> {
    use std::io::ErrorKind;
    let map = |err: std::io::Error| match err.kind() {
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => DirectError::NeedsAdmin,
        _ => DirectError::Failed(err.to_string()),
    };
    if let Some(dir) = link.parent()
        && !dir.is_dir()
    {
        std::fs::create_dir_all(dir).map_err(map)?;
    }
    match plan(link, exe) {
        Plan::AlreadyInstalled => return Ok(()),
        Plan::Occupied => {
            return Err(DirectError::Failed(format!(
                "{} exists and is not a link",
                link.display()
            )));
        }
        Plan::Replace { .. } => std::fs::remove_file(link).map_err(map)?,
        Plan::Create => {}
    }
    std::os::unix::fs::symlink(exe, link).map_err(map)
}

/// A string as a single-quoted shell word.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// A string as an AppleScript string literal.
pub fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The AppleScript that makes the link with administrator rights. macOS
/// shows its own password dialog for it.
pub fn admin_script(link: &Path, exe: &Path) -> String {
    let dir = link.parent().unwrap_or(Path::new("/usr/local/bin"));
    let command = format!(
        "mkdir -p {} && ln -sfn {} {}",
        shell_quote(&dir.to_string_lossy()),
        shell_quote(&exe.to_string_lossy()),
        shell_quote(&link.to_string_lossy()),
    );
    format!(
        "do shell script {} with prompt {} with administrator privileges",
        applescript_string(&command),
        applescript_string("Glyphvane wants to install the glyphvane command in /usr/local/bin."),
    )
}

/// Installs the handler on macOS. Called by [`super::init`].
pub(crate) fn init(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.on_action(|_: &crate::keymap::actions::app::InstallCli, cx| start(cx));
    let _ = cx;
}

/// The executable the link points at.
fn executable() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// `app.install_cli`: explains, asks, installs, reports.
#[cfg(target_os = "macos")]
pub fn start(cx: &mut App) {
    use gpui::PromptLevel;

    use crate::updates::prompt;

    let link = PathBuf::from(LINK);
    let Some(exe) = executable() else {
        prompt(
            cx,
            PromptLevel::Warning,
            "Cannot install the glyphvane command",
            Some("Glyphvane cannot tell where its program file is."),
            &["OK"],
            |_, _| {},
        );
        return;
    };
    let detail = match plan(&link, &exe) {
        Plan::AlreadyInstalled => {
            prompt(
                cx,
                PromptLevel::Info,
                "The glyphvane command is installed",
                Some(&format!("{LINK} opens {}.", exe.display())),
                &["OK"],
                |_, _| {},
            );
            return;
        }
        Plan::Occupied => {
            prompt(
                cx,
                PromptLevel::Warning,
                "Cannot install the glyphvane command",
                Some(&format!(
                    "{LINK} is a file Glyphvane did not create. Remove or rename it, then try again."
                )),
                &["OK"],
                |_, _| {},
            );
            return;
        }
        Plan::Create => format!(
            "This creates a link at {LINK} to {}, so you can type glyphvane in Terminal to open \
             files and folders, wait for edits (--wait) or export. macOS asks for an \
             administrator password if the folder needs one.",
            exe.display()
        ),
        Plan::Replace { old } => format!(
            "This replaces the link at {LINK} (to {}) with a link to {}. macOS asks for an \
             administrator password if the folder needs one.",
            old.display(),
            exe.display()
        ),
    };
    prompt(
        cx,
        PromptLevel::Info,
        "Install the glyphvane command?",
        Some(&detail),
        &["Install", "Cancel"],
        move |answer, cx| {
            if answer == 0 {
                install(link, exe, cx);
            }
        },
    );
}

#[cfg(target_os = "macos")]
fn install(link: PathBuf, exe: PathBuf, cx: &mut App) {
    use gpui::{AppContext as _, PromptLevel};

    use crate::updates::prompt;

    let task = cx.background_spawn(async move {
        match install_direct(&link, &exe) {
            Ok(()) => Ok(true),
            Err(DirectError::Failed(err)) => Err(err),
            Err(DirectError::NeedsAdmin) => {
                let output = std::process::Command::new("/usr/bin/osascript")
                    .arg("-e")
                    .arg(admin_script(&link, &exe))
                    .output()
                    .map_err(|e| e.to_string())?;
                if output.status.success() {
                    Ok(true)
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    // -128: the user cancelled the password dialog.
                    if stderr.contains("-128") {
                        Ok(false)
                    } else {
                        Err(stderr.trim().to_owned())
                    }
                }
            }
        }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(true) => prompt(
                cx,
                PromptLevel::Info,
                "The glyphvane command is installed",
                Some("Open a new Terminal window and type glyphvane --help."),
                &["OK"],
                |_, _| {},
            ),
            Ok(false) => {}
            Err(err) => prompt(
                cx,
                PromptLevel::Warning,
                "Cannot install the glyphvane command",
                Some(&err),
                &["OK"],
                |_, _| {},
            ),
        });
    })
    .detach();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn plans_and_direct_installs() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("Glyphvane.app/Contents/MacOS/glyphvane");
        std::fs::create_dir_all(exe.parent().expect("parent")).expect("dirs");
        std::fs::write(&exe, "").expect("exe");
        let link = dir.path().join("bin/glyphvane");
        assert_eq!(plan(&link, &exe), Plan::Create);
        install_direct(&link, &exe).expect("installed");
        assert_eq!(std::fs::read_link(&link).expect("a link"), exe);
        assert_eq!(plan(&link, &exe), Plan::AlreadyInstalled);
        // A link to an older copy is replaced.
        let other = dir.path().join("old");
        std::fs::remove_file(&link).expect("removed");
        std::os::unix::fs::symlink(&other, &link).expect("link");
        assert_eq!(plan(&link, &exe), Plan::Replace { old: other });
        install_direct(&link, &exe).expect("replaced");
        assert_eq!(plan(&link, &exe), Plan::AlreadyInstalled);
        // A real file is never touched.
        std::fs::remove_file(&link).expect("removed");
        std::fs::write(&link, "mine").expect("file");
        assert_eq!(plan(&link, &exe), Plan::Occupied);
        assert!(matches!(
            install_direct(&link, &exe),
            Err(DirectError::Failed(_))
        ));
        assert_eq!(std::fs::read_to_string(&link).expect("kept"), "mine");
    }

    #[test]
    fn a_read_only_folder_needs_the_administrator() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("temp dir");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("dir");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let result = install_direct(&bin.join("glyphvane"), Path::new("/x/glyphvane"));
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        // Root may write anyway (containers); then the link was made.
        if result.is_err() {
            assert_eq!(result, Err(DirectError::NeedsAdmin));
        }
    }

    #[test]
    fn the_admin_script_quotes_every_path() {
        let script = admin_script(
            Path::new("/usr/local/bin/glyphvane"),
            Path::new("/Applications/My \"Apps\"/it's/Glyphvane.app/Contents/MacOS/glyphvane"),
        );
        assert!(script.starts_with("do shell script \"mkdir -p '/usr/local/bin' && ln -sfn "));
        assert!(script.contains("'/Applications/My \\\"Apps\\\"/it'\\\\''s/Glyphvane.app"));
        assert!(script.ends_with("with administrator privileges"));
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(applescript_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
