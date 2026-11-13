// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The graphics side of starting up: what to tell the user when the GPU
//! cannot be used, which adapter is drawing, and safe mode.

use std::fmt::{self, Write as _};

use gpui::{AnyWindowHandle, App, GpuSpecs};

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
    error: String,
}

impl DebugInfo {
    pub fn new(error: &anyhow::Error) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            display: display_server(),
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

/// What to try, for this platform's renderer.
fn guidance() -> &'static str {
    if cfg!(target_os = "macos") {
        GUIDANCE_MACOS
    } else if cfg!(target_os = "windows") {
        GUIDANCE_WINDOWS
    } else {
        GUIDANCE_LINUX
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

/// The message shown when no window could be opened because the GPU could
/// not be used.
pub fn failure_message(error: &anyhow::Error) -> String {
    let mut message =
        String::from("glyphvane: cannot open a window: the graphics driver could not be used.\n\n");
    let _ = writeln!(message, "{}\n", guidance());
    let _ = write!(
        message,
        "Debug info (include it when you report the problem):\n{}",
        DebugInfo::new(error)
    );
    message
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

/// Safe mode asks for conservative graphics settings. GPUI has no API for
/// them in this version: the backend and the adapter are chosen inside its
/// renderers. The only levers are environment variables read when the
/// renderer starts, which the user can set for one run.
pub fn safe_mode_notice() -> String {
    let recipe = if cfg!(target_os = "windows") {
        "set GPUI_DISABLE_DIRECT_COMPOSITION=1 to draw without DirectComposition"
    } else if cfg!(target_os = "macos") {
        "macOS always draws with Metal; there is no alternative renderer"
    } else {
        "set VK_LOADER_DRIVERS_SELECT='*lvp*' and LIBGL_ALWAYS_SOFTWARE=1 to draw \
         with Mesa's software renderers"
    };
    format!(
        "glyphvane: --safe-mode cannot change the renderer in this version. \
         To start with conservative graphics settings, {recipe}."
    )
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
        let message = failure_message(&error);
        assert!(message.contains(guidance()));
        assert!(message.contains(env!("CARGO_PKG_VERSION")));
        assert!(message.contains(std::env::consts::OS));
        assert!(message.contains("Error: No GPU adapters found"));
    }
}
