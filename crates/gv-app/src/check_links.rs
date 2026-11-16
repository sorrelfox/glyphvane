// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `glyphvane check-links`: the link checker without a window.
//!
//! Nothing here starts GPUI, loads settings or contacts a running
//! instance: the path and content indexes are built in memory and the
//! links checked on them ([`gv_vault::content::check`]). Findings go to
//! standard output; problems with the command itself to standard error as
//! `glyphvane: …` lines.

use std::io::Write as _;
use std::process::ExitCode;

use gv_profile::{PlannedProfile, ProfileId};
use gv_vault::content::CheckOptions;
use gv_vault::content::check::{EXIT_USAGE, check_path};

use crate::cli::{CheckLinksArgs, CheckLinksFormat};

/// Runs `glyphvane check-links` and returns the process exit status.
pub fn run(args: &CheckLinksArgs) -> ExitCode {
    match check(args) {
        Ok(status) => ExitCode::from(status),
        Err(message) => {
            eprintln!("glyphvane: {message}");
            ExitCode::from(EXIT_USAGE)
        }
    }
}

fn check(args: &CheckLinksArgs) -> Result<u8, String> {
    let profile = args.profile.as_deref().map(profile_of).transpose()?;
    let run = check_path(&args.path, profile, &CheckOptions::default())
        .map_err(|err| format!("cannot check {}: {err}", args.path.display()))?;
    let out = match args.format {
        CheckLinksFormat::Text => run.report.to_text(),
        CheckLinksFormat::Json => run.report.to_json(),
    };
    let mut stdout = std::io::stdout().lock();
    // A closed pipe (`glyphvane check-links . | head`) is not an error.
    let _ = stdout
        .write_all(out.as_bytes())
        .and_then(|()| stdout.flush());
    Ok(run.report.exit_code(args.strict))
}

fn profile_of(id: &str) -> Result<ProfileId, String> {
    if let Some(profile) = ProfileId::from_id(id) {
        return Ok(profile);
    }
    let available = ProfileId::ALL.map(ProfileId::as_str).join(", ");
    Err(if PlannedProfile::from_id(id).is_some() {
        format!("profile `{id}` is not supported yet; available: {available}")
    } else {
        format!("unknown profile `{id}`; available: {available}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_checked() {
        assert_eq!(profile_of("obsidian"), Ok(ProfileId::Obsidian));
        assert!(profile_of("nope").is_err_and(|e| e.starts_with("unknown profile")));
    }
}
