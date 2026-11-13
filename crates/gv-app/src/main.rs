// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The `glyphvane` binary.

mod gpu;

use std::cell::Cell;
use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;

use clap::{Parser, ValueEnum};
use gv_ui::ThemeChoice;
use tracing_subscriber::EnvFilter;

/// A Markdown viewer and editor.
#[derive(Debug, Parser)]
#[command(name = "glyphvane", version)]
struct Cli {
    /// Theme to use. `system` follows the operating system's appearance.
    #[arg(long, value_enum, default_value_t = ThemeArg::System)]
    theme: ThemeArg,

    /// Start with conservative graphics settings, for a graphics driver that
    /// draws the window wrongly.
    #[arg(long)]
    safe_mode: bool,

    /// File to open. It opens in Preview with the side panels hidden.
    path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ThemeArg {
    System,
    Light,
    SemiLight,
    Dark,
}

impl From<ThemeArg> for ThemeChoice {
    fn from(arg: ThemeArg) -> Self {
        match arg {
            ThemeArg::System => Self::System,
            ThemeArg::Light => Self::Light,
            ThemeArg::SemiLight => Self::SemiLight,
            ThemeArg::Dark => Self::Dark,
        }
    }
}

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
    if cli.safe_mode {
        let notice = gpu::safe_mode_notice();
        tracing::info!("{notice}");
        eprintln!("{notice}");
    }
    let failed = Rc::new(Cell::new(false));

    gpui::application().with_assets(gv_ui::Assets).run({
        let failed = Rc::clone(&failed);
        move |cx| {
            gv_ui::init(cx, cli.theme.into());
            match gv_ui::open_file_window(cx, cli.path) {
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
                }
            }
        }
    });

    if failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
