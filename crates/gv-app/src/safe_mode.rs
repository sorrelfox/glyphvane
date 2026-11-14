// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Safe mode (`--safe-mode`): conservative graphics settings, the default
//! theme and no remote content, for a run that the normal settings or the
//! graphics driver break.
//!
//! GPUI chooses its renderer's backend and adapter itself and offers no
//! option for them. What steers it are environment variables that the
//! graphics stack reads when the renderer starts: the Vulkan loader and Mesa
//! on Linux, GPUI's own switch on Windows. Setting them inside this process
//! would need `unsafe`, so safe mode starts the program again with them set:
//!
//! - Linux and the other freedesktop systems: `VK_LOADER_DRIVERS_SELECT='*lvp*'`
//!   keeps only Mesa's software Vulkan driver (lavapipe), and
//!   `LIBGL_ALWAYS_SOFTWARE=1` makes the OpenGL fallback use llvmpipe;
//! - Windows: `GPUI_DISABLE_DIRECT_COMPOSITION=1` presents without
//!   DirectComposition;
//! - macOS draws with Metal only and has no such switch, so safe mode runs
//!   in the same process there.
//!
//! The new process gets the marker [`CHILD_MARKER`], which turns safe mode on
//! without starting again, so there is at most one restart. On Unix the
//! restart replaces the process (`exec`): the process id, standard streams
//! and exit status stay the same. On Windows the first process waits for the
//! second, sharing its standard streams, and exits with its status.
//!
//! Safe mode never hands its files to a running Glyphvane and never serves
//! later invocations: it starts on its own. A running instance would open
//! the files with its own renderer and settings, which is what the user is
//! trying to avoid.

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{Command, ExitCode};

/// Set in the environment of the process that safe mode starts. Safe mode is
/// on in a process that has it, with or without `--safe-mode`, and it never
/// starts again.
pub const CHILD_MARKER: &str = "GLYPHVANE_SAFE_MODE_CHILD";

/// The platforms, as far as safe mode is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Linux and the other freedesktop systems: wgpu over Vulkan or OpenGL.
    Linux,
    /// Windows: Direct3D 11.
    Windows,
    /// macOS: Metal.
    MacOs,
}

impl Platform {
    /// The platform this program was built for.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    /// The environment variables that give the renderer conservative
    /// settings. Empty where there are none.
    pub fn renderer_env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Linux => &[
                ("VK_LOADER_DRIVERS_SELECT", "*lvp*"),
                ("LIBGL_ALWAYS_SOFTWARE", "1"),
            ],
            Self::Windows => &[("GPUI_DISABLE_DIRECT_COMPOSITION", "1")],
            Self::MacOs => &[],
        }
    }

    /// What safe mode does to the renderer, for messages: "Glyphvane draws
    /// …". `None` where it changes nothing.
    pub fn safe_renderer(self) -> Option<&'static str> {
        match self {
            Self::Linux => Some("with Mesa's software renderers (lavapipe or llvmpipe)"),
            Self::Windows => Some("without DirectComposition"),
            Self::MacOs => None,
        }
    }
}

/// How this process starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Normally.
    Normal,
    /// In safe mode, in this process.
    Safe,
    /// Start again in safe mode, as described.
    Relaunch(Relaunch),
}

/// A restart in safe mode: the same program with these arguments and these
/// variables added to the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relaunch {
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

/// Whether `marker`, the value of [`CHILD_MARKER`], says that this process
/// was started by safe mode.
pub fn is_child(marker: Option<&OsStr>) -> bool {
    marker.is_some_and(|value| value == "1")
}

/// How to start: `flag` is `--safe-mode`, `marker` the value of
/// [`CHILD_MARKER`], `args` the command line without the program name.
pub fn start(platform: Platform, flag: bool, marker: Option<&OsStr>, args: &[OsString]) -> Start {
    if is_child(marker) {
        return Start::Safe;
    }
    if !flag {
        return Start::Normal;
    }
    match relaunch(platform, args) {
        Some(relaunch) => Start::Relaunch(relaunch),
        None => Start::Safe,
    }
}

/// The restart into safe mode with `args` (without the program name), or
/// `None` where safe mode does not change the renderer. The arguments are
/// passed on unchanged: the marker turns safe mode on, also when the restart
/// follows the user's choice in the no-GPU message rather than
/// `--safe-mode`.
pub fn relaunch(platform: Platform, args: &[OsString]) -> Option<Relaunch> {
    let vars = platform.renderer_env();
    if vars.is_empty() {
        return None;
    }
    let mut env: Vec<(OsString, OsString)> = vars
        .iter()
        .map(|&(name, value)| (name.into(), value.into()))
        .collect();
    env.push((CHILD_MARKER.into(), "1".into()));
    Some(Relaunch {
        args: args.to_vec(),
        env,
    })
}

/// Starts this program again as `relaunch` describes. On Unix this replaces
/// the process and returns only on failure; on Windows it waits for the new
/// process and returns its exit status.
pub fn run(relaunch: &Relaunch) -> io::Result<ExitCode> {
    let exe = std::env::current_exe()?;
    tracing::info!(
        "safe mode: starting again with {}",
        describe_env(&relaunch.env)
    );
    let mut command = Command::new(exe);
    command
        .args(&relaunch.args)
        .envs(relaunch.env.iter().map(|(name, value)| (name, value)));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        Err(command.exec())
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        Ok(ExitCode::from(exit_code(status.code())))
    }
}

/// The exit status to pass on for a child's exit code. Codes that do not fit
/// in a byte (Windows status values) and a missing code become 1.
#[cfg(any(not(unix), test))]
fn exit_code(code: Option<i32>) -> u8 {
    code.and_then(|code| u8::try_from(code).ok()).unwrap_or(1)
}

/// `NAME=value` pairs separated by spaces, for logs.
fn describe_env(env: &[(OsString, OsString)]) -> String {
    env.iter()
        .map(|(name, value)| format!("{}={}", name.to_string_lossy(), value.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Logs that safe mode is on and what it changes.
pub fn report(platform: Platform) {
    let renderer = match platform.safe_renderer() {
        Some(how) => {
            let set: Vec<(OsString, OsString)> = platform
                .renderer_env()
                .iter()
                .filter_map(|&(name, _)| std::env::var_os(name).map(|value| (name.into(), value)))
                .collect();
            if set.is_empty() {
                "renderer unchanged (its variables are not set)".to_owned()
            } else {
                format!("drawing {how} ({})", describe_env(&set))
            }
        }
        None => "renderer unchanged (Metal is the only one)".to_owned(),
    };
    tracing::warn!(
        "safe mode: default theme, remote content blocked, {renderer}; \
         running on its own, without the single-instance handoff"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn env(list: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        list.iter()
            .map(|&(name, value)| (name.into(), value.into()))
            .collect()
    }

    #[test]
    fn linux_restarts_with_software_renderers() {
        let given = args(&["--safe-mode", "--theme", "dark", "notes/a.md:3"]);
        let start = start(Platform::Linux, true, None, &given);
        assert_eq!(
            start,
            Start::Relaunch(Relaunch {
                args: given.clone(),
                env: env(&[
                    ("VK_LOADER_DRIVERS_SELECT", "*lvp*"),
                    ("LIBGL_ALWAYS_SOFTWARE", "1"),
                    (CHILD_MARKER, "1"),
                ]),
            })
        );
    }

    #[test]
    fn windows_restarts_without_direct_composition() {
        let given = args(&["--safe-mode", r"C:\notes\a.md"]);
        assert_eq!(
            start(Platform::Windows, true, None, &given),
            Start::Relaunch(Relaunch {
                args: given.clone(),
                env: env(&[
                    ("GPUI_DISABLE_DIRECT_COMPOSITION", "1"),
                    (CHILD_MARKER, "1")
                ]),
            })
        );
    }

    #[test]
    fn macos_stays_in_process() {
        let given = args(&["--safe-mode"]);
        assert_eq!(start(Platform::MacOs, true, None, &given), Start::Safe);
        assert_eq!(relaunch(Platform::MacOs, &given), None);
        assert_eq!(Platform::MacOs.safe_renderer(), None);
    }

    #[test]
    fn the_marker_prevents_a_second_restart() {
        let given = args(&["--safe-mode", "a.md"]);
        for platform in [Platform::Linux, Platform::Windows, Platform::MacOs] {
            // The restarted process: safe mode, no restart, with or without
            // the flag.
            assert_eq!(
                start(platform, true, Some(OsStr::new("1")), &given),
                Start::Safe
            );
            assert_eq!(
                start(platform, false, Some(OsStr::new("1")), &given),
                Start::Safe
            );
            // Without the flag and without a marker nothing changes; other
            // marker values are not the marker.
            assert_eq!(start(platform, false, None, &given), Start::Normal);
            assert_eq!(
                start(platform, false, Some(OsStr::new("0")), &given),
                Start::Normal
            );
        }
    }

    #[test]
    fn a_restart_after_consent_keeps_the_arguments() {
        // From the no-GPU message: no `--safe-mode` on the command line; the
        // marker alone turns safe mode on in the new process.
        let given = args(&["--", "--odd-name.md"]);
        let relaunch = relaunch(Platform::Linux, &given).expect("Linux restarts");
        assert_eq!(relaunch.args, given);
        assert!(
            relaunch
                .env
                .iter()
                .any(|(name, value)| name == CHILD_MARKER && is_child(Some(value)))
        );
    }

    #[test]
    fn exit_codes_pass_through() {
        assert_eq!(exit_code(Some(0)), 0);
        assert_eq!(exit_code(Some(1)), 1);
        assert_eq!(exit_code(Some(255)), 255);
        // An access violation's status on Windows, and a signal on Unix.
        assert_eq!(exit_code(Some(0xC000_0005_u32 as i32)), 1);
        assert_eq!(exit_code(None), 1);
    }
}
