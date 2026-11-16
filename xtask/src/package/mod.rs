// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `cargo xtask package`: release artifacts, one focused tool per format.
//!
//! | Format     | Tool                                                    |
//! |------------|---------------------------------------------------------|
//! | `deb`      | cargo-deb (`[package.metadata.deb]` in gv-app)          |
//! | `rpm`      | cargo-generate-rpm (`[package.metadata.generate-rpm]`)  |
//! | `tar.gz`   | here (reproducible tar + gzip)                          |
//! | `appimage` | here: AppDir, `mksquashfs`, the pinned type2-runtime    |
//! | `app`      | here: Info.plist template, `iconutil`, ad-hoc `codesign`|
//! | `zip`      | here (Windows); `ditto` (macOS, around the `.app`)      |
//! | `dmg`      | `hdiutil`                                               |
//! | `msi`      | WiX v6 `wix build` from `packaging/windows/glyphvane.wxs` |
//!
//! Every artifact is built from the same binaries (`[profile.dist]` by
//! default) with `SOURCE_DATE_EPOCH` set to the commit time, so the Linux
//! artifacts are byte-identical across runs. External tools are never
//! installed or downloaded here: a missing tool is reported with the command
//! that installs the pinned version.

mod archive;
mod linux;
mod macos;
pub(crate) mod size;
mod windows;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::cargo;

/// The application id (desktop file, metainfo, icons, bundle id).
pub(crate) const APP_ID: &str = "io.github.sorrelfox.Glyphvane";
/// The main executable.
const MAIN_BIN: &str = "glyphvane";
/// The helper executable (ADR-020, layout H2). Optional until `gv-helper`
/// gains its `[[bin]]`; every format packages it when it is built.
const HELPER_BIN: &str = "glyphvane-helper";
/// The package that owns the helper binary.
const HELPER_PACKAGE: &str = "gv-helper";
/// The checksums file written next to the artifacts.
const CHECKSUMS: &str = "SHA256SUMS";

const USAGE: &str = "\
Usage: cargo xtask package --format FORMAT[,FORMAT...] [options]

Builds release artifacts from the glyphvane binaries.

Formats:
  Linux targets    deb, rpm, tar.gz, appimage
  macOS targets    app, zip, dmg       (zip and dmg contain the .app)
  Windows targets  zip, msi

Options:
  --target TRIPLE        Target triple (default: the host)
  --profile NAME         Cargo profile (default: dist)
  --out DIR              Output directory (default: <target-dir>/package/<triple>)
  --no-build             Use the binaries already built for the target/profile
  --appimage-runtime F   AppImage type2-runtime file to prepend; checked against
                         the pinned SHA-256 (also GLYPHVANE_APPIMAGE_RUNTIME).
                         Without it only the squashfs image is written.
  --no-sign              macOS: skip the ad-hoc codesign of the .app

SOURCE_DATE_EPOCH defaults to the time of the last git commit. Writes
SHA256SUMS over every file in the output directory.
";

/// Artifact formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Deb,
    Rpm,
    TarGz,
    AppImage,
    App,
    Zip,
    Dmg,
    Msi,
}

impl Format {
    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "deb" => Self::Deb,
            "rpm" => Self::Rpm,
            "tar.gz" | "tgz" | "tarball" => Self::TarGz,
            "appimage" | "AppImage" => Self::AppImage,
            "app" => Self::App,
            "zip" => Self::Zip,
            "dmg" => Self::Dmg,
            "msi" => Self::Msi,
            other => bail!(
                "unknown format `{other}` (expected deb, rpm, tar.gz, appimage, app, zip, dmg or msi)"
            ),
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Deb => "deb",
            Self::Rpm => "rpm",
            Self::TarGz => "tar.gz",
            Self::AppImage => "appimage",
            Self::App => "app",
            Self::Zip => "zip",
            Self::Dmg => "dmg",
            Self::Msi => "msi",
        }
    }

    fn supported_on(self, os: Os) -> bool {
        match os {
            Os::Linux => matches!(self, Self::Deb | Self::Rpm | Self::TarGz | Self::AppImage),
            Os::Mac => matches!(self, Self::App | Self::Zip | Self::Dmg),
            Os::Windows => matches!(self, Self::Zip | Self::Msi),
        }
    }
}

/// Target operating systems.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Os {
    Linux,
    Mac,
    Windows,
}

/// Target architectures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arch {
    X86_64,
    Aarch64,
}

#[derive(Debug)]
struct Options {
    target: Option<String>,
    formats: Vec<Format>,
    profile: String,
    out: Option<PathBuf>,
    build: bool,
    appimage_runtime: Option<PathBuf>,
    sign: bool,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut o = Options {
        target: None,
        formats: Vec::new(),
        profile: "dist".to_owned(),
        out: None,
        build: true,
        appimage_runtime: std::env::var_os("GLYPHVANE_APPIMAGE_RUNTIME").map(PathBuf::from),
        sign: true,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = || -> Result<String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => it
                    .next()
                    .cloned()
                    .with_context(|| format!("`{flag}` needs a value")),
            }
        };
        match flag {
            "--target" => o.target = Some(value()?),
            "--format" | "-f" => {
                for f in value()?.split(',').filter(|f| !f.is_empty()) {
                    let f = Format::parse(f)?;
                    if !o.formats.contains(&f) {
                        o.formats.push(f);
                    }
                }
            }
            "--profile" => o.profile = value()?,
            "--out" | "-o" => o.out = Some(PathBuf::from(value()?)),
            "--no-build" => o.build = false,
            "--appimage-runtime" => o.appimage_runtime = Some(PathBuf::from(value()?)),
            "--no-sign" => o.sign = false,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown option `{other}`\n\n{USAGE}"),
        }
    }
    if o.formats.is_empty() {
        bail!("no --format given\n\n{USAGE}");
    }
    Ok(o)
}

/// Everything a format needs to build its artifact.
#[derive(Debug)]
pub(crate) struct Ctx {
    pub(crate) root: PathBuf,
    pub(crate) target: String,
    /// Whether the binaries are below `<target-dir>/<triple>/`, so the cargo
    /// packaging tools must be given `--target`. A host build without
    /// `--target` puts them in `<target-dir>/<profile>/` instead.
    pub(crate) cross_layout: bool,
    pub(crate) profile: String,
    pub(crate) os: Os,
    pub(crate) arch: Arch,
    /// The package version, as in Cargo.toml (`1.2.0-beta.1`).
    pub(crate) version: String,
    /// `SOURCE_DATE_EPOCH`: every timestamp written into an artifact.
    pub(crate) epoch: u64,
    pub(crate) out: PathBuf,
    /// Cargo's target directory.
    pub(crate) target_dir: PathBuf,
    /// Scratch space for staging trees, below the target directory.
    pub(crate) work: PathBuf,
    pub(crate) main_exe: PathBuf,
    pub(crate) helper_exe: Option<PathBuf>,
    pub(crate) appimage_runtime: Option<PathBuf>,
    pub(crate) sign: bool,
}

impl Ctx {
    /// `x.y.z` without pre-release or build metadata, for the formats whose
    /// version fields are numeric (MSI, CFBundleVersion).
    pub(crate) fn numeric_version(&self) -> String {
        numeric_version(&self.version)
    }

    /// A packaging-support file below `packaging/`.
    pub(crate) fn packaging(&self, rel: &str) -> PathBuf {
        self.root.join("packaging").join(rel)
    }

    /// The executable file name for this target.
    pub(crate) fn exe_name(&self, stem: &str) -> String {
        if self.os == Os::Windows {
            format!("{stem}.exe")
        } else {
            stem.to_owned()
        }
    }

    /// A clean scratch directory below `work`.
    pub(crate) fn scratch(&self, name: &str) -> Result<PathBuf> {
        let dir = self.work.join(name);
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(dir)
    }

    /// A command with `SOURCE_DATE_EPOCH` set, run from the workspace root.
    pub(crate) fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(&self.root)
            .env("SOURCE_DATE_EPOCH", self.epoch.to_string());
        cmd
    }
}

pub(crate) fn run(root: &Path, args: &[String]) -> Result<bool> {
    let opts = parse_args(args)?;
    let target_given = opts.target.is_some();
    let target = match opts.target {
        Some(t) => t,
        None => host_triple()?,
    };
    let (os, arch) = parse_triple(&target)?;
    for f in &opts.formats {
        if !f.supported_on(os) {
            bail!("format `{}` is not built for target `{target}`", f.name());
        }
    }

    let meta = Metadata::load(root)?;
    let epoch = source_date_epoch(root)?;
    eprintln!(
        "==> glyphvane {} for {target} ({} profile), SOURCE_DATE_EPOCH={epoch}",
        meta.version, opts.profile
    );

    if opts.build {
        build(root, &target, &opts.profile, meta.helper_is_bin)?;
    }

    let profile_dir = if opts.profile == "dev" || opts.profile == "test" {
        "debug"
    } else {
        opts.profile.as_str()
    };
    // `cargo build --target T` writes to `<target-dir>/T/<profile>`; a plain
    // host build writes to `<target-dir>/<profile>`. Accept the latter for
    // host packaging with --no-build when the former has no binary.
    let triple_dir = meta.target_dir.join(&target).join(profile_dir);
    let host_dir = meta.target_dir.join(profile_dir);
    let main_name = if os == Os::Windows {
        format!("{MAIN_BIN}.exe")
    } else {
        MAIN_BIN.to_owned()
    };
    let cross_layout = opts.build
        || target_given
        || triple_dir.join(&main_name).is_file()
        || !host_dir.join(&main_name).is_file();
    let bin_dir = if cross_layout { triple_dir } else { host_dir };
    let exe = |stem: &str| {
        bin_dir.join(if os == Os::Windows {
            format!("{stem}.exe")
        } else {
            stem.to_owned()
        })
    };
    let main_exe = exe(MAIN_BIN);
    if !main_exe.is_file() {
        bail!(
            "{} not found; build it first (drop --no-build, or run \
             `cargo build --locked --profile {} --target {target} -p gv-app`)",
            main_exe.display(),
            opts.profile
        );
    }
    let helper_exe = if meta.helper_is_bin {
        let helper = exe(HELPER_BIN);
        if !helper.is_file() {
            bail!(
                "{} not found; build -p {HELPER_PACKAGE} too",
                helper.display()
            );
        }
        Some(helper)
    } else {
        None
    };

    let out = match opts.out {
        Some(o) if o.is_absolute() => o,
        Some(o) => std::env::current_dir()?.join(o),
        None => meta.target_dir.join("package").join(&target),
    };
    fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let work = meta.target_dir.join("package-work").join(&target);
    fs::create_dir_all(&work)?;

    let ctx = Ctx {
        root: root.to_path_buf(),
        target,
        cross_layout,
        profile: opts.profile,
        os,
        arch,
        version: meta.version,
        epoch,
        out,
        target_dir: meta.target_dir,
        work,
        main_exe,
        helper_exe,
        appimage_runtime: opts.appimage_runtime,
        sign: opts.sign,
    };

    let mut artifacts = Vec::new();
    let mut app_bundle: Option<PathBuf> = None;
    for format in &opts.formats {
        eprintln!("==> {}", format.name());
        let produced = match format {
            Format::Deb => vec![linux::deb(&ctx)?],
            Format::Rpm => vec![linux::rpm(&ctx)?],
            Format::TarGz => vec![linux::tarball(&ctx)?],
            Format::AppImage => vec![linux::appimage(&ctx)?],
            Format::App => {
                let app = macos::app_bundle(&ctx)?;
                app_bundle = Some(app.clone());
                vec![app]
            }
            Format::Zip if os == Os::Mac => {
                let app = cached_app(&ctx, &mut app_bundle)?;
                vec![macos::zip(&ctx, &app)?]
            }
            Format::Dmg => {
                let app = cached_app(&ctx, &mut app_bundle)?;
                vec![macos::dmg(&ctx, &app)?]
            }
            Format::Zip => vec![windows::zip(&ctx)?],
            Format::Msi => vec![windows::msi(&ctx)?],
        };
        artifacts.extend(produced);
    }

    let sums = write_checksums(&ctx.out)?;
    eprintln!("==> {}", sums.display());
    report(&ctx, &artifacts)?;
    Ok(true)
}

fn cached_app(ctx: &Ctx, cache: &mut Option<PathBuf>) -> Result<PathBuf> {
    if let Some(app) = cache {
        return Ok(app.clone());
    }
    let app = macos::app_bundle(ctx)?;
    *cache = Some(app.clone());
    Ok(app)
}

/// The facts read from `cargo metadata`.
#[derive(Debug)]
struct Metadata {
    target_dir: PathBuf,
    version: String,
    helper_is_bin: bool,
}

impl Metadata {
    fn load(root: &Path) -> Result<Self> {
        let output = Command::new(cargo())
            .args([
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--offline",
            ])
            .current_dir(root)
            .output()
            .context("running `cargo metadata`")?;
        if !output.status.success() {
            bail!(
                "`cargo metadata` failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let json: serde_json::Value =
            serde_json::from_slice(&output.stdout).context("parsing `cargo metadata`")?;
        let target_dir = json["target_directory"]
            .as_str()
            .context("no target_directory in `cargo metadata`")?
            .into();
        let packages = json["packages"].as_array().context("no packages")?;
        let package = |name: &str| packages.iter().find(|p| p["name"] == name);
        let app = package("gv-app").context("no gv-app package")?;
        let version = app["version"]
            .as_str()
            .context("no gv-app version")?
            .to_owned();
        let has_bin = |p: &serde_json::Value, bin: &str| {
            p["targets"].as_array().is_some_and(|ts| {
                ts.iter().any(|t| {
                    t["name"] == bin
                        && t["kind"]
                            .as_array()
                            .is_some_and(|k| k.iter().any(|k| k == "bin"))
                })
            })
        };
        if !has_bin(app, MAIN_BIN) {
            bail!("gv-app has no `{MAIN_BIN}` binary");
        }
        let helper_is_bin = package(HELPER_PACKAGE).is_some_and(|p| has_bin(p, HELPER_BIN));
        Ok(Self {
            target_dir,
            version,
            helper_is_bin,
        })
    }
}

fn build(root: &Path, target: &str, profile: &str, with_helper: bool) -> Result<()> {
    let mut cmd = Command::new(cargo());
    cmd.args([
        "build",
        "--locked",
        "--profile",
        profile,
        "--target",
        target,
        "-p",
        "gv-app",
    ]);
    if with_helper {
        cmd.args(["-p", HELPER_PACKAGE]);
    }
    eprintln!("==> cargo build --locked --profile {profile} --target {target} -p gv-app");
    let status = cmd
        .current_dir(root)
        .status()
        .context("running `cargo build`")?;
    if !status.success() {
        bail!("`cargo build` failed");
    }
    Ok(())
}

fn host_triple() -> Result<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let output = Command::new(rustc)
        .arg("-vV")
        .output()
        .context("running `rustc -vV`")?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_owned)
        .context("no host triple in `rustc -vV`")
}

fn parse_triple(triple: &str) -> Result<(Os, Arch)> {
    let arch = match triple.split('-').next() {
        Some("x86_64") => Arch::X86_64,
        Some("aarch64") => Arch::Aarch64,
        _ => bail!("unsupported target architecture in `{triple}`"),
    };
    let os = if triple.contains("-linux-") {
        Os::Linux
    } else if triple.contains("-apple-darwin") {
        Os::Mac
    } else if triple.contains("-windows-") {
        Os::Windows
    } else {
        bail!("unsupported target OS in `{triple}`");
    };
    Ok((os, arch))
}

/// `SOURCE_DATE_EPOCH` from the environment, else the last commit's time.
fn source_date_epoch(root: &Path) -> Result<u64> {
    if let Ok(v) = std::env::var("SOURCE_DATE_EPOCH") {
        return v
            .trim()
            .parse()
            .with_context(|| format!("SOURCE_DATE_EPOCH is not a number: `{v}`"));
    }
    let output = Command::new("git")
        .args(["log", "-1", "--format=%ct"])
        .current_dir(root)
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse()
            .context("parsing the commit time from `git log`"),
        _ => bail!(
            "no SOURCE_DATE_EPOCH and no git history; set it, for example to \
             the commit time: SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)"
        ),
    }
}

pub(crate) fn numeric_version(version: &str) -> String {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    core.to_owned()
}

/// Fails with an install hint when `program` cannot be started.
pub(crate) fn require_tool(program: &str, version_arg: &str, hint: &str) -> Result<()> {
    match Command::new(program).arg(version_arg).output() {
        Ok(_) => Ok(()),
        Err(_) => bail!("`{program}` is not installed. {hint}"),
    }
}

/// Runs a command and fails with its output when it does not succeed. On
/// success only its standard error (warnings) is shown.
pub(crate) fn run_checked(cmd: &mut Command, what: &str) -> Result<()> {
    let output = cmd.output().with_context(|| format!("running {what}"))?;
    if !output.status.success() {
        bail!(
            "{what} failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines().filter(|l| !l.trim().is_empty()) {
        eprintln!("  {what}: {line}");
    }
    Ok(())
}

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Writes `SHA256SUMS` (`sha256sum` format) over the regular files in `dir`.
fn write_checksums(dir: &Path) -> Result<PathBuf> {
    let mut names: Vec<String> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != CHECKSUMS)
        .collect();
    names.sort();
    let mut text = String::new();
    for name in &names {
        let _ = writeln!(text, "{}  {name}", sha256_file(&dir.join(name))?);
    }
    let path = dir.join(CHECKSUMS);
    fs::write(&path, text)?;
    Ok(path)
}

/// Prints the artifacts and the binary sizes against the NFR-SIZE budgets.
fn report(ctx: &Ctx, artifacts: &[PathBuf]) -> Result<()> {
    eprintln!("==> artifacts");
    for a in artifacts {
        let bytes = if a.is_dir() {
            dir_size(a)?
        } else {
            fs::metadata(a)?.len()
        };
        eprintln!("  {:>12} B  {:>7.2} MB  {}", bytes, mb(bytes), a.display());
    }
    eprintln!("==> binaries (stripped, {} profile)", ctx.profile);
    for (exe, budget) in [
        (Some(&ctx.main_exe), size::MAIN_BUDGET),
        (ctx.helper_exe.as_ref(), size::HELPER_BUDGET),
    ] {
        if let Some(exe) = exe {
            let bytes = fs::metadata(exe)?.len();
            eprintln!(
                "  {:>12} B  {:>7.2} MB  (budget {} MB)  {}",
                bytes,
                mb(bytes),
                budget / 1_000_000,
                exe.display()
            );
        }
    }
    Ok(())
}

pub(crate) fn mb(bytes: u64) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "sizes are far below 2^52")]
    let b = bytes as f64;
    b / 1e6
}

fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let t = entry.file_type()?;
        if t.is_dir() {
            total += dir_size(&entry.path())?;
        } else if t.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_formats_and_triples() {
        let o = parse_args(&[
            "--format".into(),
            "deb,tar.gz".into(),
            "--format=rpm".into(),
        ])
        .expect("valid args");
        assert_eq!(o.formats, [Format::Deb, Format::TarGz, Format::Rpm]);
        assert_eq!(o.profile, "dist");
        assert!(parse_args(&["--format".into(), "exe".into()]).is_err());
        assert!(parse_args(&[]).is_err());

        assert_eq!(
            parse_triple("x86_64-unknown-linux-gnu").ok(),
            Some((Os::Linux, Arch::X86_64))
        );
        assert_eq!(
            parse_triple("aarch64-apple-darwin").ok(),
            Some((Os::Mac, Arch::Aarch64))
        );
        assert_eq!(
            parse_triple("aarch64-pc-windows-msvc").ok(),
            Some((Os::Windows, Arch::Aarch64))
        );
        assert!(parse_triple("riscv64gc-unknown-linux-gnu").is_err());
        assert!(!Format::Deb.supported_on(Os::Mac));
        assert!(Format::Zip.supported_on(Os::Windows));
    }

    #[test]
    fn numeric_versions() {
        assert_eq!(numeric_version("1.2.3"), "1.2.3");
        assert_eq!(numeric_version("1.2.3-beta.1"), "1.2.3");
        assert_eq!(numeric_version("1.2.3+build.5"), "1.2.3");
    }
}
