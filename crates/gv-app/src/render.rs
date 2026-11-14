// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `glyphvane render`: Markdown to HTML or to a CST dump, without a window.
//!
//! Nothing here starts GPUI, loads settings, contacts a running instance or
//! installs a logger, so the command runs headless and starts quickly.
//! Problems are reported on standard error as `glyphvane: …` lines.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gv_text::{DecodeStatus, Decoded, LossReason};

use crate::cli::{RenderArgs, RenderFormat};

/// Exit status: the input cannot be read or the output cannot be written.
const EXIT_IO: u8 = 1;
/// Exit status: a usage error, such as a profile that is not available.
/// (clap uses the same status for errors it finds in the arguments.)
const EXIT_USAGE: u8 = 2;
/// Exit status: the input did not decode exactly and `--strict` was given.
const EXIT_LOSSY: u8 = 3;

/// Profiles `render` can use.
const SUPPORTED_PROFILES: &[&str] = &["commonmark"];
/// Built-in profiles that exist but cannot render yet.
const PLANNED_PROFILES: &[&str] = &["gfm-file", "gfm-comment", "obsidian", "extended"];

/// Why the command failed: the message for standard error and the status.
struct Failure {
    status: u8,
    message: String,
}

impl Failure {
    fn new(status: u8, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

/// Runs `glyphvane render` and returns the process exit status.
pub fn run(args: &RenderArgs) -> ExitCode {
    match render(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("glyphvane: {}", failure.message);
            ExitCode::from(failure.status)
        }
    }
}

fn render(args: &RenderArgs) -> Result<(), Failure> {
    check_profile(&args.profile)?;
    let name = display_name(&args.input);
    let bytes = read_input(&args.input)
        .map_err(|err| Failure::new(EXIT_IO, format!("cannot read {name}: {err}")))?;
    let decoded = gv_text::decode(&bytes)
        .map_err(|err| Failure::new(EXIT_IO, format!("cannot render {name}: {err}")))?;
    drop(bytes);
    if let DecodeStatus::Lossy(reason) = decoded.status {
        let what = loss_description(&decoded, reason);
        if args.strict {
            return Err(Failure::new(
                EXIT_LOSSY,
                format!("{name}: {what}; not rendered because of --strict"),
            ));
        }
        eprintln!("glyphvane: warning: {name}: {what}; undecodable bytes are rendered as U+FFFD");
    }

    let src = decoded.text.as_str();
    let output = match args.format {
        // `html` has no emitter of its own yet; see `RenderFormat::Html`.
        RenderFormat::Html | RenderFormat::CommonmarkHtml => {
            gv_syntax::harness::render_commonmark_html(src)
        }
        RenderFormat::Cst => gv_syntax::harness::cst_dump(src),
    };

    match args.output.as_deref() {
        None => write_stdout(output.as_bytes()),
        Some(path) if path.as_os_str() == "-" => write_stdout(output.as_bytes()),
        Some(path) => write_atomically(path, output.as_bytes()).map_err(|err| {
            Failure::new(EXIT_IO, format!("cannot write {}: {err}", path.display()))
        }),
    }
}

fn check_profile(profile: &str) -> Result<(), Failure> {
    if SUPPORTED_PROFILES.contains(&profile) {
        Ok(())
    } else if PLANNED_PROFILES.contains(&profile) {
        Err(Failure::new(
            EXIT_USAGE,
            format!(
                "profile `{profile}` is not supported yet; available: {}",
                SUPPORTED_PROFILES.join(", ")
            ),
        ))
    } else {
        Err(Failure::new(
            EXIT_USAGE,
            format!(
                "unknown profile `{profile}`; available: {}",
                SUPPORTED_PROFILES.join(", ")
            ),
        ))
    }
}

/// The input as named in messages.
fn display_name(input: &Path) -> String {
    if input.as_os_str() == "-" {
        "standard input".to_owned()
    } else {
        input.display().to_string()
    }
}

fn read_input(input: &Path) -> io::Result<Vec<u8>> {
    if input.as_os_str() == "-" {
        let mut bytes = Vec::new();
        io::stdin().lock().read_to_end(&mut bytes)?;
        Ok(bytes)
    } else {
        fs::read(input)
    }
}

/// What went wrong in a lossy decode, for the warning.
fn loss_description(decoded: &Decoded, reason: LossReason) -> String {
    let encoding = decoded.format.encoding;
    match reason {
        LossReason::InvalidUtf8 { valid_up_to } => {
            format!("invalid UTF-8 at byte {valid_up_to}")
        }
        LossReason::MalformedUtf16 => {
            format!("malformed {encoding} (odd length or unpaired surrogate)")
        }
        LossReason::MalformedLegacy => {
            format!("decoded as {encoding}, which does not define some of its bytes")
        }
        LossReason::NotReversible => {
            format!("decoded as {encoding}, which does not reproduce the input exactly")
        }
        _ => format!("decoded as {encoding} with losses"),
    }
}

fn write_stdout(bytes: &[u8]) -> Result<(), Failure> {
    let mut out = io::stdout().lock();
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        // The reader went away (`glyphvane render a.md | head`): nothing is
        // left to do, and it is not this command's error.
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(Failure::new(
            EXIT_IO,
            format!("cannot write to standard output: {err}"),
        )),
    }
}

/// Replaces `path` with `bytes` so that readers see the old file or the new
/// one, never a partial write: the bytes go to a temporary file in the same
/// folder, which is then renamed over `path`. A symbolic link at `path` is
/// followed, and an existing file's permissions are kept.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let existing = fs::metadata(&target).ok();
    if existing.as_ref().is_some_and(|meta| meta.is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            "it is a folder",
        ));
    }
    let (temp, mut file) = create_temp_beside(&target)?;
    let written = (|| {
        file.write_all(bytes)?;
        if let Some(meta) = &existing {
            file.set_permissions(meta.permissions())?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &target)
    })();
    if written.is_err() {
        // Best effort: the temporary file is ours and useless now.
        let _ = fs::remove_file(&temp);
    }
    written
}

/// A new, empty file next to `target`, named after it.
fn create_temp_beside(target: &Path) -> io::Result<(PathBuf, File)> {
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path names no file"))?;
    let pid = std::process::id();
    let mut attempt = 0u32;
    loop {
        let mut temp_name = OsString::from(".");
        temp_name.push(name);
        temp_name.push(format!(".{pid}-{attempt}.tmp"));
        let temp = dir.join(temp_name);
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists && attempt < 100 => {
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_checked() {
        assert!(check_profile("commonmark").is_ok());
        for profile in ["gfm-file", "obsidian", "nope", ""] {
            let Err(failure) = check_profile(profile) else {
                panic!("{profile} is accepted");
            };
            assert_eq!(failure.status, EXIT_USAGE);
            assert!(failure.message.contains(&format!("`{profile}`")));
        }
    }

    #[test]
    fn atomic_write_replaces_and_cleans_up() {
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("out.html");
        write_atomically(&out, b"one").expect("first write");
        write_atomically(&out, b"two").expect("second write");
        assert_eq!(fs::read(&out).expect("read"), b"two");
        let names: Vec<_> = fs::read_dir(dir.path())
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, [OsString::from("out.html")]);
        assert!(write_atomically(dir.path(), b"x").is_err());
        assert!(write_atomically(&dir.path().join("missing/out.html"), b"x").is_err());
    }
}
