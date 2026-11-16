// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Which profile a document uses, and why.
//!
//! Precedence, highest first:
//!
//! 1. a per-file override (app settings);
//! 2. the nearest `.glyphvane.toml` that sets `profile`;
//! 3. a per-folder override (nearest folder wins), then a per-vault
//!    override (app settings);
//! 4. the file extension: `.mdx`, `.qmd`, `.rmd`;
//! 5. the nearest ancestor holding a tool's configuration: an Obsidian
//!    config folder, `mkdocs.yml`, `docusaurus.config.*`, `_quarto.yml`,
//!    `.vitepress/`, Hugo or Jekyll configuration, `.gitlab-ci.yml`;
//! 6. the nearest Git repository: a `github.com` remote means GFM (files),
//!    a GitLab remote means GitLab;
//! 7. the default profile (Extended unless changed).
//!
//! A step that names a profile this build does not have yet (a planned
//! dialect, or Obsidian before it ships) still decides: the document uses
//! Extended and the status bar says what was found.
//!
//! Front matter is not a signal: no common key exists, and inventing one
//! would put Glyphvane-specific keys in users' files.
//!
//! The `[switches]` of the nearest `.glyphvane.toml` apply to whatever
//! profile is chosen, except a per-file override, which is taken as is.
//!
//! The file-system part ([`FolderScan`]) only checks for names; it reads
//! nothing but `.glyphvane.toml` and `.git/config`. [`Detector`] caches it
//! per folder.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::compile::{CompiledProfile, SwitchOverrides, Tuning, compile};
use crate::config::{ConfigDiagnostic, ConfigFile, FILE_NAME, FolderConfig, Severity};
use crate::id::{PlannedProfile, ProfileId, Requested};

/// The overrides kept in app settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    /// Set for this file.
    pub file: Option<ProfileId>,
    /// Set for folders (absolute paths); the nearest ancestor of the file
    /// wins.
    pub folders: Vec<(PathBuf, ProfileId)>,
    /// Set for the vault the file is in.
    pub vault: Option<ProfileId>,
}

/// Settings that change what detection looks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectOptions {
    /// The profile when nothing is detected.
    pub default: ProfileId,
    /// More names of Obsidian config folders, besides `.obsidian` and any
    /// dot-folder that holds both `app.json` and `core-plugins.json`.
    pub obsidian_config_folders: Vec<String>,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            default: ProfileId::Extended,
            obsidian_config_folders: Vec::new(),
        }
    }
}

/// A tool whose configuration marks a folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Tool {
    /// An Obsidian vault; the config folder's name.
    Obsidian(String),
    MkDocs,
    Docusaurus,
    Quarto,
    VitePress,
    Hugo,
    Jekyll,
    GitLabCi,
}

impl Tool {
    /// The profile the tool means.
    #[must_use]
    pub fn requested(&self) -> Requested {
        match self {
            Self::Obsidian(_) => Requested::BuiltIn(ProfileId::Obsidian),
            Self::MkDocs => Requested::Planned(PlannedProfile::MkDocs),
            Self::Docusaurus => Requested::Planned(PlannedProfile::Docusaurus),
            Self::Quarto => Requested::Planned(PlannedProfile::Quarto),
            Self::VitePress => Requested::Planned(PlannedProfile::VitePress),
            Self::Hugo => Requested::Planned(PlannedProfile::Hugo),
            Self::Jekyll => Requested::Planned(PlannedProfile::Kramdown),
            Self::GitLabCi => Requested::Planned(PlannedProfile::GitLab),
        }
    }
}

/// The Git hosts detection tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitHost {
    GitHub,
    GitLab,
    /// No remote, or one on another host.
    Other,
}

/// Why a profile was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// A per-file override.
    FileOverride,
    /// `profile` in this `.glyphvane.toml`.
    FolderConfig(PathBuf),
    /// A per-folder override set for this folder.
    FolderOverride(PathBuf),
    /// A per-vault override.
    VaultOverride,
    /// The file extension (without the dot, lower case).
    Extension(String),
    /// A tool's configuration: the tool, the name found and its folder.
    ToolConfig {
        tool: Tool,
        marker: String,
        folder: PathBuf,
    },
    /// The repository at this folder has a remote on this host.
    GitRemote { host: GitHost, repo: PathBuf },
    /// Nothing was found.
    Default,
}

impl Reason {
    /// One sentence for the chip's tooltip, with paths under `home`
    /// written as `~/…`: "Found `.obsidian/` at `~/Notes`".
    #[must_use]
    pub fn describe(&self, home: Option<&Path>) -> String {
        let at = |p: &Path| tilde(p, home);
        match self {
            Self::FileOverride => "Set for this file".to_owned(),
            Self::FolderConfig(path) => format!("Set in `{}`", at(path)),
            Self::FolderOverride(folder) => format!("Set for the folder `{}`", at(folder)),
            Self::VaultOverride => "Set for this vault".to_owned(),
            Self::Extension(ext) => format!("A `.{ext}` file"),
            Self::ToolConfig { marker, folder, .. } => {
                format!("Found `{marker}` at `{}`", at(folder))
            }
            Self::GitRemote { host, repo } => {
                let host = match host {
                    GitHost::GitHub => "GitHub",
                    GitHost::GitLab => "GitLab",
                    GitHost::Other => "another host",
                };
                format!("Git repository at `{}` with a {host} remote", at(repo))
            }
            Self::Default => "Nothing detected: the default profile".to_owned(),
        }
    }
}

/// `path` with `home` written as `~`.
fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// What detection found for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    /// What the deciding step asked for.
    pub requested: Requested,
    /// The profile the document uses.
    pub profile: ProfileId,
    pub reason: Reason,
    /// The nearest `.glyphvane.toml`, with its diagnostics, whether or not
    /// it decided.
    pub config: Option<Arc<ConfigFile>>,
    /// Whether the config's `[switches]` apply.
    pub switches_apply: bool,
}

impl Detection {
    /// "MkDocs detected — full support in 1.2" when the requested profile
    /// has not shipped.
    #[must_use]
    pub fn pending_note(&self) -> Option<String> {
        self.requested.pending_note()
    }

    /// The switch overrides in effect.
    #[must_use]
    pub fn switch_overrides(&self) -> SwitchOverrides {
        match &self.config {
            Some(file) if self.switches_apply => file.config.switches.clone(),
            _ => SwitchOverrides::new(),
        }
    }

    /// Compiles the chosen profile with the overrides in effect.
    #[must_use]
    pub fn compile(&self) -> CompiledProfile {
        compile(self.profile, Tuning::default(), &self.switch_overrides())
    }

    /// The `.glyphvane.toml` problems, for the status bar and the log.
    #[must_use]
    pub fn config_diagnostics(&self) -> &[ConfigDiagnostic] {
        self.config
            .as_ref()
            .map_or(&[], |f| f.diagnostics.as_slice())
    }

    /// Every line of "Explain detection": the profile, the reason, a note
    /// for a profile still to ship, switch overrides and config problems.
    #[must_use]
    pub fn explain(&self, home: Option<&Path>) -> Vec<String> {
        let mut lines = vec![
            format!(
                "Profile: {} ({})",
                crate::builtin::definition(self.profile).name,
                self.profile
            ),
            format!("{}.", self.reason.describe(home)),
        ];
        lines.extend(
            self.pending_note()
                .map(|n| format!("{n}; Extended is used until then.")),
        );
        let overrides = self.switch_overrides();
        if !overrides.is_empty() {
            let list: Vec<String> = overrides
                .iter()
                .map(|(k, v)| format!("{k} = {v}"))
                .collect();
            lines.push(format!(
                "Switches from `.glyphvane.toml`: {}.",
                list.join(", ")
            ));
        }
        if let Some(file) = &self.config {
            for d in &file.diagnostics {
                lines.push(format!("{}:{d}", tilde(&file.path, home)));
            }
        }
        lines
    }
}

/// What the file system says about one folder and its ancestors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderScan {
    /// The nearest `.glyphvane.toml`.
    pub config: Option<Arc<ConfigFile>>,
    /// The nearest tool configuration.
    pub tool: Option<(Tool, String, PathBuf)>,
    /// The nearest Git repository and its remote's host.
    pub git: Option<(GitHost, PathBuf)>,
}

/// Looks at `folder` (absolute) and its ancestors.
#[must_use]
pub fn scan_folder(folder: &Path, options: &DetectOptions) -> FolderScan {
    let mut scan = FolderScan::default();
    for dir in folder.ancestors() {
        if scan.config.is_none() {
            let path = dir.join(FILE_NAME);
            if path.is_file() {
                scan.config = Some(Arc::new(read_config(&path)));
            }
        }
        if scan.tool.is_none() {
            scan.tool =
                tool_in(dir, options).map(|(tool, marker)| (tool, marker, dir.to_path_buf()));
        }
        if scan.git.is_none() && dir.join(".git").exists() {
            scan.git = Some((git_host(dir), dir.to_path_buf()));
        }
        if scan.config.is_some() && scan.tool.is_some() && scan.git.is_some() {
            break;
        }
    }
    scan
}

/// A config file, or one error saying why it could not be read.
fn read_config(path: &Path) -> ConfigFile {
    ConfigFile::read(path).unwrap_or_else(|e| ConfigFile {
        path: path.to_path_buf(),
        config: FolderConfig::default(),
        diagnostics: vec![ConfigDiagnostic {
            severity: Severity::Error,
            line: 1,
            column: 1,
            key: None,
            message: format!("could not read the file: {e}"),
        }],
    })
}

/// The tool configuration directly in `dir`, and the name that shows it.
fn tool_in(dir: &Path, options: &DetectOptions) -> Option<(Tool, String)> {
    if let Some(name) = obsidian_config(dir, options) {
        return Some((Tool::Obsidian(name.clone()), format!("{name}/")));
    }
    let file = |names: &[&str], tool: Tool| {
        names
            .iter()
            .find(|n| dir.join(n).is_file())
            .map(|n| (tool, (*n).to_owned()))
    };
    file(&["mkdocs.yml", "mkdocs.yaml"], Tool::MkDocs)
        .or_else(|| {
            file(
                &[
                    "docusaurus.config.js",
                    "docusaurus.config.ts",
                    "docusaurus.config.mjs",
                    "docusaurus.config.cjs",
                ],
                Tool::Docusaurus,
            )
        })
        .or_else(|| file(&["_quarto.yml", "_quarto.yaml"], Tool::Quarto))
        .or_else(|| {
            dir.join(".vitepress")
                .is_dir()
                .then(|| (Tool::VitePress, ".vitepress/".to_owned()))
        })
        .or_else(|| file(&["hugo.toml", "hugo.yaml", "hugo.json"], Tool::Hugo))
        .or_else(|| file(&["_config.yml"], Tool::Jekyll))
        .or_else(|| file(&[".gitlab-ci.yml"], Tool::GitLabCi))
}

/// The name of an Obsidian config folder directly in `dir`.
fn obsidian_config(dir: &Path, options: &DetectOptions) -> Option<String> {
    let names = std::iter::once(".obsidian")
        .chain(options.obsidian_config_folders.iter().map(String::as_str));
    for name in names {
        if dir.join(name).is_dir() {
            return Some(name.to_owned());
        }
    }
    // A renamed config folder: a top-level dot-folder with both files.
    let entries = std::fs::read_dir(dir).ok()?;
    let mut found: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|name| name.starts_with('.') && name != "." && name != "..")
        .filter(|name| {
            let d = dir.join(name);
            d.join("app.json").is_file() && d.join("core-plugins.json").is_file()
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// The host of the repository at `repo`'s remotes: GitHub if any remote
/// is on `github.com`, else GitLab if any is on a GitLab host.
fn git_host(repo: &Path) -> GitHost {
    let Some(text) = git_config(repo) else {
        return GitHost::Other;
    };
    let urls: Vec<String> = remote_urls(&text);
    if urls
        .iter()
        .any(|u| host_of(u).is_some_and(|h| h == "github.com"))
    {
        GitHost::GitHub
    } else if urls
        .iter()
        .any(|u| host_of(u).is_some_and(|h| h == "gitlab.com" || h.starts_with("gitlab.")))
    {
        GitHost::GitLab
    } else {
        GitHost::Other
    }
}

/// The text of the repository's `config`, following a worktree's `.git`
/// file (`gitdir:`) and its `commondir`.
fn git_config(repo: &Path) -> Option<String> {
    let dot_git = repo.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
        let dir = repo.join(target);
        match std::fs::read_to_string(dir.join("commondir")) {
            Ok(common) => dir.join(common.trim()),
            Err(_) => dir,
        }
    };
    std::fs::read_to_string(git_dir.join("config")).ok()
}

/// The `url` values of `[remote "…"]` sections.
fn remote_urls(config: &str) -> Vec<String> {
    let mut in_remote = false;
    let mut urls = Vec::new();
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_remote = line.starts_with("[remote ");
            continue;
        }
        if !in_remote {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            urls.push(value.trim().trim_matches('"').to_owned());
        }
    }
    urls
}

/// The lower-case host of a remote URL: `https://host/…`,
/// `ssh://user@host:port/…` or scp-like `user@host:path`.
fn host_of(url: &str) -> Option<String> {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url.split_once(':')?.0,
    };
    let authority = rest.split('/').next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// The profile for `file` (absolute) from a scan of its folder.
#[must_use]
pub fn resolve(
    file: &Path,
    scan: &FolderScan,
    overrides: &Overrides,
    options: &DetectOptions,
) -> Detection {
    let config = scan.config.clone();
    let make = |requested: Requested, reason: Reason| Detection {
        requested,
        profile: requested.effective(),
        switches_apply: reason != Reason::FileOverride,
        reason,
        config: config.clone(),
    };
    if let Some(id) = overrides.file {
        return make(id.into(), Reason::FileOverride);
    }
    if let Some(file_config) = &scan.config
        && let Some(requested) = file_config.config.profile
    {
        return make(requested, Reason::FolderConfig(file_config.path.clone()));
    }
    let folder = file.parent().unwrap_or(file);
    for dir in folder.ancestors() {
        if let Some((path, id)) = overrides.folders.iter().find(|(f, _)| f == dir) {
            return make((*id).into(), Reason::FolderOverride(path.clone()));
        }
    }
    if let Some(id) = overrides.vault {
        return make(id.into(), Reason::VaultOverride);
    }
    if let Some((requested, ext)) = by_extension(file) {
        return make(requested, Reason::Extension(ext));
    }
    if let Some((tool, marker, folder)) = &scan.tool {
        return make(
            tool.requested(),
            Reason::ToolConfig {
                tool: tool.clone(),
                marker: marker.clone(),
                folder: folder.clone(),
            },
        );
    }
    if let Some((host, repo)) = &scan.git {
        let requested = match host {
            GitHost::GitHub => Some(Requested::BuiltIn(ProfileId::GfmFile)),
            GitHost::GitLab => Some(Requested::Planned(PlannedProfile::GitLab)),
            GitHost::Other => None,
        };
        if let Some(requested) = requested {
            return make(
                requested,
                Reason::GitRemote {
                    host: *host,
                    repo: repo.clone(),
                },
            );
        }
    }
    make(options.default.into(), Reason::Default)
}

/// The profile a file extension means.
fn by_extension(file: &Path) -> Option<(Requested, String)> {
    let ext = file
        .extension()
        .and_then(OsStr::to_str)?
        .to_ascii_lowercase();
    let planned = match ext.as_str() {
        "mdx" => PlannedProfile::Docusaurus,
        "qmd" => PlannedProfile::Quarto,
        "rmd" => PlannedProfile::RMarkdown,
        _ => return None,
    };
    Some((Requested::Planned(planned), ext))
}

/// Detects the profile of `file` without a cache.
#[must_use]
pub fn detect(file: &Path, overrides: &Overrides, options: &DetectOptions) -> Detection {
    let file = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
    let folder = file.parent().unwrap_or(&file);
    let scan = scan_folder(folder, options);
    resolve(&file, &scan, overrides, options)
}

/// Detection with the file-system scan cached per folder. Call
/// [`Detector::invalidate`] when a `.glyphvane.toml`, a tool config or a
/// Git config changes.
#[derive(Debug, Default)]
pub struct Detector {
    options: DetectOptions,
    scans: HashMap<PathBuf, FolderScan>,
}

impl Detector {
    #[must_use]
    pub fn new(options: DetectOptions) -> Self {
        Self {
            options,
            scans: HashMap::new(),
        }
    }

    #[must_use]
    pub fn options(&self) -> &DetectOptions {
        &self.options
    }

    /// Changes the options; cached scans are dropped.
    pub fn set_options(&mut self, options: DetectOptions) {
        if options != self.options {
            self.options = options;
            self.scans.clear();
        }
    }

    /// The profile of `file`.
    pub fn detect(&mut self, file: &Path, overrides: &Overrides) -> Detection {
        let file = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
        let folder = file.parent().unwrap_or(&file).to_path_buf();
        let options = &self.options;
        let scan = self
            .scans
            .entry(folder)
            .or_insert_with_key(|folder| scan_folder(folder, options));
        resolve(&file, scan, overrides, &self.options)
    }

    /// Forgets the scans of `folder` and the folders below it (everything,
    /// for `None`).
    pub fn invalidate(&mut self, folder: Option<&Path>) {
        match folder {
            Some(folder) => self.scans.retain(|f, _| !f.starts_with(folder)),
            None => self.scans.clear(),
        }
    }
}

impl fmt::Display for Detection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.profile, self.reason.describe(None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(tempfile::TempDir);

    impl Tree {
        fn new() -> Self {
            Self(tempfile::tempdir().expect("temp dir"))
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.0.path().join(rel)
        }

        fn file(&self, rel: &str, text: &str) -> PathBuf {
            let path = self.path(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, text).expect("write");
            path
        }

        fn dir(&self, rel: &str) -> PathBuf {
            let path = self.path(rel);
            std::fs::create_dir_all(&path).expect("mkdir");
            path
        }

        fn git(&self, rel: &str, url: &str) {
            self.file(
                &format!("{rel}/.git/config"),
                &format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n"),
            );
        }
    }

    fn run(file: &Path, overrides: &Overrides) -> Detection {
        detect(file, overrides, &DetectOptions::default())
    }

    #[test]
    fn nothing_found_is_the_default() {
        let t = Tree::new();
        let note = t.file("a/note.md", "");
        let d = run(&note, &Overrides::default());
        assert_eq!(d.profile, ProfileId::Extended);
        assert_eq!(d.reason, Reason::Default);
        let options = DetectOptions {
            default: ProfileId::CommonMark,
            ..DetectOptions::default()
        };
        assert_eq!(
            detect(&note, &Overrides::default(), &options).profile,
            ProfileId::CommonMark
        );
    }

    #[test]
    fn a_github_remote_means_gfm_file() {
        let t = Tree::new();
        t.git("repo", "git@github.com:someone/project.git");
        let note = t.file("repo/docs/note.md", "");
        let d = run(&note, &Overrides::default());
        assert_eq!(d.profile, ProfileId::GfmFile);
        assert_eq!(
            d.reason,
            Reason::GitRemote {
                host: GitHost::GitHub,
                repo: t.path("repo")
            }
        );
        assert_eq!(
            d.reason.describe(Some(t.0.path())),
            "Git repository at `~/repo` with a GitHub remote"
        );

        t.git("lab", "https://gitlab.example.org/a/b.git");
        let lab = t.file("lab/n.md", "");
        let d = run(&lab, &Overrides::default());
        assert_eq!(d.requested, Requested::Planned(PlannedProfile::GitLab));
        assert_eq!(d.profile, ProfileId::Extended);
        assert_eq!(
            d.pending_note().as_deref(),
            Some("GitLab detected — full support in 1.3")
        );

        t.git("plain", "ssh://me@example.org:22/x.git");
        assert_eq!(
            run(&t.file("plain/n.md", ""), &Overrides::default()).reason,
            Reason::Default
        );
    }

    #[test]
    fn worktrees_follow_the_git_file() {
        let t = Tree::new();
        t.git("main", "https://github.com/a/b");
        t.dir("main/.git/worktrees/wt");
        t.file("main/.git/worktrees/wt/commondir", "../..\n");
        let wt = t.dir("wt");
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", t.path("main/.git/worktrees/wt").display()),
        )
        .expect("write");
        let d = run(&t.file("wt/n.md", ""), &Overrides::default());
        assert_eq!(d.profile, ProfileId::GfmFile);
    }

    #[test]
    fn tool_configs_beat_the_git_remote_and_the_nearest_wins() {
        let t = Tree::new();
        t.git("repo", "https://github.com/a/b");
        t.dir("repo/.obsidian");
        t.file("repo/site/mkdocs.yml", "site_name: x\n");
        let vault_note = t.file("repo/n.md", "");
        let d = run(&vault_note, &Overrides::default());
        assert_eq!(d.requested, Requested::BuiltIn(ProfileId::Obsidian));
        // Obsidian has not shipped: Extended, with a note.
        assert_eq!(d.profile, ProfileId::Extended);
        assert_eq!(
            d.reason.describe(Some(t.0.path())),
            "Found `.obsidian/` at `~/repo`"
        );
        assert_eq!(
            d.pending_note().as_deref(),
            Some("Obsidian detected — full support in 0.5")
        );
        let site_note = t.file("repo/site/docs/n.md", "");
        let d = run(&site_note, &Overrides::default());
        assert_eq!(d.requested, Requested::Planned(PlannedProfile::MkDocs));
    }

    #[test]
    fn renamed_obsidian_config_folders_are_found() {
        let t = Tree::new();
        t.file("v/.config-ob/app.json", "{}");
        t.file("v/.config-ob/core-plugins.json", "[]");
        t.file("w/.half/app.json", "{}");
        let d = run(&t.file("v/n.md", ""), &Overrides::default());
        assert_eq!(d.requested, Requested::BuiltIn(ProfileId::Obsidian));
        assert!(matches!(&d.reason, Reason::ToolConfig { marker, .. } if marker == ".config-ob/"));
        assert_eq!(
            run(&t.file("w/n.md", ""), &Overrides::default()).reason,
            Reason::Default
        );
        t.dir("x/vault-settings");
        let options = DetectOptions {
            obsidian_config_folders: vec!["vault-settings".into()],
            ..DetectOptions::default()
        };
        let d = detect(&t.file("x/n.md", ""), &Overrides::default(), &options);
        assert_eq!(d.requested, Requested::BuiltIn(ProfileId::Obsidian));
    }

    #[test]
    fn extensions_beat_tool_configs() {
        let t = Tree::new();
        t.file("site/docusaurus.config.ts", "");
        let d = run(&t.file("site/docs/a.md", ""), &Overrides::default());
        assert_eq!(d.requested, Requested::Planned(PlannedProfile::Docusaurus));
        t.dir("site/.obsidian");
        let d = run(&t.file("site/report.QMD", ""), &Overrides::default());
        assert_eq!(d.requested, Requested::Planned(PlannedProfile::Quarto));
        assert_eq!(d.reason, Reason::Extension("qmd".into()));
    }

    #[test]
    fn precedence_file_then_toml_then_folder_then_vault_then_detection() {
        let t = Tree::new();
        t.git("repo", "https://github.com/a/b");
        t.file(
            "repo/docs/.glyphvane.toml",
            "profile = \"commonmark\"\n[switches]\ntables = true\n",
        );
        let note = t.file("repo/docs/sub/n.md", "");
        let other = t.file("repo/src/n.md", "");
        let mut overrides = Overrides {
            file: Some(ProfileId::GfmComment),
            folders: vec![(t.path("repo/docs/sub"), ProfileId::Extended)],
            vault: Some(ProfileId::Extended),
        };

        // 1. The per-file override, without the toml's switches.
        let d = run(&note, &overrides);
        assert_eq!(
            (d.profile, &d.reason),
            (ProfileId::GfmComment, &Reason::FileOverride)
        );
        assert!(!d.switches_apply);
        assert!(d.compile().changed.is_empty());

        // 2. `.glyphvane.toml`, with its switches.
        overrides.file = None;
        let d = run(&note, &overrides);
        assert_eq!(d.profile, ProfileId::CommonMark);
        assert_eq!(
            d.reason,
            Reason::FolderConfig(t.path("repo/docs/.glyphvane.toml"))
        );
        assert_eq!(d.compile().changed, ["tables"]);

        // 3. The folder override, then the vault override.
        std::fs::write(
            t.path("repo/docs/.glyphvane.toml"),
            "[switches]\ntables = false\n",
        )
        .expect("write");
        overrides
            .folders
            .push((t.path("repo"), ProfileId::GfmComment));
        let d = run(&note, &overrides);
        assert_eq!(d.reason, Reason::FolderOverride(t.path("repo/docs/sub")));
        // A toml without `profile` still contributes its switches.
        assert_eq!(d.compile().changed, ["tables"]);
        let d = run(&other, &overrides);
        assert_eq!(
            (d.profile, &d.reason),
            (
                ProfileId::GfmComment,
                &Reason::FolderOverride(t.path("repo"))
            )
        );
        overrides.folders.clear();
        let d = run(&other, &overrides);
        assert_eq!(
            (d.profile, &d.reason),
            (ProfileId::Extended, &Reason::VaultOverride)
        );

        // 4. Detection.
        overrides.vault = None;
        assert_eq!(run(&other, &overrides).profile, ProfileId::GfmFile);
    }

    #[test]
    fn planned_profiles_in_toml_fall_back_with_a_note() {
        let t = Tree::new();
        t.file(".glyphvane.toml", "profile = \"mkdocs\"\nnope = 1\n");
        let d = run(&t.file("n.md", ""), &Overrides::default());
        assert_eq!(d.profile, ProfileId::Extended);
        assert_eq!(d.config_diagnostics().len(), 1);
        let explained = d.explain(None);
        assert_eq!(explained[0], "Profile: Extended (extended)");
        assert!(explained[2].starts_with("MkDocs detected — full support in 1.2"));
        assert!(
            explained[3].ends_with(".glyphvane.toml:2:1: warning: `nope`: unknown key; ignored")
        );
    }

    #[test]
    fn front_matter_is_not_a_signal() {
        let t = Tree::new();
        let note = t.file("n.md", "---\nprofile: commonmark\nflavor: gfm\n---\n# x\n");
        assert_eq!(run(&note, &Overrides::default()).reason, Reason::Default);
    }

    #[test]
    fn the_detector_caches_per_folder_until_invalidated() {
        let t = Tree::new();
        let note = t.file("a/n.md", "");
        let mut detector = Detector::new(DetectOptions::default());
        assert_eq!(
            detector.detect(&note, &Overrides::default()).reason,
            Reason::Default
        );
        t.file("a/.glyphvane.toml", "profile = \"gfm-file\"\n");
        assert_eq!(
            detector.detect(&note, &Overrides::default()).reason,
            Reason::Default
        );
        detector.invalidate(Some(&t.path("a")));
        assert_eq!(
            detector.detect(&note, &Overrides::default()).profile,
            ProfileId::GfmFile
        );
    }

    #[test]
    fn remote_hosts_are_parsed() {
        assert_eq!(
            host_of("git@github.com:a/b.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            host_of("https://GitHub.com/a/b").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            host_of("ssh://git@gitlab.com:2222/a").as_deref(),
            Some("gitlab.com")
        );
        assert_eq!(
            host_of("https://user:pw@github.com/a").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            remote_urls(
                "[remote \"a\"]\n url = x\n[branch \"m\"]\n url = y\n[remote \"b\"]\nurl=z"
            ),
            ["x", "z"]
        );
    }
}
