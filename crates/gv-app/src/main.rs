// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The `glyphvane` binary.

use std::cell::Cell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;

use clap::{Parser, ValueEnum};
use gv_ui::ThemeChoice;

/// A Markdown viewer and editor.
#[derive(Debug, Parser)]
#[command(name = "glyphvane", version)]
struct Cli {
    /// Theme to use. `system` follows the operating system's appearance.
    #[arg(long, value_enum, default_value_t = ThemeArg::System)]
    theme: ThemeArg,

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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let failed = Rc::new(Cell::new(false));

    gpui::application().with_assets(gv_ui::Assets).run({
        let failed = Rc::clone(&failed);
        move |cx| {
            gv_ui::init(cx, cli.theme.into());
            if let Err(err) = gv_ui::open_file_window(cx, cli.path) {
                eprintln!("glyphvane: cannot open a window: {err:#}");
                failed.set(true);
                cx.quit();
            }
        }
    });

    if failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
