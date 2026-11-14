// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Windows artifacts: a portable `.zip` and a per-user MSI (WiX v6).
//!
//! Signing (SignPath) happens in the release workflow: the executables are
//! signed before they are packaged here, and the MSI after.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::archive::Tree;
use super::{Arch, Ctx, HELPER_BIN, MAIN_BIN, require_tool, run_checked};

/// The pinned WiX Toolset version (a .NET tool) and its UI extension.
const WIX_VERSION: &str = "6.0.2";

/// `glyphvane-<version>-<triple>.zip`: both executables, licence, notices.
pub(super) fn zip(ctx: &Ctx) -> Result<PathBuf> {
    let top = format!("glyphvane-{}-{}", ctx.version, ctx.target);
    let mut tree = Tree::default();
    tree.file(
        &format!("{top}/{}", ctx.exe_name(MAIN_BIN)),
        &ctx.main_exe,
        0o755,
    );
    if let Some(helper) = &ctx.helper_exe {
        tree.file(
            &format!("{top}/{}", ctx.exe_name(HELPER_BIN)),
            helper,
            0o755,
        );
    }
    tree.file(
        &format!("{top}/LICENSE.txt"),
        &ctx.root.join("LICENSE"),
        0o644,
    );
    tree.file(
        &format!("{top}/README.md"),
        &ctx.root.join("README.md"),
        0o644,
    );
    tree.file(
        &format!("{top}/THIRD-PARTY-NOTICES.txt"),
        &ctx.packaging("THIRD-PARTY-NOTICES.txt"),
        0o644,
    );
    let out = ctx.out.join(format!("{top}.zip"));
    tree.write_zip(&out, ctx.epoch)?;
    Ok(out)
}

/// `Glyphvane-<version>-windows-<arch>.msi` from `packaging/windows/glyphvane.wxs`.
pub(super) fn msi(ctx: &Ctx) -> Result<PathBuf> {
    let hint = format!(
        "Install the pinned version (needs the .NET SDK): \
         dotnet tool install --global wix --version {WIX_VERSION} && \
         wix extension add --global WixToolset.UI.wixext/{WIX_VERSION}"
    );
    require_tool("wix", "--version", &hint)?;
    let arch = match ctx.arch {
        Arch::X86_64 => "x64",
        Arch::Aarch64 => "arm64",
    };
    let licence = ctx.root.join("LICENSE");
    let rtf = ctx.scratch("msi")?.join("LICENSE.rtf");
    let text = fs::read_to_string(&licence).context("reading LICENSE")?;
    fs::write(&rtf, to_rtf(&text))?;
    let bin_dir = ctx
        .main_exe
        .parent()
        .context("the executable has no directory")?;

    let out = ctx
        .out
        .join(format!("Glyphvane-{}-windows-{arch}.msi", ctx.version));
    let define = |name: &str, value: &dyn AsRef<std::ffi::OsStr>| {
        let mut d = std::ffi::OsString::from(format!("{name}="));
        d.push(value.as_ref());
        d
    };
    let mut cmd = ctx.command("wix");
    cmd.arg("build")
        .arg(ctx.packaging("windows/glyphvane.wxs"))
        .args(["-arch", arch, "-ext", "WixToolset.UI.wixext"])
        .arg("-d")
        .arg(define("Version", &ctx.numeric_version()))
        .arg("-d")
        .arg(define("BinDir", &bin_dir))
        .arg("-d")
        .arg(define("LicenseFile", &licence))
        .arg("-d")
        .arg(define("LicenseRtf", &rtf))
        .arg("-d")
        .arg(define(
            "NoticesFile",
            &ctx.packaging("THIRD-PARTY-NOTICES.txt"),
        ))
        .arg("-d")
        .arg(define("IconFile", &ctx.packaging("icons/glyphvane.ico")));
    if ctx.helper_exe.is_some() {
        cmd.args(["-d", "IncludeHelper=1"]);
    }
    cmd.arg("-o").arg(&out);
    run_checked(&mut cmd, "wix build")?;
    // `wix build` writes a .wixpdb next to the MSI; it is not an artifact.
    let _ = fs::remove_file(out.with_extension("wixpdb"));
    Ok(out)
}

/// Plain text as a minimal RTF document, for the installer's licence page.
fn to_rtf(text: &str) -> String {
    let mut rtf =
        String::from("{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0\\fmodern Consolas;}}\\f0\\fs16\n");
    for line in text.lines() {
        for c in line.chars() {
            match c {
                '\\' | '{' | '}' => {
                    rtf.push('\\');
                    rtf.push(c);
                }
                c if c.is_ascii() && !c.is_ascii_control() => rtf.push(c),
                '\t' => rtf.push_str("\\tab "),
                c => {
                    let mut units = [0u16; 2];
                    for unit in c.encode_utf16(&mut units) {
                        // RTF \u takes a signed 16-bit value.
                        let _ = write!(rtf, "\\u{}?", i32::from(*unit as i16));
                    }
                }
            }
        }
        rtf.push_str("\\par\n");
    }
    rtf.push('}');
    rtf
}

#[cfg(test)]
mod tests {
    use super::to_rtf;

    #[test]
    fn rtf_escapes() {
        let rtf = to_rtf("a{b}\\c\n\u{e9}\u{1f600}");
        assert!(rtf.starts_with("{\\rtf1"));
        assert!(rtf.contains("a\\{b\\}\\\\c\\par\n"));
        assert!(rtf.contains("\\u233?"));
        assert!(rtf.contains("\\u-10179?\\u-8704?"));
        assert!(rtf.ends_with("\\par\n}"));
    }
}
