// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The `glyphvane` binary.

mod check_links;
mod cli;
mod clipboard;
mod crash;
#[cfg(target_os = "linux")]
mod emoji_font;
mod export;
mod gpu;
mod instance;
mod latency;
mod no_gpu_dialog;
mod pdf;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod present;
#[cfg(target_os = "linux")]
mod print_portal;
mod render;
mod safe_mode;
mod session;
mod settings;
mod startup;
mod wait;

use std::cell::Cell;
use std::ffi::OsString;
use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{AnyWindowHandle, App};
use gv_ui::shell::session as ui_session;
use gv_ui::{AppSettings, OpenTarget, Placement, ThemeChoice};
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command, OpenRequest};
use crate::instance::{Claim, Reporter, WaitEvent, Waiting};
use crate::no_gpu_dialog::Choice;
use crate::safe_mode::{Platform, Start};

/// Logs go to standard error, warnings and errors by default, more with
/// `--verbose` ([`cli::log_filter`]). `RUST_LOG` picks other levels, for
/// example `RUST_LOG=info` to see which GPU adapter GPUI selects.
fn init_logging(verbose: bool) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(cli::log_filter(verbose)));
    // Fails only if a logger is installed already, which leaves logging working.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
}

fn main() -> ExitCode {
    // First, so that the trace starts as early as it can.
    startup::start();
    let cli = Cli::parse();
    startup::mark("arguments parsed");
    // Headless commands start no GUI, logger, settings or instance.
    if let Some(command) = &cli.command {
        return match command {
            Command::Export(args) => export::run(args),
            Command::Render(args) => render::run(args),
            Command::CheckLinks(args) => check_links::run(args),
        };
    }
    if cli.licenses {
        // A closed pipe (`glyphvane --licenses | head`) is not an error.
        let _ = cli::print_licenses(&mut std::io::stdout().lock());
        return ExitCode::SUCCESS;
    }
    init_logging(cli.verbose);
    let platform = Platform::current();
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let marker = std::env::var_os(safe_mode::CHILD_MARKER);
    let safe_mode = match safe_mode::start(platform, cli.safe_mode, marker.as_deref(), &args) {
        Start::Normal => false,
        Start::Safe => true,
        Start::Relaunch(relaunch) => match safe_mode::run(&relaunch) {
            Ok(code) => return code,
            Err(err) => {
                tracing::warn!(
                    "safe mode: cannot start again with conservative graphics settings, \
                     so the renderer is unchanged: {err}"
                );
                true
            }
        },
    };
    if safe_mode {
        safe_mode::report(platform);
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let request = OpenRequest::from_cli(&cli, &cwd);
    if request.wait && request.files.is_empty() {
        eprintln!("glyphvane: --wait needs a file to open");
        return ExitCode::from(2);
    }
    let locations = settings::locations(cli.config_dir.as_deref());
    let log_dir = locations
        .as_ref()
        .map_or_else(std::env::temp_dir, |locations| {
            locations.log_dir().to_path_buf()
        });

    // Hand the paths to a running instance before anything else is loaded.
    // A separate `--config-dir` is a separate instance. Safe mode runs on
    // its own: a running instance would open the files with its normal
    // renderer and settings.
    let claim = match &locations {
        Some(locations) if !safe_mode => instance::claim(
            instance::endpoint(locations, instance::runtime_dir_from_env()),
            &request,
            instance::TIMEOUT,
        ),
        _ => Claim::Standalone,
    };
    let primary = match claim {
        Claim::Delivered => {
            startup::mark("single-instance claim done (delivered to the running instance)");
            return ExitCode::SUCCESS;
        }
        Claim::Waiting(waiting) => {
            startup::mark("single-instance claim done (delivered, waiting)");
            return wait_for_running_instance(waiting);
        }
        Claim::Primary(primary) => {
            startup::mark("single-instance claim done (primary)");
            Some(primary)
        }
        Claim::Standalone => {
            startup::mark("single-instance claim done (standalone)");
            None
        }
    };

    let settings_file = settings::load(locations.as_ref());
    let watched = settings_file.as_ref().map(|file| file.path().to_path_buf());
    let mut app_settings = AppSettings::new(settings_file, cli.theme.map(ThemeChoice::from));
    if let Some(locations) = &locations {
        app_settings = app_settings.with_locations(locations.clone());
    }
    if safe_mode {
        app_settings = app_settings.with_safe_mode();
    }
    settings::report(&app_settings);
    // Local crash reports, written only while the opt-in setting is on.
    crash::install(
        locations
            .as_ref()
            .map(gv_settings::Locations::crash_reports_dir),
        app_settings.settings().privacy_and_security.crash_reports,
    );
    startup::mark("settings loaded");
    // Read before the event loop starts, like the settings.
    let session_store = session::store(locations.as_ref());
    startup::mark("session store read");
    let failed = Rc::new(Cell::new(false));
    // Set when the user chooses safe mode in the no-GPU message.
    let restart_safe = Rc::new(Cell::new(false));

    // Requests from later invocations and from the platform reach the UI
    // thread through this channel, a `--wait` request with its reporter.
    let (sender, mut requests) = mpsc::unbounded::<(OpenRequest, Option<Reporter>)>();
    let instance = primary.map(|primary| {
        let sender = sender.clone();
        primary.serve(Arc::new(move |request, reporter| {
            sender
                .unbounded_send((request, reporter))
                .map_err(|_| "the application is quitting".to_owned())
        }))
    });
    let waiters = instance.as_ref().map(instance::Guard::waiters);
    let quit_waiters = waiters.clone();

    let application = gpui::application().with_assets(gv_ui::Assets);
    startup::mark("GPUI application created");
    application.on_open_urls(move |urls| {
        if let Some(request) = request_from_urls(&urls) {
            // The receiver is gone only when the app is quitting.
            let _ = sender.unbounded_send((request, None));
        }
    });
    application.run({
        let failed = Rc::clone(&failed);
        let restart_safe = Rc::clone(&restart_safe);
        move |cx| {
            startup::mark("GPUI application started");
            gv_ui::init(cx, app_settings);
            #[cfg(target_os = "linux")]
            emoji_font::install(cx);
            latency::install(cx);
            gv_ui::editor::set_html_clipboard(cx, Some(clipboard::system_reader()));
            gv_ui::export::set_rich_clipboard(cx, Some(clipboard::system_writer()));
            gv_ui::export::set_pdf_engine(
                cx,
                Some(std::sync::Arc::new(pdf::HelperEngine::default())),
            );
            #[cfg(target_os = "linux")]
            gv_ui::print::set_printer(cx, print_portal::PortalPrinter);
            gv_ui::keymap::install(cx, locations.as_ref());
            // The crash journal and file watching; safe mode keeps both:
            // they protect the user's text.
            gv_ui::files::install(cx, locations.as_ref());
            ui_session::install(cx, session_store);
            // Onboarding, update check and crash-report bookkeeping.
            gv_ui::app_state::install(cx);
            cx.observe_global::<AppSettings>(|cx| {
                crash::set_enabled(gv_ui::crash_reports::enabled(cx));
            })
            .detach();
            // Recent documents for the Dock (macOS) and the jump list
            // (Windows).
            gv_ui::integration::recent::install(cx);
            if let Some(path) = watched {
                settings::watch(path, cx);
            }
            // Quitting lets every tab go: waiting clients are told their
            // files are closed (saved first by the quit), and get the time
            // to read it.
            cx.on_app_quit(move |cx| {
                gv_ui::shell::release_close_hooks(cx);
                if let Some(waiters) = &quit_waiters {
                    waiters.finish(WAITERS_AT_EXIT);
                }
                async {}
            })
            .detach();
            startup::mark("UI set up (theme, keymap, journal, session, settings watcher)");
            let waits = request.wait;
            let plain_launch = !waits && restores_session(&request);
            let opened = if waits {
                wait::open(request, wait::Waiter::Local, cx)
            } else if restores_session(&request) {
                restore(cx)
            } else {
                open(request, cx)
            };
            match opened {
                Ok(window) => {
                    startup::mark("window created");
                    startup::attach(window, cx);
                    gpu::report_adapter(window, cx);
                    latency::attach(window, cx);
                    // Documents with unsaved changes from a crash (or a
                    // close without saving) reopen with their text. Not in
                    // a window for `--wait`, which quits when its files are
                    // closed: they wait for the next start.
                    if !waits {
                        gv_ui::files::recover_unsaved(cx);
                        // A report from the last run, the update check
                        // (only with the opt-in), and the first-run
                        // welcome on a start without files.
                        gv_ui::crash_reports::announce(cx);
                        gv_ui::updates::start(cx);
                        if plain_launch {
                            gv_ui::onboarding::show_if_first_run(cx);
                        }
                    }
                }
                Err(err) => {
                    if gpu::is_gpu_error(&err) {
                        tracing::error!("no usable GPU: {err:#}");
                        if report_gpu_failure(&err, platform, safe_mode, &log_dir, cx)
                            == Some(Choice::SafeMode)
                        {
                            restart_safe.set(true);
                        }
                    } else {
                        tracing::error!("cannot open a window: {err:#}");
                        eprintln!("glyphvane: cannot open a window: {err:#}");
                    }
                    failed.set(true);
                    // A quit asked for while the application is still
                    // launching is lost on Linux, whose event loop is not
                    // running yet. Quit from the loop's first turn instead.
                    cx.spawn(async |cx| cx.update(|cx| cx.quit())).detach();
                    return;
                }
            }
            cx.spawn(async move |cx| {
                while let Some((request, reporter)) = requests.next().await {
                    cx.update(|cx| {
                        let opened = match reporter {
                            Some(reporter) => {
                                wait::open(request, wait::Waiter::Remote(reporter), cx)
                            }
                            None => open(request, cx),
                        };
                        if let Err(err) = opened {
                            tracing::error!("cannot open the requested files: {err:#}");
                        }
                    });
                }
            })
            .detach();
        }
    });

    if let Some(waiters) = &waiters {
        waiters.finish(WAITERS_AT_EXIT);
    }
    if restart_safe.get() {
        // Give up the single-instance endpoint first: the new process does
        // not take it, and a later normal start should.
        drop(instance);
        if let Some(relaunch) = safe_mode::relaunch(platform, &args) {
            match safe_mode::run(&relaunch) {
                Ok(code) => return code,
                Err(err) => tracing::error!("safe mode: cannot start again: {err}"),
            }
        }
    }
    if failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// How long a quitting instance gives its waiting clients to read that
/// their files are closed.
const WAITERS_AT_EXIT: Duration = Duration::from_secs(2);

/// `--wait` handed to the running instance: blocks until it reports the
/// files closed. Fails when it ends first.
fn wait_for_running_instance(waiting: Waiting) -> ExitCode {
    let waited = waiting.wait(|event| {
        if let WaitEvent::Closed {
            path,
            unsaved: true,
        } = event
        {
            eprintln!("{}", wait::unsaved_message(path));
        }
    });
    match waited {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!(
                "glyphvane: stopped waiting: Glyphvane ended before the files were closed ({err})"
            );
            ExitCode::FAILURE
        }
    }
}

/// Tells the user that the GPU could not be used: on standard error, in the
/// failure file in the log folder, and in a native message box, which offers
/// a restart in safe mode where that can help. Returns the user's choice, or
/// `None` when no message box could be shown.
fn report_gpu_failure(
    err: &anyhow::Error,
    platform: Platform,
    safe_mode: bool,
    log_dir: &std::path::Path,
    cx: &mut App,
) -> Option<Choice> {
    let failure = gpu::Failure::new(err, platform, safe_mode);
    let saved: Option<PathBuf> = match failure.save(log_dir) {
        Ok(path) => Some(path),
        Err(err) => {
            tracing::warn!("cannot save the debug info in {}: {err}", log_dir.display());
            None
        }
    };
    eprintln!("{}", failure.terminal_message(saved.as_deref()));
    let text = failure.dialog_text(saved.as_deref());
    let debug_info = failure.debug_info();
    let choice = no_gpu_dialog::show(
        &no_gpu_dialog::Message {
            text: &text,
            debug_info: &debug_info,
            debug_file: saved.as_deref(),
            offer_safe_mode: failure.offers_safe_mode(),
        },
        cx,
    );
    match choice {
        Some(choice) => tracing::info!("no-GPU message answered: {choice:?}"),
        None => tracing::info!("no native message box could be shown"),
    }
    choice
}

/// Whether the first request of this run restores the session: a launch
/// without paths does. A launch with files or folders opens only those, and
/// leaves the stored session as it is (FR-VIEW-16).
fn restores_session(request: &OpenRequest) -> bool {
    request.files.is_empty() && request.folders.is_empty()
}

/// Restores the session for a launch without paths; an empty window when
/// there is nothing to restore.
fn restore(cx: &mut App) -> gpui::Result<AnyWindowHandle> {
    match ui_session::restore(cx)? {
        Some(window) => Ok(window),
        None => gv_ui::open(cx, Vec::new(), Placement::RecentWindow),
    }
}

/// Opens what `request` names. Each folder opens as a vault by the rules of
/// [`gv_ui::open_folder`] (a blank window takes it, else a new window) and
/// shows the Files panel. A file inside one of those folders opens as a tab
/// in its window; the other files open by the rules of [`gv_ui::open`]: as
/// tabs of the most recently focused window, or of a new window that starts
/// with its panels hidden.
fn open(request: OpenRequest, cx: &mut App) -> gpui::Result<AnyWindowHandle> {
    for file in &request.files {
        match file.position {
            Some(at) => tracing::info!(
                "open {} at line {}{}",
                file.path.display(),
                at.line,
                at.column
                    .map(|c| format!(", column {c}"))
                    .unwrap_or_default()
            ),
            None => tracing::info!("open {}", file.path.display()),
        }
    }
    let placement = if request.new_window {
        Placement::NewWindow
    } else {
        Placement::RecentWindow
    };
    let mut vaults: Vec<(PathBuf, gv_ui::WorkspaceWindow)> = Vec::new();
    for folder in &request.folders {
        tracing::info!("open folder {}", folder.display());
        match gv_ui::open_folder(cx, folder, placement) {
            Ok(window) => vaults.push((folder.clone(), window)),
            Err(err) => {
                tracing::error!("cannot open the folder {}: {err:#}", folder.display());
            }
        }
    }
    let folders: Vec<PathBuf> = vaults.iter().map(|(folder, _)| folder.clone()).collect();
    let (inside, loose) = route_files(request.files, &folders);
    for (index, target) in inside {
        vaults[index].1.open_tab(target, cx)?;
    }
    match vaults.last() {
        Some((_, window)) if loose.is_empty() => {
            window.activate(cx);
            Ok(window.window)
        }
        _ => gv_ui::open(cx, loose, placement),
    }
}

/// Splits `files` into those inside one of `folders`, each with the index
/// of the innermost folder that holds it, and the others.
fn route_files(
    files: Vec<OpenTarget>,
    folders: &[PathBuf],
) -> (Vec<(usize, OpenTarget)>, Vec<OpenTarget>) {
    let mut inside = Vec::new();
    let mut loose = Vec::new();
    for file in files {
        let holder = folders
            .iter()
            .enumerate()
            .filter(|(_, folder)| file.path.starts_with(folder))
            .max_by_key(|(_, folder)| folder.components().count())
            .map(|(index, _)| index);
        match holder {
            Some(index) => inside.push((index, file)),
            None => loose.push(file),
        }
    }
    (inside, loose)
}

/// The request for URLs the platform asked to open (macOS sends files
/// opened from Finder this way). Only `file:` URLs are taken.
fn request_from_urls(urls: &[String]) -> Option<OpenRequest> {
    let mut request = OpenRequest {
        cwd: std::env::current_dir().unwrap_or_default(),
        files: Vec::new(),
        folders: Vec::new(),
        new_window: false,
        wait: false,
    };
    for text in urls {
        let path = url::Url::parse(text)
            .ok()
            .filter(|url| url.scheme() == "file")
            .and_then(|url| url.to_file_path().ok());
        match path {
            Some(path) if path.is_dir() => request.folders.push(path),
            Some(path) => request.files.push(OpenTarget::new(path)),
            None => tracing::info!("ignored a request to open {text}"),
        }
    }
    (!request.files.is_empty() || !request.folders.is_empty()).then_some(request)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser as _;

    use super::*;

    #[test]
    fn config_dir_moves_the_settings_file() {
        let cli = Cli::try_parse_from(["glyphvane", "--config-dir", "/tmp/gv-root", "a.md"])
            .expect("arguments parse");
        let locations = settings::locations(cli.config_dir.as_deref());
        assert_eq!(
            locations.map(|l| l.settings_file()),
            Some(PathBuf::from("/tmp/gv-root/config/settings.toml"))
        );
        assert_eq!(cli.paths, ["a.md"]);
    }

    #[test]
    fn only_a_launch_without_files_restores_the_session() {
        let cwd = std::env::temp_dir();
        let request = |args: &[&str]| {
            let cli = Cli::try_parse_from(args).expect("arguments parse");
            OpenRequest::from_cli(&cli, &cwd)
        };
        assert!(restores_session(&request(&["glyphvane"])));
        assert!(restores_session(&request(&["glyphvane", "--new-window"])));
        assert!(!restores_session(&request(&["glyphvane", "a.md"])));
        assert!(!restores_session(&request(&["glyphvane", "a.md:3"])));
    }

    #[test]
    fn a_launch_with_a_folder_opens_it_instead_of_the_session() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join("vault")).expect("folder");
        let cli = Cli::try_parse_from(["glyphvane", "vault"]).expect("arguments parse");
        let request = OpenRequest::from_cli(&cli, dir.path());
        assert_eq!(request.folders, [dir.path().join("vault")]);
        assert!(request.files.is_empty());
        assert!(!restores_session(&request));
    }

    #[test]
    fn files_go_to_the_innermost_folder_that_holds_them() {
        let folders = [PathBuf::from("/notes"), PathBuf::from("/notes/work")];
        let files = vec![
            OpenTarget::new("/notes/a.md"),
            OpenTarget::new("/notes/work/b.md"),
            OpenTarget::new("/notesx/c.md"),
            OpenTarget::new("/elsewhere/d.md"),
        ];
        let (inside, loose) = route_files(files, &folders);
        assert_eq!(
            inside,
            [
                (0, OpenTarget::new("/notes/a.md")),
                (1, OpenTarget::new("/notes/work/b.md")),
            ]
        );
        assert_eq!(
            loose,
            [
                OpenTarget::new("/notesx/c.md"),
                OpenTarget::new("/elsewhere/d.md")
            ]
        );
        let (inside, loose) = route_files(vec![OpenTarget::new("/a.md")], &[]);
        assert!(inside.is_empty());
        assert_eq!(loose, [OpenTarget::new("/a.md")]);
    }

    /// Folder requests in headless windows: at launch and when another
    /// instance hands one over, which both go through [`open`].
    mod folders {
        use std::path::Path;

        use gpui::TestAppContext;
        use gv_ui::{ThemeChoice, WorkspaceWindow};

        use super::*;

        const NOTE: &str = "# Note\n";

        fn request(folders: &[&Path], files: &[&Path], new_window: bool) -> OpenRequest {
            OpenRequest {
                cwd: std::env::temp_dir(),
                files: files.iter().map(|f| OpenTarget::new(*f)).collect(),
                folders: folders.iter().map(|f| f.to_path_buf()).collect(),
                new_window,
                wait: false,
            }
        }

        fn setup(cx: &mut TestAppContext) -> tempfile::TempDir {
            let dir = tempfile::tempdir().expect("temp dir");
            for name in ["vault/a.md", "vault/sub/b.md", "other/c.md", "loose.md"] {
                let path = dir.path().join(name);
                std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
                std::fs::write(path, NOTE).expect("write");
            }
            cx.update(|cx| {
                gv_ui::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
            });
            dir
        }

        fn open_request(cx: &mut TestAppContext, request: OpenRequest) -> WorkspaceWindow {
            cx.update(|cx| open(request, cx))
                .expect("the request opens");
            cx.run_until_parked();
            cx.update(gv_ui::most_recent_window).expect("a window")
        }

        /// The window's vault folder and its tabs' files.
        fn shown(
            cx: &mut TestAppContext,
            window: &WorkspaceWindow,
        ) -> (Option<PathBuf>, Vec<PathBuf>) {
            cx.update(|cx| {
                let ws = window.workspace.upgrade().expect("open").read(cx);
                let tabs = ws
                    .tabs()
                    .iter()
                    .filter_map(|t| t.path().map(Path::to_path_buf))
                    .collect();
                (ws.vault_root(cx), tabs)
            })
        }

        fn windows(cx: &mut TestAppContext) -> usize {
            cx.update(|cx| cx.windows().len())
        }

        #[gpui::test]
        fn a_folder_at_launch_opens_as_the_vault_of_one_window(cx: &mut TestAppContext) {
            let dir = setup(cx);
            let vault = dir.path().join("vault");
            let window = open_request(cx, request(&[&vault], &[], false));
            assert_eq!(windows(cx), 1);
            assert_eq!(shown(cx, &window), (Some(vault), Vec::new()));
        }

        #[gpui::test]
        fn files_inside_the_folder_open_in_its_window(cx: &mut TestAppContext) {
            let dir = setup(cx);
            let vault = dir.path().join("vault");
            let (a, b) = (vault.join("a.md"), vault.join("sub/b.md"));
            let window = open_request(cx, request(&[&vault], &[&a, &b], false));
            assert_eq!(windows(cx), 1);
            assert_eq!(shown(cx, &window), (Some(vault), vec![a, b]));
        }

        #[gpui::test]
        fn a_loose_file_with_new_window_gets_its_own_window(cx: &mut TestAppContext) {
            let dir = setup(cx);
            let vault = dir.path().join("vault");
            let (a, loose) = (vault.join("a.md"), dir.path().join("loose.md"));
            let window = open_request(cx, request(&[&vault], &[&a, &loose], true));
            assert_eq!(windows(cx), 2);
            assert_eq!(shown(cx, &window), (None, vec![loose]));
        }

        #[gpui::test]
        fn a_handed_over_folder_opens_beside_the_documents(cx: &mut TestAppContext) {
            let dir = setup(cx);
            let loose = dir.path().join("loose.md");
            let first = open_request(cx, request(&[], &[&loose], false));
            assert_eq!(windows(cx), 1);

            // A second `glyphvane other` while this one runs.
            let other = dir.path().join("other");
            let window = open_request(cx, request(&[&other], &[], false));
            assert_eq!(windows(cx), 2);
            assert_ne!(window.window, first.window);
            assert_eq!(shown(cx, &window), (Some(other.clone()), Vec::new()));
            assert_eq!(shown(cx, &first), (None, vec![loose]));

            // The same folder again comes back to its window.
            let again = open_request(cx, request(&[&other], &[], false));
            assert_eq!(windows(cx), 2);
            assert_eq!(again.window, window.window);

            // `--new-window` always opens one.
            open_request(cx, request(&[&other], &[], true));
            assert_eq!(windows(cx), 3);
        }

        #[gpui::test]
        fn a_folder_that_is_gone_still_leaves_a_window(cx: &mut TestAppContext) {
            let dir = setup(cx);
            let gone = dir.path().join("gone");
            let window = open_request(cx, request(&[&gone], &[], false));
            assert_eq!(windows(cx), 1);
            assert_eq!(shown(cx, &window), (None, Vec::new()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_urls_become_paths() {
        let urls = [
            "file:///notes/a%20b.md".to_owned(),
            "https://example.com/".to_owned(),
        ];
        let request = request_from_urls(&urls).expect("one file");
        assert_eq!(request.files, [OpenTarget::new("/notes/a b.md")]);
        assert!(request_from_urls(&urls[1..]).is_none());
    }
}
