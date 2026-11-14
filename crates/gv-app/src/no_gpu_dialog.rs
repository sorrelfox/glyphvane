// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The message box shown when the GPU cannot be used. There is no window to
//! draw a GPUI prompt in, so the platform shows it:
//!
//! - Linux and the other freedesktop systems: `zenity` (GNOME and most
//!   desktops) or `kdialog` (KDE, tried first on KDE Plasma), started as a
//!   separate program. A *Show Debug Info* button opens the debug info as
//!   text to select and copy: this process cannot keep a clipboard selection
//!   alive once it exits, and on Wayland it cannot take one without a
//!   window. When neither program runs, the message is only on standard
//!   error and in the failure file.
//! - Windows: a system message box (`MessageBoxW`, through `rfd`). Its text
//!   carries the debug info, and Ctrl+C copies the whole message, as in
//!   every Windows message box.
//! - macOS: an alert (`NSAlert`, through `rfd`) with a *Copy Debug Info*
//!   button that puts the debug info on the pasteboard.
//!
//! The message offers a restart in safe mode only when the caller says so;
//! nothing restarts without the user's choice.

use std::path::Path;

use gpui::App;

/// What the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Close the message; the program exits.
    Close,
    /// Start again in safe mode.
    SafeMode,
}

/// What the message box says.
#[derive(Debug)]
pub struct Message<'a> {
    /// The message: the problem, the guidance and where the debug info is
    /// saved.
    pub text: &'a str,
    /// The debug info block.
    pub debug_info: &'a str,
    /// The file the message and debug info were saved in, if any. The text
    /// names it already; only kdialog shows the file itself.
    #[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
    pub debug_file: Option<&'a Path>,
    /// Whether to offer a restart in safe mode.
    pub offer_safe_mode: bool,
}

const TITLE: &str = "Glyphvane";
#[cfg_attr(windows, allow(dead_code))]
const SAFE_MODE_LABEL: &str = "Start in Safe Mode";
#[cfg_attr(windows, allow(dead_code))]
const CLOSE_LABEL: &str = "Close";
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
const SHOW_DEBUG_LABEL: &str = "Show Debug Info";
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const COPY_DEBUG_LABEL: &str = "Copy Debug Info";

/// The debug info as copied or shown on its own.
#[cfg_attr(windows, allow(dead_code))]
fn debug_text(message: &Message<'_>) -> String {
    format!(
        "Glyphvane debug info (include it when you report the problem):\n{}\n",
        message.debug_info
    )
}

/// Shows `message` and waits for the user. `None` when no message box could
/// be shown.
pub fn show(message: &Message<'_>, cx: &mut App) -> Option<Choice> {
    #[cfg(target_os = "macos")]
    {
        macos::show(message, cx)
    }
    #[cfg(windows)]
    {
        let _ = cx;
        windows::show(message)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = cx;
        desktop::show(message)
    }
}

#[cfg(windows)]
mod windows {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

    use super::{Choice, Message, TITLE};

    pub fn show(message: &Message<'_>) -> Option<Choice> {
        let mut text = format!(
            "{}\n\nDebug info:\n{}\n\nPress Ctrl+C to copy this message.",
            message.text, message.debug_info
        );
        let buttons = if message.offer_safe_mode {
            text.push_str("\n\nStart Glyphvane again in safe mode?");
            MessageButtons::YesNo
        } else {
            MessageButtons::Ok
        };
        let result = MessageDialog::new()
            .set_level(MessageLevel::Error)
            .set_title(TITLE)
            .set_description(text)
            .set_buttons(buttons)
            .show();
        Some(
            if message.offer_safe_mode && result == MessageDialogResult::Yes {
                Choice::SafeMode
            } else {
                Choice::Close
            },
        )
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use gpui::{App, ClipboardItem};
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

    use super::{
        CLOSE_LABEL, COPY_DEBUG_LABEL, Choice, Message, SAFE_MODE_LABEL, TITLE, debug_text,
    };

    pub fn show(message: &Message<'_>, cx: &mut App) -> Option<Choice> {
        let buttons = if message.offer_safe_mode {
            MessageButtons::YesNoCancelCustom(
                SAFE_MODE_LABEL.to_owned(),
                COPY_DEBUG_LABEL.to_owned(),
                CLOSE_LABEL.to_owned(),
            )
        } else {
            MessageButtons::OkCancelCustom(CLOSE_LABEL.to_owned(), COPY_DEBUG_LABEL.to_owned())
        };
        let result = MessageDialog::new()
            .set_level(MessageLevel::Error)
            .set_title(TITLE)
            .set_description(message.text)
            .set_buttons(buttons)
            .show();
        match result {
            MessageDialogResult::Custom(label) if label == SAFE_MODE_LABEL => {
                Some(Choice::SafeMode)
            }
            MessageDialogResult::Custom(label) if label == COPY_DEBUG_LABEL => {
                cx.write_to_clipboard(ClipboardItem::new_string(debug_text(message)));
                Some(Choice::Close)
            }
            _ => Some(Choice::Close),
        }
    }
}

/// zenity and kdialog. Built on every platform for the tests.
#[cfg(any(test, not(any(windows, target_os = "macos"))))]
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
mod desktop {
    use std::ffi::{OsStr, OsString};
    use std::io::Write as _;
    use std::process::{Command, Output, Stdio};

    use super::{
        CLOSE_LABEL, Choice, Message, SAFE_MODE_LABEL, SHOW_DEBUG_LABEL, TITLE, debug_text,
    };

    /// How often the message comes back after the debug info was shown.
    const MAX_ROUNDS: usize = 8;
    /// Wide enough for the guidance lines without wrapping them twice.
    const WIDTH: &str = "560";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Tool {
        Zenity,
        Kdialog,
    }

    impl Tool {
        fn program(self) -> &'static str {
            match self {
                Self::Zenity => "zenity",
                Self::Kdialog => "kdialog",
            }
        }
    }

    /// What the user did in the message.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Answer {
        Choice(Choice),
        ShowDebugInfo,
    }

    /// The tools to try, in order: kdialog first on KDE Plasma
    /// (`XDG_CURRENT_DESKTOP`), zenity first elsewhere.
    pub fn tools(desktop: Option<&OsStr>) -> [Tool; 2] {
        let kde = desktop.is_some_and(|names| {
            names
                .to_string_lossy()
                .split(':')
                .any(|name| name.eq_ignore_ascii_case("kde"))
        });
        if kde {
            [Tool::Kdialog, Tool::Zenity]
        } else {
            [Tool::Zenity, Tool::Kdialog]
        }
    }

    /// The arguments that show the message.
    pub fn message_args(tool: Tool, message: &Message<'_>) -> Vec<OsString> {
        let args: Vec<&str> = match (tool, message.offer_safe_mode) {
            (Tool::Zenity, true) => vec![
                "--question",
                "--no-markup",
                "--title",
                TITLE,
                "--width",
                WIDTH,
                "--text",
                message.text,
                "--ok-label",
                SAFE_MODE_LABEL,
                "--cancel-label",
                CLOSE_LABEL,
                "--extra-button",
                SHOW_DEBUG_LABEL,
            ],
            (Tool::Zenity, false) => vec![
                "--error",
                "--no-markup",
                "--title",
                TITLE,
                "--width",
                WIDTH,
                "--text",
                message.text,
                "--ok-label",
                CLOSE_LABEL,
                "--extra-button",
                SHOW_DEBUG_LABEL,
            ],
            (Tool::Kdialog, true) => vec![
                "--title",
                TITLE,
                "--warningyesnocancel",
                message.text,
                "--yes-label",
                SAFE_MODE_LABEL,
                "--no-label",
                SHOW_DEBUG_LABEL,
                "--cancel-label",
                CLOSE_LABEL,
            ],
            // The details expand below the message, as selectable text.
            (Tool::Kdialog, false) => vec![
                "--title",
                TITLE,
                "--detailederror",
                message.text,
                message.debug_info,
            ],
        };
        args.into_iter().map(OsString::from).collect()
    }

    /// The user's answer from the tool's exit code and output, or `None`
    /// when the tool did not show the message.
    pub fn answer(
        tool: Tool,
        offer_safe_mode: bool,
        code: Option<i32>,
        stdout: &str,
        stderr: &str,
    ) -> Option<Answer> {
        let close = Some(Answer::Choice(Choice::Close));
        match (tool, code?) {
            // zenity also exits with 1 when it cannot reach the display.
            (Tool::Zenity, 1) if stderr.contains("open display") => None,
            (Tool::Zenity, 0) if offer_safe_mode => Some(Answer::Choice(Choice::SafeMode)),
            (Tool::Zenity, 1) if stdout.trim_end() == SHOW_DEBUG_LABEL => {
                Some(Answer::ShowDebugInfo)
            }
            // OK, Cancel, the window's close button, or a timeout.
            (Tool::Zenity, 0 | 1 | 5) => close,
            (Tool::Kdialog, 0) if offer_safe_mode => Some(Answer::Choice(Choice::SafeMode)),
            (Tool::Kdialog, 1) if offer_safe_mode => Some(Answer::ShowDebugInfo),
            (Tool::Kdialog, 0..=2) => close,
            _ => None,
        }
    }

    /// The arguments that show the debug info on its own, and the text to
    /// pass on standard input.
    pub fn debug_args(tool: Tool, message: &Message<'_>) -> (Vec<OsString>, Option<String>) {
        let title = format!("{TITLE} debug info");
        match (tool, message.debug_file) {
            (Tool::Zenity, _) => (
                [
                    "--text-info",
                    "--title",
                    title.as_str(),
                    "--width",
                    "640",
                    "--height",
                    "360",
                ]
                .into_iter()
                .map(OsString::from)
                .collect(),
                Some(debug_text(message)),
            ),
            (Tool::Kdialog, Some(file)) => (
                vec![
                    "--title".into(),
                    title.into(),
                    "--textbox".into(),
                    file.into(),
                    "640".into(),
                    "360".into(),
                ],
                None,
            ),
            (Tool::Kdialog, None) => (
                vec![
                    "--title".into(),
                    title.into(),
                    "--msgbox".into(),
                    debug_text(message).into(),
                ],
                None,
            ),
        }
    }

    fn run(tool: Tool, args: &[OsString], input: Option<&str>) -> std::io::Result<Output> {
        let mut child = Command::new(tool.program())
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
            // The text is small; a tool that closes its input early only
            // loses the rest of it.
            let _ = stdin.write_all(text.as_bytes());
        }
        child.wait_with_output()
    }

    /// Shows `message` with the first tool that works.
    pub fn show(message: &Message<'_>) -> Option<Choice> {
        let has_display = ["WAYLAND_DISPLAY", "DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
        if !has_display {
            tracing::info!("no display, so the GPU failure is reported on standard error only");
            return None;
        }
        let desktop = std::env::var_os("XDG_CURRENT_DESKTOP");
        tools(desktop.as_deref())
            .into_iter()
            .find_map(|tool| show_with(tool, message))
    }

    fn show_with(tool: Tool, message: &Message<'_>) -> Option<Choice> {
        let args = message_args(tool, message);
        for _ in 0..MAX_ROUNDS {
            let output = match run(tool, &args, None) {
                Ok(output) => output,
                Err(err) => {
                    tracing::info!("cannot run {}: {err}", tool.program());
                    return None;
                }
            };
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            match answer(
                tool,
                message.offer_safe_mode,
                output.status.code(),
                &stdout,
                &stderr,
            ) {
                Some(Answer::Choice(choice)) => return Some(choice),
                Some(Answer::ShowDebugInfo) => {
                    let (debug_args, input) = debug_args(tool, message);
                    if let Err(err) = run(tool, &debug_args, input.as_deref()) {
                        tracing::info!("cannot show the debug info: {err}");
                    }
                }
                None => {
                    tracing::info!(
                        "{} did not show the message ({}): {}",
                        tool.program(),
                        output.status,
                        stderr.trim()
                    );
                    return None;
                }
            }
        }
        Some(Choice::Close)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::path::Path;

    use super::desktop::{Answer, Tool, answer, debug_args, message_args, tools};
    use super::*;

    fn message(offer_safe_mode: bool) -> Message<'static> {
        Message {
            text: "Glyphvane cannot open a window.",
            debug_info: "  Glyphvane 0.0.0\n  Error: none",
            debug_file: Some(Path::new("/logs/gpu-failure.txt")),
            offer_safe_mode,
        }
    }

    fn strings(args: &[OsString]) -> Vec<&str> {
        args.iter()
            .map(|arg| arg.to_str().expect("UTF-8"))
            .collect()
    }

    #[test]
    fn kde_prefers_kdialog() {
        assert_eq!(
            tools(Some(OsStr::new("KDE"))),
            [Tool::Kdialog, Tool::Zenity]
        );
        assert_eq!(
            tools(Some(OsStr::new("ubuntu:GNOME"))),
            [Tool::Zenity, Tool::Kdialog]
        );
        assert_eq!(tools(None), [Tool::Zenity, Tool::Kdialog]);
    }

    #[test]
    fn zenity_offers_safe_mode_and_the_debug_info() {
        let args = message_args(Tool::Zenity, &message(true));
        let args = strings(&args);
        assert_eq!(args[0], "--question");
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--ok-label", SAFE_MODE_LABEL])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--extra-button", SHOW_DEBUG_LABEL])
        );
        assert!(args.contains(&"--no-markup"));

        let offer = |code, stdout| answer(Tool::Zenity, true, Some(code), stdout, "");
        assert_eq!(offer(0, ""), Some(Answer::Choice(Choice::SafeMode)));
        assert_eq!(offer(1, "Show Debug Info\n"), Some(Answer::ShowDebugInfo));
        assert_eq!(offer(1, ""), Some(Answer::Choice(Choice::Close)));
        assert_eq!(offer(255, ""), None);
        assert_eq!(
            answer(Tool::Zenity, true, Some(1), "", "Failed to open display"),
            None
        );
        assert_eq!(answer(Tool::Zenity, true, None, "", ""), None);
    }

    #[test]
    fn zenity_without_an_offer_is_an_error_message() {
        let args = message_args(Tool::Zenity, &message(false));
        let args = strings(&args);
        assert_eq!(args[0], "--error");
        assert!(!args.contains(&SAFE_MODE_LABEL));
        assert_eq!(
            answer(Tool::Zenity, false, Some(0), "", ""),
            Some(Answer::Choice(Choice::Close))
        );
    }

    #[test]
    fn kdialog_maps_its_three_buttons() {
        let args = message_args(Tool::Kdialog, &message(true));
        assert!(strings(&args).contains(&"--warningyesnocancel"));
        let offer = |code| answer(Tool::Kdialog, true, Some(code), "", "");
        assert_eq!(offer(0), Some(Answer::Choice(Choice::SafeMode)));
        assert_eq!(offer(1), Some(Answer::ShowDebugInfo));
        assert_eq!(offer(2), Some(Answer::Choice(Choice::Close)));
        // A crash (abort) is not an answer.
        assert_eq!(offer(134), None);

        let args = message_args(Tool::Kdialog, &message(false));
        let args = strings(&args);
        assert_eq!(
            &args[2..],
            [
                "--detailederror",
                message(false).text,
                message(false).debug_info
            ]
        );
        assert_eq!(
            answer(Tool::Kdialog, false, Some(1), "", ""),
            Some(Answer::Choice(Choice::Close))
        );
    }

    #[test]
    fn the_debug_info_is_shown_as_text() {
        let (args, input) = debug_args(Tool::Zenity, &message(true));
        assert_eq!(strings(&args)[0], "--text-info");
        let input = input.expect("zenity reads the text from standard input");
        assert!(input.contains("Error: none"));

        let (args, input) = debug_args(Tool::Kdialog, &message(true));
        assert!(
            strings(&args)
                .windows(2)
                .any(|pair| pair == ["--textbox", "/logs/gpu-failure.txt"])
        );
        assert_eq!(input, None);
    }
}
