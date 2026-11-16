// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `glyphvane export`: a Markdown file to a standalone HTML file, without
//! a window, as *Export as HTML…* writes it (light theme, images inlined,
//! links to other notes pointing to their `.html`).
//!
//! The profile is the one given, else the one the app would detect for the
//! file. Links get no vault index here, so only their URL kind is checked.
//! PDF export comes with the helper process; until then `--pdf` is a usage
//! error. Problems are reported on standard error as `glyphvane: …` lines.

use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gv_profile::{CompiledProfile, DetectOptions, Overrides, PlannedProfile, ProfileId};
use gv_text::DecodeStatus;

use crate::cli::ExportArgs;

/// Exit status: the input cannot be read or the output cannot be written.
const EXIT_IO: u8 = 1;
/// Exit status: a usage error (a profile or format that is not available).
const EXIT_USAGE: u8 = 2;

/// Runs `glyphvane export` and returns the process exit status.
pub fn run(args: &ExportArgs) -> ExitCode {
    match export(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((status, message)) => {
            eprintln!("glyphvane: {message}");
            ExitCode::from(status)
        }
    }
}

fn export(args: &ExportArgs) -> Result<(), (u8, String)> {
    if args.pdf {
        return Err((
            EXIT_USAGE,
            "PDF export is not available yet; use --html".to_owned(),
        ));
    }
    let stdin = args.input.as_os_str() == "-";
    let path = (!stdin).then(|| std::path::absolute(&args.input).unwrap_or(args.input.clone()));
    let profile = profile(args.profile.as_deref(), path.as_deref())?;
    let name = path
        .as_deref()
        .map_or_else(|| "standard input".to_owned(), |p| p.display().to_string());
    let bytes =
        read_input(&args.input).map_err(|e| (EXIT_IO, format!("cannot read {name}: {e}")))?;
    let decoded =
        gv_text::decode(&bytes).map_err(|e| (EXIT_IO, format!("cannot export {name}: {e}")))?;
    if let DecodeStatus::Lossy(_) = decoded.status {
        eprintln!(
            "glyphvane: warning: {name}: decoded as {} with losses; undecodable bytes are exported as U+FFFD",
            decoded.format.encoding
        );
    }
    let root = path.as_deref().and_then(Path::parent);
    let export = gv_ui::export::export_text(&decoded.text, path.as_deref(), root, &profile, None);
    for note in &export.notes {
        eprintln!(
            "glyphvane: warning: image {} not embedded: {}",
            note.src, note.reason
        );
    }
    let output = match (&args.output, &path) {
        (Some(out), _) => out.clone(),
        (None, Some(path)) => path.with_extension("html"),
        (None, None) => PathBuf::from("-"),
    };
    if output.as_os_str() == "-" {
        let mut out = io::stdout().lock();
        return match out
            .write_all(export.html.as_bytes())
            .and_then(|()| out.flush())
        {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            Err(e) => Err((EXIT_IO, format!("cannot write to standard output: {e}"))),
        };
    }
    if path.is_some() && path == std::path::absolute(&output).ok() {
        return Err((
            EXIT_USAGE,
            format!(
                "{} is the input; give another output with -o",
                output.display()
            ),
        ));
    }
    gv_ui::export::write_export(&output, &export)
        .map_err(|e| (EXIT_IO, format!("cannot write {}: {e}", output.display())))
}

/// The profile named `id`, else the one detected for `path` (extended for
/// standard input).
fn profile(id: Option<&str>, path: Option<&Path>) -> Result<CompiledProfile, (u8, String)> {
    match id {
        Some(id) => match ProfileId::from_id(id) {
            Some(id) => Ok(gv_profile::compile_builtin(id)),
            None => {
                let available = ProfileId::ALL.map(ProfileId::as_str).join(", ");
                let message = if PlannedProfile::from_id(id).is_some() {
                    format!("profile `{id}` is not supported yet; available: {available}")
                } else {
                    format!("unknown profile `{id}`; available: {available}")
                };
                Err((EXIT_USAGE, message))
            }
        },
        None => Ok(match path {
            Some(path) => {
                gv_profile::detect(path, &Overrides::default(), &DetectOptions::default()).compile()
            }
            None => gv_profile::compile_builtin(ProfileId::Extended),
        }),
    }
}

fn read_input(input: &Path) -> io::Result<Vec<u8>> {
    if input.as_os_str() == "-" {
        let mut bytes = Vec::new();
        io::stdin().lock().read_to_end(&mut bytes)?;
        Ok(bytes)
    } else {
        std::fs::read(input)
    }
}
