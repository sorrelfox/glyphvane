// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Handing files and URLs to the operating system: *Reveal in file
//! manager*, *Open with default app* and external links (FR-PLAT-04,
//! FR-NAV-08, FR-NAV-09).
//!
//! The work goes to a [`Launcher`], by default the platform's through
//! GPUI:
//!
//! | | Linux | macOS | Windows |
//! |---|---|---|---|
//! | Reveal | the desktop portal's `OpenDirectory` (the file manager opens the folder with the file selected), else `xdg-open` on the folder | `activateFileViewerSelecting` | `explorer /select,` |
//! | Open | `xdg-open` | `NSWorkspace` open | `ShellExecuteW` |
//! | URL | `xdg-open` | `NSWorkspace` open | `ShellExecuteW` |
//!
//! Every call returns at once; the system does the rest off the UI thread.
//! Tests install their own launcher with [`set_launcher`]. In unit tests
//! the default launcher launches nothing, so no test can start a program by
//! mistake (URLs go to the test platform, which only records them).
//!
//! [`classify`] says what may be launched at all: executables and
//! launchers never are (plan § Special targets).

use std::path::Path;
use std::rc::Rc;

use gpui::{App, Global};

/// Hands files and URLs to the operating system.
pub trait Launcher {
    /// Shows `path` selected in the system's file manager.
    fn reveal(&self, path: &Path, cx: &mut App);
    /// Opens `path` with the application the system associates with it.
    fn open_with_default_app(&self, path: &Path, cx: &mut App);
    /// Opens `url` with the system's handler for its scheme.
    fn open_url(&self, url: &str, cx: &mut App);
}

/// The platform's launcher, through GPUI.
#[cfg_attr(test, allow(dead_code))]
struct SystemLauncher;

impl Launcher for SystemLauncher {
    fn reveal(&self, path: &Path, cx: &mut App) {
        cx.reveal_path(path);
    }

    fn open_with_default_app(&self, path: &Path, cx: &mut App) {
        cx.open_with_system(path);
    }

    fn open_url(&self, url: &str, cx: &mut App) {
        cx.open_url(url);
    }
}

/// The launcher of unit tests that installed none: it launches nothing.
#[cfg(test)]
struct InertLauncher;

#[cfg(test)]
impl Launcher for InertLauncher {
    fn reveal(&self, path: &Path, _: &mut App) {
        tracing::debug!("test: not revealing {}", path.display());
    }

    fn open_with_default_app(&self, path: &Path, _: &mut App) {
        tracing::debug!("test: not opening {}", path.display());
    }

    fn open_url(&self, url: &str, cx: &mut App) {
        // The test platform only records the URL.
        cx.open_url(url);
    }
}

/// The launcher in use, when not the platform's.
struct InstalledLauncher(Rc<dyn Launcher>);

impl Global for InstalledLauncher {}

/// Uses `launcher` instead of the platform's (tests).
pub fn set_launcher(cx: &mut App, launcher: impl Launcher + 'static) {
    cx.set_global(InstalledLauncher(Rc::new(launcher)));
}

/// The launcher: the installed one, or the platform's.
pub(crate) fn launcher(cx: &App) -> Rc<dyn Launcher> {
    match cx.try_global::<InstalledLauncher>() {
        Some(installed) => installed.0.clone(),
        #[cfg(not(test))]
        None => Rc::new(SystemLauncher),
        #[cfg(test)]
        None => Rc::new(InertLauncher),
    }
}

/// What a file is, for opening it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    /// A Markdown file: opens in a tab.
    Markdown,
    /// An HTML page: opens in the default browser after confirmation.
    Html,
    /// A program or something that starts one: never launched, only
    /// revealed in the file manager.
    Executable,
    /// Anything else: opens with the default app after confirmation (the
    /// built-in viewers come later).
    Other,
}

/// Extensions of programs and launchers, never opened by Glyphvane.
const LAUNCHER_EXTENSIONS: &[&str] = &[
    "exe", "bat", "cmd", "com", "msi", "ps1", "vbs", "js", "jar", "app", "command", "desktop",
    "lnk", "url", "webloc", "appimage", "deb", "rpm", "pkg", "dmg", "scr", "msix", "appx",
];

/// Script extensions that are launchers only with the executable bit.
const SCRIPT_EXTENSIONS: &[&str] = &["sh", "bash", "zsh", "py"];

/// File extensions opened in the Reading view.
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdown", "mkd", "mkdn", "mdwn", "mdx"];

/// Whether `path` names a Markdown file, which opens in a tab.
pub fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        MARKDOWN_EXTENSIONS
            .iter()
            .any(|m| m.eq_ignore_ascii_case(e))
    })
}

/// What `path` is, by its name (and, for scripts, its executable bit).
pub fn classify(path: &Path) -> FileClass {
    if is_markdown(path) {
        return FileClass::Markdown;
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let extension = extension.as_deref().unwrap_or("");
    if matches!(extension, "html" | "htm") {
        return FileClass::Html;
    }
    if LAUNCHER_EXTENSIONS.contains(&extension) || path.is_dir() && extension == "app" {
        return FileClass::Executable;
    }
    if (SCRIPT_EXTENSIONS.contains(&extension) || extension.is_empty()) && is_executable(path) {
        return FileClass::Executable;
    }
    FileClass::Other
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    // Windows has no executable bit: the extensions above decide.
    let _ = path;
    false
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{FileClass, classify};

    #[test]
    fn files_are_classified_by_name() {
        let cases = [
            ("notes/a.md", FileClass::Markdown),
            ("notes/a.MARKDOWN", FileClass::Markdown),
            ("site/index.html", FileClass::Html),
            ("site/old.HTM", FileClass::Html),
            ("setup.exe", FileClass::Executable),
            ("run.BAT", FileClass::Executable),
            ("tool.AppImage", FileClass::Executable),
            ("app.desktop", FileClass::Executable),
            ("link.lnk", FileClass::Executable),
            ("script.js", FileClass::Executable),
            ("report.pdf", FileClass::Other),
            ("photo.png", FileClass::Other),
            ("data.zip", FileClass::Other),
            ("README", FileClass::Other),
            // Without the executable bit (the file does not exist), a
            // script is just text.
            ("build.sh", FileClass::Other),
        ];
        for (path, class) in cases {
            assert_eq!(classify(Path::new(path)), class, "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_script_with_the_executable_bit_is_never_launched() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("temp dir");
        for name in ["build.sh", "tool"] {
            let path = dir.path().join(name);
            std::fs::write(&path, "#!/bin/sh\n").expect("write");
            assert_eq!(classify(&path), FileClass::Other, "{name} without +x");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            assert_eq!(classify(&path), FileClass::Executable, "{name} with +x");
        }
    }
}
