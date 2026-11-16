// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The command line, and turning its paths into an open request.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use gv_ui::{OpenTarget, TextPosition, ThemeChoice};

/// A Markdown viewer and editor.
#[derive(Debug, Parser)]
#[command(name = "glyphvane", version, args_conflicts_with_subcommands = true)]
pub struct Cli {
    /// A command that runs without a window.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Theme for this run, over `appearance.theme` in the settings. `system`
    /// follows the operating system's appearance.
    #[arg(long, value_enum)]
    pub theme: Option<ThemeArg>,

    /// Keep settings and the app's other files below this folder instead of
    /// the platform's locations, for testing: settings are read from
    /// `DIR/config/settings.toml`. A separate folder is a separate instance.
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,

    /// Start in safe mode: conservative graphics settings (Mesa's software
    /// renderers on Linux, no DirectComposition on Windows), the default theme
    /// and no remote content, without changing the settings. Safe mode runs on
    /// its own: it does not hand its files to a running Glyphvane.
    #[arg(long)]
    pub safe_mode: bool,

    /// Open the files in a new window, also when Glyphvane is already
    /// running.
    #[arg(long)]
    pub new_window: bool,

    /// Open the files for editing and wait until they are closed, for use as
    /// `$EDITOR` or `GIT_EDITOR`. They open in a new window, in the editor
    /// rather than Preview. Glyphvane waits until each file's tab is closed
    /// (or its window), with the file saved when autosave is on. When
    /// Glyphvane was not running, this process is the one that starts it,
    /// and it quits once the files are closed.
    #[arg(long)]
    pub wait: bool,

    /// Files or folders to open. Files open in Preview; `PATH:LINE` and
    /// `PATH:LINE:COL` scroll to that line. When Glyphvane is already running,
    /// they open as tabs in its most recently used window.
    #[arg(value_name = "PATH[:LINE[:COL]]")]
    pub paths: Vec<OsString>,
}

/// The commands that run without starting the GUI.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Render a Markdown file to HTML or to a syntax tree dump, without
    /// opening a window.
    ///
    /// Exit status: 0 on success, 1 when the input cannot be read (or is
    /// binary) or the output cannot be written, 2 for a usage error or an
    /// unsupported profile, 3 when the input does not decode exactly and
    /// `--strict` is given.
    Render(RenderArgs),
}

/// Arguments of `glyphvane render`.
#[derive(Debug, clap::Args)]
pub struct RenderArgs {
    /// The Markdown file, or `-` for standard input. The encoding is
    /// detected: a byte-order mark, UTF-8, or a legacy encoding.
    #[arg(value_name = "IN")]
    pub input: PathBuf,

    /// Write to this file instead of standard output (`-` is standard
    /// output). The file is replaced atomically.
    #[arg(short, long, value_name = "OUT")]
    pub output: Option<PathBuf>,

    /// The flavor profile: `commonmark`, `gfm-file`, `gfm-comment`,
    /// `obsidian` or `extended`.
    #[arg(long, value_name = "P", default_value = "commonmark")]
    pub profile: String,

    /// What to write.
    #[arg(long, value_enum, default_value_t = RenderFormat::Html)]
    pub format: RenderFormat,

    /// Fail (status 3) instead of warning when the input does not decode
    /// exactly, for example invalid UTF-8 or malformed UTF-16.
    #[arg(long)]
    pub strict: bool,
}

/// Output formats of `glyphvane render`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RenderFormat {
    /// HTML for reading. For now the same as `commonmark-html`.
    // TODO(M8): switch to the rich HTML emitter of the exporter (classes,
    // source positions, heading ids) once it exists.
    Html,
    /// CommonMark-exact HTML, as the reference implementation writes it.
    CommonmarkHtml,
    /// The concrete syntax tree as a deterministic S-expression dump.
    Cst,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ThemeArg {
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

/// What one invocation asks to open, with every path absolute. It is what
/// a second instance sends to the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    /// The working directory of the invocation.
    pub cwd: PathBuf,
    pub files: Vec<OpenTarget>,
    /// Folders to open as vaults.
    pub folders: Vec<PathBuf>,
    pub new_window: bool,
    /// Block until the files are closed (`--wait`).
    pub wait: bool,
}

impl OpenRequest {
    /// The request of the command line `cli`, run in `cwd`.
    pub fn from_cli(cli: &Cli, cwd: &Path) -> Self {
        let mut request = Self {
            cwd: cwd.to_path_buf(),
            files: Vec::new(),
            folders: Vec::new(),
            new_window: cli.new_window,
            wait: cli.wait,
        };
        for arg in &cli.paths {
            let (path, position) = parse_location(arg, cfg!(windows), |p| cwd.join(p).exists());
            let path = absolute(cwd, &path);
            if path.is_dir() {
                request.folders.push(path);
            } else {
                request.files.push(OpenTarget { path, position });
            }
        }
        request
    }
}

/// `path` resolved against `cwd`, lexically: symbolic links are kept as given.
pub fn absolute(cwd: &Path, path: &Path) -> PathBuf {
    let joined = cwd.join(path);
    std::path::absolute(&joined).unwrap_or(joined)
}

/// Splits `PATH[:LINE[:COL]]` into the path and the position.
///
/// A path that exists as given is taken whole, so names that contain `:12`
/// keep working. Otherwise a trailing `:LINE:COL` or `:LINE` is split off,
/// preferring a split whose path exists. Line and column are 1-based; 0 is
/// read as 1. On Windows (`windows`), a drive letter alone is never the path
/// (`C:12` is a drive-relative path, not line 12 of `C`). Arguments that are
/// not Unicode are always taken whole.
pub fn parse_location(
    arg: &OsString,
    windows: bool,
    exists: impl Fn(&str) -> bool,
) -> (PathBuf, Option<TextPosition>) {
    let whole = || (PathBuf::from(arg), None);
    let Some(text) = arg.to_str() else {
        return whole();
    };
    if exists(text) {
        return whole();
    }
    let valid = |prefix: &str| !prefix.is_empty() && !(windows && is_drive_letter(prefix));

    let mut candidates = Vec::with_capacity(2);
    if let Some((rest, column)) = split_number(text)
        && let Some((path, line)) = split_number(rest)
        && valid(path)
    {
        candidates.push((path, line, Some(column)));
    }
    if let Some((path, line)) = split_number(text)
        && valid(path)
    {
        candidates.push((path, line, None));
    }
    let chosen = candidates
        .iter()
        .find(|(path, ..)| exists(path))
        .or_else(|| candidates.first());
    match chosen {
        Some(&(path, line, column)) => (
            PathBuf::from(path),
            Some(TextPosition {
                line: line.max(1),
                column: column.map(|c| c.max(1)),
            }),
        ),
        None => whole(),
    }
}

/// `text` as `REST:N`, with N a decimal number that fits `u32`.
fn split_number(text: &str) -> Option<(&str, u32)> {
    let (rest, digits) = text.rsplit_once(':')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((rest, digits.parse().ok()?))
}

fn is_drive_letter(text: &str) -> bool {
    text.len() == 1 && text.bytes().all(|b| b.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|err| panic!("{args:?}: {err}"))
    }

    type Expected<'a> = (&'a str, Option<(u32, Option<u32>)>);

    /// (argument, Windows rules, paths that exist, expected path and position)
    const CASES: &[(&str, bool, &[&str], Expected<'_>)] = &[
        ("a.md", false, &[], ("a.md", None)),
        ("a.md:12", false, &[], ("a.md", Some((12, None)))),
        ("a.md:12:3", false, &[], ("a.md", Some((12, Some(3))))),
        (
            "notes/a b.md:7",
            false,
            &[],
            ("notes/a b.md", Some((7, None))),
        ),
        (
            "/abs/a.md:1:1",
            false,
            &[],
            ("/abs/a.md", Some((1, Some(1)))),
        ),
        // Not a number, nothing to split.
        ("a.md:", false, &[], ("a.md:", None)),
        ("a.md:x", false, &[], ("a.md:x", None)),
        ("a.md:-1", false, &[], ("a.md:-1", None)),
        ("a.md:1x", false, &[], ("a.md:1x", None)),
        ("a.md:99999999999", false, &[], ("a.md:99999999999", None)),
        (":12", false, &[], (":12", None)),
        // Line and column 0 mean the first.
        ("a.md:0:0", false, &[], ("a.md", Some((1, Some(1))))),
        // A colon inside the path.
        ("dir:x/a.md:4", false, &[], ("dir:x/a.md", Some((4, None)))),
        // An existing file wins over a split.
        ("2024:11", false, &["2024:11"], ("2024:11", None)),
        ("log:12:3", false, &["log:12"], ("log:12", Some((3, None)))),
        ("log:12:3", false, &["log"], ("log", Some((12, Some(3))))),
        (
            "log:12:3",
            false,
            &["log", "log:12"],
            ("log", Some((12, Some(3)))),
        ),
        // Windows: drive letters and UNC paths.
        (r"C:\notes\a.md", true, &[], (r"C:\notes\a.md", None)),
        (
            r"C:\notes\a.md:12",
            true,
            &[],
            (r"C:\notes\a.md", Some((12, None))),
        ),
        (
            r"C:\notes\a.md:12:3",
            true,
            &[],
            (r"C:\notes\a.md", Some((12, Some(3)))),
        ),
        ("C:12", true, &[], ("C:12", None)),
        ("C:12:3", true, &[], ("C:12", Some((3, None)))),
        (
            r"\\server\share\a.md:5",
            true,
            &[],
            (r"\\server\share\a.md", Some((5, None))),
        ),
        // The drive-letter rule is Windows-only: on Unix, `C` is a file.
        ("C:12", false, &[], ("C", Some((12, None)))),
    ];

    #[test]
    fn locations_split_into_path_line_and_column() {
        for &(arg, windows, existing, (path, position)) in CASES {
            let got = parse_location(&OsString::from(arg), windows, |p| existing.contains(&p));
            let want = (
                PathBuf::from(path),
                position.map(|(line, column)| TextPosition { line, column }),
            );
            assert_eq!(got, want, "{arg:?} (windows: {windows})");
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_arguments_are_taken_whole() {
        use std::os::unix::ffi::OsStringExt as _;
        let arg = OsString::from_vec(b"a\xff.md:12".to_vec());
        assert_eq!(
            parse_location(&arg, false, |_| false),
            (PathBuf::from(&arg), None)
        );
    }

    #[test]
    fn render_is_a_subcommand() {
        let cli = parse(&[
            "glyphvane",
            "render",
            "a.md",
            "-o",
            "a.html",
            "--format",
            "cst",
        ]);
        let Some(Command::Render(args)) = cli.command else {
            panic!("render subcommand expected");
        };
        assert_eq!(args.input, PathBuf::from("a.md"));
        assert_eq!(args.output, Some(PathBuf::from("a.html")));
        assert_eq!(args.format, RenderFormat::Cst);
        assert_eq!(args.profile, "commonmark");
        assert!(!args.strict);
        assert!(cli.paths.is_empty());

        let defaults = parse(&["glyphvane", "render", "-"]);
        let Some(Command::Render(args)) = defaults.command else {
            panic!("render subcommand expected");
        };
        assert_eq!(args.format, RenderFormat::Html);
        assert_eq!(args.output, None);

        // After an option of the GUI, or written as a path, `render` is a
        // file to open.
        assert!(parse(&["glyphvane", "./render"]).command.is_none());
        let cli = parse(&["glyphvane", "--new-window", "render", "a.md"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.paths, ["render", "a.md"]);
        assert!(Cli::try_parse_from(["glyphvane", "render"]).is_err());
    }

    #[test]
    fn wait_is_carried_into_the_request() {
        let cwd = std::env::temp_dir();
        let request = |args: &[&str]| OpenRequest::from_cli(&parse(args), &cwd);
        assert!(!request(&["glyphvane", "a.md"]).wait);
        let waiting = request(&["glyphvane", "--wait", "COMMIT_EDITMSG"]);
        assert!(waiting.wait);
        assert_eq!(waiting.files, [OpenTarget::new(cwd.join("COMMIT_EDITMSG"))]);
    }

    #[test]
    fn theme_is_taken_from_settings_unless_given() {
        assert!(parse(&["glyphvane"]).theme.is_none());
        let cli = parse(&["glyphvane", "--theme", "semi-light"]);
        assert_eq!(
            cli.theme.map(ThemeChoice::from),
            Some(ThemeChoice::SemiLight)
        );
    }

    #[test]
    fn paths_become_absolute_files_and_folders() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cwd = dir.path();
        std::fs::create_dir(cwd.join("vault")).expect("folder");
        std::fs::write(cwd.join("a.md"), "# A\n").expect("file");
        let cli = parse(&["glyphvane", "--new-window", "a.md:3", "vault", "missing.md"]);
        let request = OpenRequest::from_cli(&cli, cwd);
        assert!(request.new_window);
        assert_eq!(request.folders, [cwd.join("vault")]);
        assert_eq!(
            request.files,
            [
                OpenTarget::new(cwd.join("a.md")).at(TextPosition {
                    line: 3,
                    column: None
                }),
                OpenTarget::new(cwd.join("missing.md")),
            ]
        );
    }
}
