// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The `glyphvane` binary.

mod cli;
mod gpu;
mod instance;
mod settings;

use std::cell::Cell;
use std::io::IsTerminal as _;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;

use clap::Parser as _;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{AnyWindowHandle, App};
use gv_ui::{AppSettings, OpenTarget, Placement, ThemeChoice};
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, OpenRequest};
use crate::instance::Claim;

/// Logs go to standard error, warnings and errors by default. `RUST_LOG`
/// picks other levels, for example `RUST_LOG=info` to see which GPU adapter
/// GPUI selects.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    // Fails only if a logger is installed already, which leaves logging working.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging();
    let cwd = std::env::current_dir().unwrap_or_default();
    let request = OpenRequest::from_cli(&cli, &cwd);
    let locations = settings::locations(cli.config_dir.as_deref());

    // Hand the paths to a running instance before anything else is loaded.
    // A separate `--config-dir` is a separate instance.
    let claim = match &locations {
        Some(locations) => instance::claim(
            instance::endpoint(locations, instance::runtime_dir_from_env()),
            &request,
            instance::TIMEOUT,
        ),
        None => Claim::Standalone,
    };
    let primary = match claim {
        Claim::Delivered => return ExitCode::SUCCESS,
        Claim::Primary(primary) => Some(primary),
        Claim::Standalone => None,
    };

    if cli.safe_mode {
        let notice = gpu::safe_mode_notice();
        tracing::info!("{notice}");
        eprintln!("{notice}");
    }
    let settings_file = settings::load(locations.as_ref());
    let watched = settings_file.as_ref().map(|file| file.path().to_path_buf());
    let app_settings = AppSettings::new(settings_file, cli.theme.map(ThemeChoice::from));
    settings::report(&app_settings);
    let failed = Rc::new(Cell::new(false));

    // Requests from later invocations and from the platform reach the UI
    // thread through this channel.
    let (sender, mut requests) = mpsc::unbounded::<OpenRequest>();
    let _instance = primary.map(|primary| {
        let sender = sender.clone();
        primary.serve(Arc::new(move |request| {
            sender
                .unbounded_send(request)
                .map_err(|_| "the application is quitting".to_owned())
        }))
    });

    let application = gpui::application().with_assets(gv_ui::Assets);
    application.on_open_urls(move |urls| {
        if let Some(request) = request_from_urls(&urls) {
            // The receiver is gone only when the app is quitting.
            let _ = sender.unbounded_send(request);
        }
    });
    application.run({
        let failed = Rc::clone(&failed);
        move |cx| {
            gv_ui::init(cx, app_settings);
            if let Some(path) = watched {
                settings::watch(path, cx);
            }
            match open(request, cx) {
                Ok(window) => gpu::report_adapter(window, cx),
                Err(err) => {
                    if gpu::is_gpu_error(&err) {
                        tracing::error!("no usable GPU: {err:#}");
                        eprintln!("{}", gpu::failure_message(&err));
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
                while let Some(request) = requests.next().await {
                    cx.update(|cx| {
                        if let Err(err) = open(request, cx) {
                            tracing::error!("cannot open the requested files: {err:#}");
                        }
                    });
                }
            })
            .detach();
        }
    });

    if failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Opens what `request` names, by the rules of [`gv_ui::open`]: files as
/// tabs of the most recently focused window, or of a new window that starts
/// with its panels hidden.
fn open(request: OpenRequest, cx: &mut App) -> gpui::Result<AnyWindowHandle> {
    for folder in &request.folders {
        // Vaults come later; the folder is only noted.
        tracing::info!("folder requested: {}", folder.display());
    }
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
    gv_ui::open(cx, request.files, placement)
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
