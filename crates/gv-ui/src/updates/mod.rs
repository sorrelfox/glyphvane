// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Update notifications (FR-PLAT-07, plan 14 § Updates) and the
//! application notices they use ([`notices`]).
//!
//! - **Opt-in.** Nothing is checked unless `general.check_for_updates` is
//!   on (first-run onboarding asks; the settings window can change it).
//!   Builds for a package manager turn the check off at build time
//!   (`GLYPHVANE_UPDATE_CHECK=off`), since the package manager updates
//!   them. The Flatpak repackages the release binaries, so it is
//!   recognised at run time instead ([`in_flatpak`]).
//! - **At most once a day**, in the background: the first look is a
//!   minute after start-up, then every 30 minutes the schedule is
//!   consulted. A failed request is retried after 1 h, 2 h, 4 h … up to a
//!   day ([`github::next_due`]). Safe mode makes no automatic check.
//! - **One request**: the latest release from the GitHub Releases API
//!   ([`github`]). Nothing is downloaded or installed.
//! - **A newer release** shows an application notice in every main window:
//!   *What's new* (the release notes, then the release page), *Skip this
//!   version*, and dismiss (until the next start).
//! - **Help ▸ Check for updates** (`app.check_for_updates`) checks at once
//!   and answers in a dialog, also when it finds nothing. Without the
//!   opt-in it first says what it will contact and offers *Check once* or
//!   *Check daily*.
//!
//! Not designed (NFR-UI-01): the notice and the dialogs.

pub mod github;
pub mod notices;
#[cfg(test)]
mod tests;
pub mod version;

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{App, AppContext as _, Global, PromptLevel};

use self::github::{Outcome, Release, Transport};
use self::notices::{AppButton, AppNotice, AppTopic};
use self::version::Version;
use crate::app_state;
use crate::files::notice::NoticeKind;
use crate::keymap::actions;
use crate::settings::{AppSettings, Target};

/// The setting that turns the check on.
pub const SETTING: &str = "general.check_for_updates";

/// How long after start-up the first automatic check may run.
const FIRST_LOOK: Duration = Duration::from_secs(60);

/// How often the schedule is consulted.
const TICK: Duration = Duration::from_secs(30 * 60);

/// Whether this build may check at all: package-manager builds turn the
/// check off when they are built.
pub fn enabled_by_build() -> bool {
    !matches!(option_env!("GLYPHVANE_UPDATE_CHECK"), Some("off" | "0"))
}

/// Whether this copy may check at all: not when the build turns the check
/// off ([`enabled_by_build`]) and not inside a Flatpak, which Flatpak
/// keeps up to date.
pub fn enabled() -> bool {
    enabled_by_build() && !in_flatpak()
}

/// Whether this copy runs inside a Flatpak sandbox (`/.flatpak-info`).
pub fn in_flatpak() -> bool {
    static IN_FLATPAK: OnceLock<bool> = OnceLock::new();
    *IN_FLATPAK.get_or_init(|| cfg!(target_os = "linux") && flatpak_info_in(Path::new("/")))
}

/// Whether `root` holds the file Flatpak puts at the root of every sandbox.
fn flatpak_info_in(root: &Path) -> bool {
    root.join(".flatpak-info").is_file()
}

/// The update checker.
struct Updates {
    transport: Arc<dyn Transport>,
    /// A request is under way.
    running: bool,
    /// The background schedule runs.
    started: bool,
    /// The time, for tests.
    clock: Option<u64>,
}

impl Global for Updates {}

impl Default for Updates {
    fn default() -> Self {
        Self {
            transport: default_transport(),
            running: false,
            started: false,
            clock: None,
        }
    }
}

#[cfg(not(test))]
fn default_transport() -> Arc<dyn Transport> {
    Arc::new(github::UreqTransport::default())
}

/// Unit tests never reach the network: without a fake, every request
/// fails.
#[cfg(test)]
fn default_transport() -> Arc<dyn Transport> {
    struct Offline;
    impl Transport for Offline {
        fn get(&self, _: &str, _: Option<&str>) -> Result<github::HttpResponse, String> {
            Err("tests have no network".to_owned())
        }
    }
    Arc::new(Offline)
}

/// Installs the handler of `app.check_for_updates`. Called by
/// [`crate::init`].
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &actions::app::CheckForUpdates, cx| check_now(cx));
}

/// Uses `transport` for the requests (tests).
pub fn set_transport(cx: &mut App, transport: Arc<dyn Transport>) {
    cx.default_global::<Updates>().transport = transport;
}

/// Fixes the time (Unix seconds) the checker sees (tests).
pub fn set_clock(cx: &mut App, now: u64) {
    cx.default_global::<Updates>().clock = Some(now);
}

fn now(cx: &mut App) -> u64 {
    cx.default_global::<Updates>().clock.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    })
}

/// Whether automatic checks are on: the user opted in, the build allows
/// it, and this is not safe mode.
pub fn automatic(cx: &App) -> bool {
    enabled()
        && cx
            .try_global::<AppSettings>()
            .is_some_and(|s| !s.safe_mode() && s.settings().general.check_for_updates)
}

/// Starts the background schedule. Call once at start-up, after the
/// application state is installed ([`crate::app_state::install`]).
/// Checks run only while [`automatic`] holds.
pub fn start(cx: &mut App) {
    let updates = cx.default_global::<Updates>();
    if updates.started {
        return;
    }
    updates.started = true;
    cx.spawn(async move |cx| {
        cx.background_executor().timer(FIRST_LOOK).await;
        loop {
            cx.update(tick);
            cx.background_executor().timer(TICK).await;
        }
    })
    .detach();
    // Turning the setting on looks at once (after the first minute).
    cx.observe_global::<AppSettings>(|cx| {
        if cx.default_global::<Updates>().started && automatic(cx) && never_checked(cx) {
            tick(cx);
        }
    })
    .detach();
}

fn never_checked(cx: &App) -> bool {
    app_state::get(cx).updates.last_attempt.is_none()
}

/// Runs an automatic check if one is due.
pub fn tick(cx: &mut App) {
    if !automatic(cx) || cx.default_global::<Updates>().running {
        return;
    }
    let state = app_state::get(cx).updates;
    if now(cx) < github::next_due(&state) {
        return;
    }
    run(cx, false);
}

/// `app.check_for_updates`: checks now and answers in a dialog. Without
/// the opt-in, asks first.
pub fn check_now(cx: &mut App) {
    if !enabled() {
        prompt(
            cx,
            PromptLevel::Info,
            "Updates come from your package manager",
            Some(
                "This copy of Glyphvane was installed by a package manager, which keeps it up to date.",
            ),
            &["OK"],
            |_, _| {},
        );
        return;
    }
    if cx
        .try_global::<AppSettings>()
        .is_some_and(|s| s.settings().general.check_for_updates)
    {
        run(cx, true);
        return;
    }
    prompt(
        cx,
        PromptLevel::Info,
        "Check for updates?",
        Some(
            "Glyphvane asks GitHub (api.github.com) which release is the newest. \
             Nothing about you or your documents is sent, and nothing is downloaded. \
             Check daily turns on the automatic check, at most once a day; \
             Settings ▸ General can turn it off again.",
        ),
        &["Check once", "Check daily", "Cancel"],
        |answer, cx| match answer {
            0 => run(cx, true),
            1 => {
                if let Err(err) = AppSettings::set_value(
                    cx,
                    Target::AllVaults,
                    SETTING,
                    &toml::Value::Boolean(true),
                ) {
                    tracing::warn!("updates: cannot turn on the daily check: {err}");
                }
                run(cx, true);
            }
            _ => {}
        },
    );
}

/// Makes the request off the UI thread and handles the outcome. `manual`
/// answers in a dialog whatever the outcome.
fn run(cx: &mut App, manual: bool) {
    let updates = cx.default_global::<Updates>();
    if updates.running {
        return;
    }
    updates.running = true;
    let transport = updates.transport.clone();
    let mut state = app_state::get(cx).updates;
    let now = now(cx);
    let current = Version::current();
    let task = cx.background_spawn(async move {
        let outcome = github::check(transport.as_ref(), &current, &mut state, now);
        (outcome, state)
    });
    cx.spawn(async move |cx| {
        let (outcome, state) = task.await;
        cx.update(|cx| finish(cx, outcome, state, manual));
    })
    .detach();
}

fn finish(cx: &mut App, outcome: Outcome, checked: gv_settings::UpdateState, manual: bool) {
    cx.default_global::<Updates>().running = false;
    app_state::update(cx, |state| {
        // The skipped version may have changed while the request ran.
        let skipped = state.updates.skipped_version.take();
        state.updates = checked;
        state.updates.skipped_version = skipped;
    });
    match outcome {
        Outcome::Newer(release) => {
            tracing::info!("updates: Glyphvane {} is available", release.version);
            let skipped = app_state::get(cx).updates.skipped_version == Some(release.tag.clone());
            if !skipped || manual {
                notices::show(cx, notice(&release));
            }
            if manual {
                prompt(
                    cx,
                    PromptLevel::Info,
                    &format!("Glyphvane {} is available", release.version),
                    Some(&format!(
                        "You have {}. The release page has the release notes and the downloads.",
                        Version::current()
                    )),
                    &["Open release page", "Later"],
                    move |answer, cx| {
                        if answer == 0 {
                            open_release_page(&release.url, cx);
                        }
                    },
                );
            }
        }
        Outcome::UpToDate => {
            notices::remove(cx, AppTopic::Update);
            if manual {
                prompt(
                    cx,
                    PromptLevel::Info,
                    "Glyphvane is up to date",
                    Some(&format!("{} is the newest release.", Version::current())),
                    &["OK"],
                    |_, _| {},
                );
            }
        }
        Outcome::Failed(err) => {
            tracing::warn!("updates: the check failed: {err}");
            if manual {
                prompt(
                    cx,
                    PromptLevel::Warning,
                    "Cannot check for updates",
                    Some(&format!("{err}. Try again later.")),
                    &["OK"],
                    |_, _| {},
                );
            }
        }
    }
}

/// The notice of a newer release.
fn notice(release: &Release) -> AppNotice {
    let notes = release.clone();
    let skip = release.tag.clone();
    AppNotice {
        topic: AppTopic::Update,
        kind: NoticeKind::Information,
        title: format!("Glyphvane {} is available", release.version).into(),
        detail: Some(format!("You have {}.", Version::current()).into()),
        buttons: vec![
            AppButton {
                label: "What's new".into(),
                run: std::rc::Rc::new(move |_, cx| show_notes(&notes, cx)),
                dismisses: false,
            },
            AppButton {
                label: "Skip this version".into(),
                run: std::rc::Rc::new(move |_, cx| {
                    let tag = skip.clone();
                    app_state::update(cx, |s| s.updates.skipped_version = Some(tag));
                }),
                dismisses: true,
            },
        ],
        on_dismiss: None,
    }
}

/// The release notes in a dialog, with the way to the release page.
fn show_notes(release: &Release, cx: &mut App) {
    let notes = notes_excerpt(&release.notes);
    let url = release.url.clone();
    prompt(
        cx,
        PromptLevel::Info,
        &format!("What's new in Glyphvane {}", release.version),
        Some(&notes),
        &["Open release page", "Close"],
        move |answer, cx| {
            if answer == 0 {
                open_release_page(&url, cx);
            }
        },
    );
}

/// The release notes as plain lines for a dialog: at most 1,500
/// characters, Markdown markers left as they are.
pub fn notes_excerpt(notes: &str) -> String {
    const MAX: usize = 1500;
    let notes = notes.trim();
    if notes.is_empty() {
        return "The release page has the details.".to_owned();
    }
    if notes.chars().count() <= MAX {
        return notes.to_owned();
    }
    let mut text: String = notes.chars().take(MAX).collect();
    text.push_str("…\n\nThe release page has the rest.");
    text
}

/// Opens a release page in the browser. Only the project's release pages
/// are ever opened.
fn open_release_page(url: &str, cx: &mut App) {
    if !url.starts_with(github::RELEASES_PAGE) {
        return;
    }
    crate::platform::launcher(cx).open_url(url, cx);
}

/// Shows a dialog in the active window (else the main window focused
/// last) and runs `then` with the index of the button clicked. Without a
/// window, the message goes to the log.
pub(crate) fn prompt(
    cx: &mut App,
    level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    answers: &[&str],
    then: impl FnOnce(usize, &mut App) + 'static,
) {
    let message = message.to_owned();
    let detail = detail.map(str::to_owned);
    let answers: Vec<String> = answers.iter().map(|a| (*a).to_owned()).collect();
    // Deferred: a click or a command may come from inside a window's own
    // update, which cannot be entered again.
    cx.defer(move |cx| {
        let window = cx
            .active_window()
            .or_else(|| crate::most_recent_window(cx).map(|w| w.window))
            .or_else(|| cx.windows().into_iter().next());
        let Some(window) = window else {
            tracing::info!("{message}");
            return;
        };
        let answers: Vec<&str> = answers.iter().map(String::as_str).collect();
        let receiver = window.update(cx, |_, window, cx| {
            window.prompt(level, &message, detail.as_deref(), &answers, cx)
        });
        let Ok(receiver) = receiver else {
            tracing::info!("{message}");
            return;
        };
        cx.spawn(async move |cx| {
            if let Ok(answer) = receiver.await {
                cx.update(|cx| then(answer, cx));
            }
        })
        .detach();
    });
}
