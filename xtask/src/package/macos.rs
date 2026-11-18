// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! macOS artifacts: `Glyphvane.app` (ad-hoc signed), and the `.zip` and
//! `.dmg` around it. The bundle can be assembled on any host for
//! inspection; the icon, signature, zip and disk image need macOS tools.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::archive::Tree;
use super::fonts::add_pdf_fonts;
use super::{APP_ID, Arch, Ctx, HELPER_BIN, MAIN_BIN, require_tool, run_checked};

const MACOS_TOOLS_HINT: &str = "It is part of macOS; build macOS artifacts on a Mac.";

fn arch_name(ctx: &Ctx) -> &'static str {
    match ctx.arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "arm64",
    }
}

/// Fills in `packaging/macos/Info.plist`.
fn info_plist(ctx: &Ctx) -> Result<Vec<u8>> {
    let path = ctx.packaging("macos/Info.plist");
    let template =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let version = ctx.numeric_version();
    let plist = template
        .replace("@SHORT_VERSION@", &version)
        .replace("@BUNDLE_VERSION@", &version);
    if let Some(i) = plist
        .find("<plist")
        .and_then(|start| plist[start..].find('@').map(|i| start + i))
    {
        let token: String = plist[i..].chars().take(32).collect();
        bail!("unfilled placeholder in {}: {token}", path.display());
    }
    Ok(plist.into_bytes())
}

/// Builds `Glyphvane.app` in the work directory and signs it ad hoc.
pub(super) fn app_bundle(ctx: &Ctx) -> Result<PathBuf> {
    let dir = ctx.scratch("macos")?;
    let app = dir.join("Glyphvane.app");
    fs::create_dir(&app)?;

    let mut tree = Tree::default();
    tree.bytes("Contents/Info.plist", info_plist(ctx)?, 0o644);
    tree.bytes("Contents/PkgInfo", b"APPL????".to_vec(), 0o644);
    tree.file(&format!("Contents/MacOS/{MAIN_BIN}"), &ctx.main_exe, 0o755);
    if let Some(helper) = &ctx.helper_exe {
        tree.file(&format!("Contents/MacOS/{HELPER_BIN}"), helper, 0o755);
    }
    tree.file(
        "Contents/Resources/LICENSE",
        &ctx.root.join("LICENSE"),
        0o644,
    );
    tree.file(
        "Contents/Resources/THIRD-PARTY-NOTICES.txt",
        &ctx.packaging("THIRD-PARTY-NOTICES.txt"),
        0o644,
    );
    add_pdf_fonts(ctx, &mut tree, "Contents/Resources/fonts");
    tree.materialize(&app, ctx.epoch)?;

    if tool_exists("iconutil") {
        let iconset = dir.join("glyphvane.iconset");
        fs::create_dir(&iconset)?;
        for (name, size) in [
            ("16x16", 16),
            ("16x16@2x", 32),
            ("32x32", 32),
            ("32x32@2x", 64),
            ("128x128", 128),
            ("128x128@2x", 256),
            ("256x256", 256),
            ("256x256@2x", 512),
            ("512x512", 512),
        ] {
            fs::copy(
                ctx.packaging(&format!("icons/{size}x{size}.png")),
                iconset.join(format!("icon_{name}.png")),
            )?;
        }
        let icns = app.join("Contents/Resources/glyphvane.icns");
        run_checked(
            Command::new("iconutil")
                .args(["-c", "icns", "-o"])
                .arg(&icns)
                .arg(&iconset),
            "iconutil",
        )?;
    } else {
        eprintln!("warning: `iconutil` not found (macOS only); the bundle has no icon");
    }

    if ctx.sign {
        sign(&app, ctx.helper_exe.is_some())?;
    }
    eprintln!("  {}", app.display());
    Ok(app)
}

/// Ad-hoc signature, inside-out: the helper, then the bundle (which signs
/// the main executable), then a strict verification. No Developer ID, no
/// notarization (plan: no Apple Developer Program).
fn sign(app: &Path, helper: bool) -> Result<()> {
    if !tool_exists("codesign") {
        if cfg!(target_os = "macos") {
            bail!("`codesign` is not available. {MACOS_TOOLS_HINT}");
        }
        eprintln!("warning: `codesign` runs on macOS only; the bundle is not signed");
        return Ok(());
    }
    if helper {
        let path = app.join("Contents/MacOS").join(HELPER_BIN);
        run_checked(
            Command::new("codesign")
                .args(["--force", "--sign", "-", "--identifier"])
                .arg(format!("{APP_ID}.helper"))
                .arg(&path),
            "codesign (helper)",
        )?;
    }
    run_checked(
        Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(app),
        "codesign (bundle)",
    )?;
    run_checked(
        Command::new("codesign")
            .args(["--verify", "--strict", "--verbose=2"])
            .arg(app),
        "codesign --verify",
    )
}

/// `Glyphvane-<version>-macos-<arch>.zip` with `ditto`, which keeps the
/// bundle's signature intact.
pub(super) fn zip(ctx: &Ctx, app: &Path) -> Result<PathBuf> {
    require_tool("ditto", "-h", MACOS_TOOLS_HINT)?;
    let out = ctx.out.join(format!(
        "Glyphvane-{}-macos-{}.zip",
        ctx.version,
        arch_name(ctx)
    ));
    if out.exists() {
        fs::remove_file(&out)?;
    }
    run_checked(
        Command::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(app)
            .arg(&out),
        "ditto",
    )?;
    Ok(out)
}

/// `Glyphvane-<version>-macos-<arch>.dmg`: the app, an `/Applications`
/// link and the first-launch instructions.
pub(super) fn dmg(ctx: &Ctx, app: &Path) -> Result<PathBuf> {
    require_tool("hdiutil", "help", MACOS_TOOLS_HINT)?;
    let stage = ctx.scratch("dmg")?;
    run_checked(
        Command::new("ditto")
            .arg(app)
            .arg(stage.join("Glyphvane.app")),
        "ditto (dmg staging)",
    )?;
    applications_link(&stage.join("Applications"))?;
    fs::copy(
        ctx.packaging("macos/FIRST-LAUNCH.txt"),
        stage.join("First launch.txt"),
    )?;
    let out = ctx.out.join(format!(
        "Glyphvane-{}-macos-{}.dmg",
        ctx.version,
        arch_name(ctx)
    ));
    // `hdiutil create` fails now and then with "Resource busy" on build
    // machines (a scanner still holds the staged files); try a few times.
    let mut attempt = 1;
    loop {
        let result = run_checked(
            Command::new("hdiutil")
                .args(["create", "-volname", "Glyphvane", "-srcfolder"])
                .arg(&stage)
                .args(["-fs", "HFS+", "-format", "UDZO", "-ov"])
                .arg(&out),
            "hdiutil create",
        );
        match result {
            Ok(()) => break,
            Err(err) if attempt < 4 => {
                eprintln!("warning: attempt {attempt} failed, trying again: {err:#}");
                std::thread::sleep(std::time::Duration::from_secs(10 * attempt));
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}

#[cfg(unix)]
fn applications_link(link: &Path) -> Result<()> {
    std::os::unix::fs::symlink("/Applications", link)?;
    Ok(())
}

#[cfg(not(unix))]
fn applications_link(_link: &Path) -> Result<()> {
    bail!("disk images are built on macOS")
}

fn tool_exists(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}
