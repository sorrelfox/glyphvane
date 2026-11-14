// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The graphics side of starting up: what to tell the user when the GPU
//! cannot be used, and which adapter is drawing.

use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::io;
use std::path::{Path, PathBuf};

use gpui::{AnyWindowHandle, App, GpuSpecs};

use crate::safe_mode::{CHILD_MARKER, Platform};

/// The file in the log folder that keeps the last GPU failure's message and
/// debug info.
pub const FAILURE_FILE: &str = "gpu-failure.txt";

/// Environment variables that change how the renderer picks its device,
/// quoted in the debug info when set.
const RENDERER_VARIABLES: &[&str] = &[
    "VK_LOADER_DRIVERS_SELECT",
    "VK_LOADER_DRIVERS_DISABLE",
    "VK_DRIVER_FILES",
    "VK_ICD_FILENAMES",
    "LIBGL_ALWAYS_SOFTWARE",
    "__EGL_VENDOR_LIBRARY_FILENAMES",
    "ZED_DEVICE_ID",
    "GPUI_DISABLE_DIRECT_COMPOSITION",
    CHILD_MARKER,
];

/// Words that the renderers of GPUI put into the errors they return when no
/// GPU device can be created: wgpu (Linux), Direct3D (Windows) and Metal
/// (macOS).
const GPU_ERROR_MARKERS: &[&str] = &[
    "gpu adapter",
    "graphics surface",
    "wgpu",
    "vulkan",
    "dxgi",
    "direct3d",
    "metal device",
    "failed to create surface",
];

/// Whether a failure to open a window came from the graphics stack.
pub fn is_gpu_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let text = cause.to_string().to_lowercase();
        GPU_ERROR_MARKERS.iter().any(|marker| text.contains(marker))
    })
}

/// The facts to quote in a bug report.
#[derive(Debug)]
pub struct DebugInfo {
    version: &'static str,
    os: &'static str,
    arch: &'static str,
    display: Option<&'static str>,
    safe_mode: bool,
    renderer_env: Vec<(&'static str, OsString)>,
    error: String,
}

impl DebugInfo {
    pub fn new(error: &anyhow::Error, safe_mode: bool) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            display: display_server(),
            safe_mode,
            renderer_env: RENDERER_VARIABLES
                .iter()
                .filter_map(|&name| std::env::var_os(name).map(|value| (name, value)))
                .collect(),
            error: format!("{error:#}"),
        }
    }
}

impl fmt::Display for DebugInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "  Glyphvane {}", self.version)?;
        writeln!(f, "  OS: {} {}", self.os, self.arch)?;
        if let Some(display) = self.display {
            writeln!(f, "  Display server: {display}")?;
        }
        writeln!(
            f,
            "  Safe mode: {}",
            if self.safe_mode { "on" } else { "off" }
        )?;
        for (name, value) in &self.renderer_env {
            writeln!(f, "  {name}={}", value.to_string_lossy())?;
        }
        write!(f, "  Error: {}", self.error)
    }
}

/// The display server a Linux session runs, as GPUI picks it: Wayland when
/// a Wayland socket is named, else X11.
fn display_server() -> Option<&'static str> {
    if !cfg!(any(target_os = "linux", target_os = "freebsd")) {
        return None;
    }
    let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    Some(if set("WAYLAND_DISPLAY") {
        "Wayland"
    } else if set("DISPLAY") {
        "X11"
    } else {
        "none found (neither WAYLAND_DISPLAY nor DISPLAY is set)"
    })
}

/// What to try, for the platform's renderer.
fn guidance_for(platform: Platform) -> &'static str {
    match platform {
        Platform::Linux => GUIDANCE_LINUX,
        Platform::Windows => GUIDANCE_WINDOWS,
        Platform::MacOs => GUIDANCE_MACOS,
    }
}

const GUIDANCE_LINUX: &str = "\
Glyphvane draws its windows with the GPU, through Vulkan or OpenGL ES 3.
- Install or update the Vulkan driver for your graphics card (with Mesa,
  your distribution's Mesa Vulkan drivers package).
- In a virtual machine or without a graphics card, install Mesa's software
  Vulkan driver (lavapipe) or software OpenGL driver (llvmpipe). Glyphvane
  uses them when no graphics card works.
- Start Glyphvane with RUST_LOG=info to see which adapters were tried.";

const GUIDANCE_WINDOWS: &str = "\
Glyphvane draws its windows with Direct3D 11 (feature level 10.1 or later).
- Install or update the driver for your graphics card.
- Without a usable graphics card, Windows normally provides a software
  renderer (Microsoft Basic Render Driver) that Glyphvane uses. If you see
  this message anyway, Direct3D is not available on this installation.";

const GUIDANCE_MACOS: &str = "\
Glyphvane draws its windows with Metal, which every Mac that runs macOS 13
or later supports. Restart the Mac; if the app runs in a virtual machine,
enable graphics acceleration for it.";

/// What went wrong, after "glyphvane: " or "Glyphvane ".
const PROBLEM: &str = "cannot open a window: the graphics driver could not be used.";

/// Why no window could be opened, and what to tell the user, when the GPU
/// could not be used.
#[derive(Debug)]
pub struct Failure {
    platform: Platform,
    safe_mode: bool,
    debug: DebugInfo,
}

impl Failure {
    /// The failure `error`, in a process where safe mode is on or off.
    pub fn new(error: &anyhow::Error, platform: Platform, safe_mode: bool) -> Self {
        Self {
            platform,
            safe_mode,
            debug: DebugInfo::new(error, safe_mode),
        }
    }

    /// Whether to offer a restart in safe mode: only where safe mode changes
    /// the renderer, and not when it is on already.
    pub fn offers_safe_mode(&self) -> bool {
        !self.safe_mode && self.platform.safe_renderer().is_some()
    }

    /// What safe mode did or can do, as a paragraph, or `None` where it
    /// changes nothing. `terminal` wraps it for standard error, where the
    /// restart is the `--safe-mode` option rather than a button.
    fn safe_mode_advice(&self, terminal: bool) -> Option<String> {
        let how = self.platform.safe_renderer()?;
        Some(match (self.safe_mode, terminal) {
            (true, true) => format!(
                "Safe mode is on: Glyphvane tried to draw {how},\nand that did not work either."
            ),
            (true, false) => {
                format!(
                    "Safe mode is on: Glyphvane tried to draw {how}, and that did not work either."
                )
            }
            (false, true) => format!(
                "To try conservative graphics settings, start Glyphvane with --safe-mode.\n\
                 Safe mode draws {how}."
            ),
            (false, false) => {
                format!("You can start Glyphvane again in safe mode, which draws {how}.")
            }
        })
    }

    /// The debug info block, indented.
    pub fn debug_info(&self) -> String {
        self.debug.to_string()
    }

    /// The message for standard error and the failure file. `saved` is where
    /// the failure file was written.
    pub fn terminal_message(&self, saved: Option<&Path>) -> String {
        let mut message = format!("glyphvane: {PROBLEM}\n\n");
        let _ = writeln!(message, "{}\n", guidance_for(self.platform));
        if let Some(advice) = self.safe_mode_advice(true) {
            let _ = writeln!(message, "{advice}\n");
        }
        let _ = write!(
            message,
            "Debug info (include it when you report the problem):\n{}",
            self.debug
        );
        if let Some(path) = saved {
            let _ = write!(message, "\n\nThis message is saved in {}", path.display());
        }
        message
    }

    /// The text of the native message box. `saved` is where the failure
    /// file was written.
    pub fn dialog_text(&self, saved: Option<&Path>) -> String {
        let mut text = format!("Glyphvane {PROBLEM}\n\n");
        // The guidance is wrapped for a terminal; a message box wraps itself.
        text.push_str(&guidance_for(self.platform).replace("\n  ", " "));
        if let Some(advice) = self.safe_mode_advice(false) {
            let _ = write!(text, "\n\n{advice}");
        }
        if let Some(path) = saved {
            let _ = write!(
                text,
                "\n\nThe debug info for a bug report is saved in {}",
                path.display()
            );
        }
        text
    }

    /// Writes the terminal message to [`FAILURE_FILE`] in `dir`, creating
    /// the folder, and returns the file's path.
    pub fn save(&self, dir: &Path) -> io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(FAILURE_FILE);
        let mut text = self.terminal_message(None);
        text.push('\n');
        std::fs::write(&path, text)?;
        Ok(path)
    }
}

/// Logs which adapter draws the window, and warns when it is a software
/// renderer: it works, but costs CPU time on every frame.
pub fn report_adapter(window: AnyWindowHandle, cx: &mut App) {
    let specs = window
        .update(cx, |_, window, _| window.gpu_specs())
        .ok()
        .flatten();
    match specs {
        Some(specs) => log_adapter(&specs),
        None => tracing::info!("the renderer did not report its GPU"),
    }
}

fn log_adapter(specs: &GpuSpecs) {
    tracing::info!(
        device = %specs.device_name,
        driver = %specs.driver_name,
        driver_info = %specs.driver_info,
        software = specs.is_software_emulated,
        "GPU adapter"
    );
    if specs.is_software_emulated {
        tracing::warn!(
            "drawing with a software renderer ({}); expect high CPU use. \
             Install or update the graphics driver to draw with the GPU.",
            specs.device_name
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_errors_are_recognised() {
        let errors = [
            // wgpu (Linux)
            anyhow::anyhow!("No GPU adapters found"),
            anyhow::anyhow!("No GPU adapter found that can configure the display surface"),
            anyhow::anyhow!("Failed to create surface: no backend"),
            // Direct3D (Windows)
            anyhow::anyhow!("E_FAIL").context("Getting DXGI adapter"),
            // Metal (macOS)
            anyhow::anyhow!("none").context("accessing a compatible Metal device"),
        ];
        for error in errors {
            assert!(is_gpu_error(&error), "{error:#}");
        }
        assert!(!is_gpu_error(&anyhow::anyhow!("no such file")));
    }

    #[test]
    fn failure_message_carries_guidance_and_debug_info() {
        let error = anyhow::anyhow!("No GPU adapters found");
        let failure = Failure::new(&error, Platform::Linux, false);
        let message = failure.terminal_message(Some(Path::new("/logs/gpu-failure.txt")));
        assert!(message.starts_with("glyphvane: cannot open a window"));
        assert!(message.contains(GUIDANCE_LINUX));
        assert!(message.contains("start Glyphvane with --safe-mode"));
        assert!(
            message
                .lines()
                .all(|line| line.chars().count() <= 80 || line.contains("Error:"))
        );
        assert!(message.contains(env!("CARGO_PKG_VERSION")));
        assert!(message.contains(std::env::consts::OS));
        assert!(message.contains("Safe mode: off"));
        assert!(message.contains("Error: No GPU adapters found"));
        assert!(message.ends_with("saved in /logs/gpu-failure.txt"));
    }

    #[test]
    fn safe_mode_is_offered_once_and_where_it_helps() {
        let error = anyhow::anyhow!("No GPU adapters found");
        let linux = Failure::new(&error, Platform::Linux, false);
        assert!(linux.offers_safe_mode());
        assert!(linux.dialog_text(None).contains("again in safe mode"));

        // Already in safe mode: no second offer, and the message says what
        // was tried.
        let again = Failure::new(&error, Platform::Linux, true);
        assert!(!again.offers_safe_mode());
        let text = again.dialog_text(None);
        assert!(text.contains("Safe mode is on"));
        assert!(text.contains("software renderers"));
        assert!(!again.terminal_message(None).contains("--safe-mode"));
        assert!(again.debug_info().contains("Safe mode: on"));

        assert!(Failure::new(&error, Platform::Windows, false).offers_safe_mode());
        // macOS has no other renderer.
        let mac = Failure::new(&error, Platform::MacOs, false);
        assert!(!mac.offers_safe_mode());
        assert!(!mac.dialog_text(None).contains("safe mode"));
    }

    #[test]
    fn the_failure_is_saved_in_the_log_folder() {
        let dir = tempfile::tempdir().expect("temporary folder");
        let logs = dir.path().join("logs");
        let failure = Failure::new(
            &anyhow::anyhow!("Failed to create surface"),
            Platform::Linux,
            false,
        );
        let path = failure.save(&logs).expect("saved");
        assert_eq!(path, logs.join(FAILURE_FILE));
        let text = std::fs::read_to_string(&path).expect("readable");
        assert!(text.contains("Error: Failed to create surface"));
        assert!(text.contains(GUIDANCE_LINUX));
    }
}
