// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Linux artifacts: `.deb`, `.rpm`, `.tar.gz` and AppImage.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::archive::Tree;
use super::{APP_ID, Arch, Ctx, HELPER_BIN, MAIN_BIN, require_tool, run_checked, sha256_file};
use crate::cargo;

/// Pinned tool versions (ADR-020). Keep in step with the workflows.
const CARGO_DEB_VERSION: &str = "3.8.0";
const CARGO_GENERATE_RPM_VERSION: &str = "0.21.1";

/// The AppImage type2-runtime release, and the SHA-256 of each runtime file
/// (https://github.com/AppImage/type2-runtime/releases/tag/20251108). The
/// runtime is only concatenated with the image here, never executed.
const APPIMAGE_RUNTIME_RELEASE: &str = "20251108";
const APPIMAGE_RUNTIME_SHA256_X86_64: &str =
    "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d";
const APPIMAGE_RUNTIME_SHA256_AARCH64: &str =
    "00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444";

/// The desktop integration files, below `prefix` (`share/` or `usr/share/`).
fn add_desktop_files(ctx: &Ctx, tree: &mut Tree, prefix: &str, metainfo_name: &str) {
    tree.file(
        &format!("{prefix}/applications/{APP_ID}.desktop"),
        &ctx.packaging(&format!("linux/{APP_ID}.desktop")),
        0o644,
    );
    tree.file(
        &format!("{prefix}/metainfo/{metainfo_name}"),
        &ctx.packaging(&format!("linux/{APP_ID}.metainfo.xml")),
        0o644,
    );
    tree.file(
        &format!("{prefix}/icons/hicolor/scalable/apps/{APP_ID}.svg"),
        &ctx.packaging("icons/glyphvane.svg"),
        0o644,
    );
    for size in [128, 256, 512] {
        tree.file(
            &format!("{prefix}/icons/hicolor/{size}x{size}/apps/{APP_ID}.png"),
            &ctx.packaging(&format!("icons/{size}x{size}.png")),
            0o644,
        );
    }
}

/// The bundled colour emoji font and its licence, below `prefix` (`share/`
/// or `usr/share/`). The app adds it when the installed emoji font cannot be
/// drawn (R-22) and looks for it in `share/glyphvane/fonts/` next to the
/// executable or in `../share/glyphvane/fonts/`. The deb and rpm install the
/// same files through `crates/gv-app/Cargo.toml`.
fn add_emoji_font(ctx: &Ctx, tree: &mut Tree, prefix: &str) {
    tree.file(
        &format!("{prefix}/glyphvane/fonts/NotoColorEmoji.ttf"),
        &ctx.root.join("third_party/noto-emoji/NotoColorEmoji.ttf"),
        0o644,
    );
    tree.file(
        &format!("{prefix}/glyphvane/fonts/NotoColorEmoji-OFL.txt"),
        &ctx.root.join("third_party/noto-emoji/LICENSE"),
        0o644,
    );
}

/// `glyphvane-<version>-<triple>.tar.gz`: both executables at the top, the
/// licence, notices and desktop files in `share/`.
pub(super) fn tarball(ctx: &Ctx) -> Result<PathBuf> {
    let top = format!("glyphvane-{}-{}", ctx.version, ctx.target);
    let mut tree = Tree::default();
    tree.file(&format!("{top}/{MAIN_BIN}"), &ctx.main_exe, 0o755);
    if let Some(helper) = &ctx.helper_exe {
        tree.file(&format!("{top}/{HELPER_BIN}"), helper, 0o755);
    }
    tree.file(&format!("{top}/LICENSE"), &ctx.root.join("LICENSE"), 0o644);
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
    add_desktop_files(
        ctx,
        &mut tree,
        &format!("{top}/share"),
        &format!("{APP_ID}.metainfo.xml"),
    );
    add_emoji_font(ctx, &mut tree, &format!("{top}/share"));
    let out = ctx.out.join(format!("{top}.tar.gz"));
    tree.write_tar_gz(&out, ctx.epoch)?;
    Ok(out)
}

fn warn_if_not_pinned(program: &str, output: &[u8], pinned: &str) {
    let text = String::from_utf8_lossy(output);
    if !text.contains(pinned) {
        eprintln!(
            "warning: {program} reports `{}`; release artifacts use {pinned}",
            text.trim()
        );
    }
}

fn tool_version(ctx: &Ctx, sub: &str) -> Result<Vec<u8>> {
    let output = ctx
        .command(&cargo())
        .args([sub, "--version"])
        .output()
        .with_context(|| format!("running `cargo {sub} --version`"))?;
    Ok([output.stdout, output.stderr].concat())
}

/// `glyphvane_<version>-1_<arch>.deb` through cargo-deb.
pub(super) fn deb(ctx: &Ctx) -> Result<PathBuf> {
    let hint =
        format!("Install the pinned version: cargo install --locked cargo-deb@{CARGO_DEB_VERSION}");
    require_tool("cargo-deb", "--version", &hint)?;
    warn_if_not_pinned("cargo-deb", &tool_version(ctx, "deb")?, CARGO_DEB_VERSION);
    let arch = match ctx.arch {
        Arch::X86_64 => "amd64",
        Arch::Aarch64 => "arm64",
    };
    // Debian sorts `~` before anything, so `1.0.0~beta.1` < `1.0.0`.
    let version = ctx.version.replace('-', "~");
    let out = ctx.out.join(format!("glyphvane_{version}-1_{arch}.deb"));
    let mut cmd = ctx.command(&cargo());
    cmd.args([
        "deb",
        "-p",
        "gv-app",
        "--no-build",
        "--no-strip",
        "--profile",
    ])
    .arg(&ctx.profile)
    .arg("--deb-version")
    .arg(format!("{version}-1"))
    .arg("-o")
    .arg(&out);
    if ctx.cross_layout {
        cmd.args(["--target", &ctx.target]);
    }
    if ctx.helper_exe.is_some() {
        cmd.args(["--variant", "with-helper"]);
    }
    if !dpkg_shlibdeps_usable() {
        // Off Debian (no dpkg database), dpkg-shlibdeps fails and cargo-deb
        // drops `$auto` with only a warning. Stand in for it with a fixed
        // answer computed from the binaries' ELF dependencies.
        let mut depends = Vec::new();
        for exe in std::iter::once(&ctx.main_exe).chain(ctx.helper_exe.as_ref()) {
            for dep in fallback_depends(exe)? {
                if !depends.contains(&dep) {
                    depends.push(dep);
                }
            }
        }
        let line = depends.join(", ");
        eprintln!("note: dpkg-shlibdeps is unusable here; Depends from the ELF headers: {line}");
        let shim = ctx.scratch("shlibdeps-shim")?;
        let script = shim.join("dpkg-shlibdeps");
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' 'shlibs:Depends={line}'\n"),
        )?;
        set_executable(&script)?;
        let path = std::env::var_os("PATH").unwrap_or_default();
        let joined =
            std::env::join_paths(std::iter::once(shim).chain(std::env::split_paths(&path)))?;
        cmd.env("PATH", joined);
    }
    run_checked(&mut cmd, "cargo deb")?;
    Ok(out)
}

/// Whether dpkg-shlibdeps can resolve libraries to packages: it needs a
/// dpkg database that knows libc6 (Debian and Ubuntu, not Fedora).
fn dpkg_shlibdeps_usable() -> bool {
    let ok = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    ok("dpkg-shlibdeps", &["--version"]) && ok("dpkg-query", &["-W", "libc6"])
}

/// The newest glibc version below which the Depends floor is never set: the
/// release build host (ubuntu-22.04).
const GLIBC_FLOOR: &str = "2.35";

/// Conservative Debian/Ubuntu dependencies for one executable, from its
/// `NEEDED` entries and glibc symbol versions (`readelf`).
fn fallback_depends(exe: &Path) -> Result<Vec<String>> {
    require_tool(
        "readelf",
        "--version",
        "Install binutils (apt-get install binutils, dnf install binutils).",
    )?;
    let output = Command::new("readelf")
        .args(["--wide", "--dynamic", "--version-info"])
        .arg(exe)
        .output()
        .context("running readelf")?;
    if !output.status.success() {
        bail!("readelf failed on {}", exe.display());
    }
    depends_from_readelf(&String::from_utf8_lossy(&output.stdout))
}

/// Maps `readelf -d -V` output to Debian package relations. An unknown
/// library is an error, so no dependency is ever dropped silently.
fn depends_from_readelf(text: &str) -> Result<Vec<String>> {
    let glibc = text
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .filter_map(|w| w.strip_prefix("GLIBC_"))
        .filter(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.') && !v.is_empty())
        .chain(std::iter::once(GLIBC_FLOOR))
        .max_by(|a, b| version_key(a).cmp(&version_key(b)))
        .unwrap_or(GLIBC_FLOOR)
        .to_owned();
    let mut deps = Vec::new();
    for line in text.lines().filter(|l| l.contains("(NEEDED)")) {
        let Some(lib) = line.split('[').nth(1).and_then(|r| r.split(']').next()) else {
            continue;
        };
        let dep = match lib {
            "libc.so.6"
            | "libm.so.6"
            | "libdl.so.2"
            | "libpthread.so.0"
            | "librt.so.1"
            | "ld-linux-x86-64.so.2"
            | "ld-linux-aarch64.so.1" => format!("libc6 (>= {glibc})"),
            "libgcc_s.so.1" => "libgcc-s1 (>= 3.0)".to_owned(),
            "libxcb.so.1" => "libxcb1 (>= 1.14)".to_owned(),
            "libxkbcommon.so.0" => "libxkbcommon0 (>= 1.0.0)".to_owned(),
            "libxkbcommon-x11.so.0" => "libxkbcommon-x11-0 (>= 1.0.0)".to_owned(),
            "libwayland-client.so.0" => "libwayland-client0 (>= 1.20.0)".to_owned(),
            "libwayland-egl.so.1" => "libwayland-egl1 (>= 1.20.0)".to_owned(),
            "libvulkan.so.1" => "libvulkan1 (>= 1.3.204)".to_owned(),
            "libEGL.so.1" => "libegl1".to_owned(),
            "libfontconfig.so.1" => "libfontconfig1 (>= 2.13.1)".to_owned(),
            "libfreetype.so.6" => "libfreetype6 (>= 2.11.1)".to_owned(),
            "libz.so.1" => "zlib1g (>= 1:1.2.11)".to_owned(),
            other => match xcb_extension(other) {
                Some(pkg) => format!("{pkg} (>= 1.14)"),
                None => {
                    bail!("no Debian package known for `{other}`; add it to depends_from_readelf")
                }
            },
        };
        if !deps.contains(&dep) {
            deps.push(dep);
        }
    }
    Ok(deps)
}

/// `libxcb-<ext>.so.<n>` is packaged as `libxcb-<ext><n>`.
fn xcb_extension(lib: &str) -> Option<String> {
    let rest = lib.strip_prefix("libxcb-")?;
    let (ext, soversion) = rest.split_once(".so.")?;
    (!ext.is_empty() && soversion.chars().all(|c| c.is_ascii_digit()))
        .then(|| format!("libxcb-{ext}{soversion}"))
}

fn version_key(v: &str) -> Vec<u32> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// `glyphvane-<version>-1.<arch>.rpm` through cargo-generate-rpm.
pub(super) fn rpm(ctx: &Ctx) -> Result<PathBuf> {
    let hint = format!(
        "Install the pinned version: cargo install --locked cargo-generate-rpm@{CARGO_GENERATE_RPM_VERSION}"
    );
    require_tool("cargo-generate-rpm", "--version", &hint)?;
    warn_if_not_pinned(
        "cargo-generate-rpm",
        &tool_version(ctx, "generate-rpm")?,
        CARGO_GENERATE_RPM_VERSION,
    );
    let arch = match ctx.arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
    };
    // RPM versions cannot contain `-`; `~` sorts before the release.
    let version = ctx.version.replace('-', "~");
    let out = ctx.out.join(format!("glyphvane-{version}-1.{arch}.rpm"));
    let mut cmd = ctx.command(&cargo());
    cmd.args(["generate-rpm", "-p", "crates/gv-app", "--profile"])
        .arg(&ctx.profile)
        .args(["--arch", arch, "--target-dir"])
        .arg(&ctx.target_dir)
        .arg("--set-metadata")
        .arg(format!("version = \"{version}\""))
        .arg("-o")
        .arg(&out);
    if ctx.cross_layout {
        cmd.args(["--target", &ctx.target]);
    }
    if ctx.helper_exe.is_some() {
        cmd.args(["--variant", "with-helper"]);
    }
    run_checked(&mut cmd, "cargo generate-rpm")?;
    Ok(out)
}

/// `Glyphvane-<version>-<arch>.AppImage`: an AppDir in a squashfs image,
/// behind the pinned type2-runtime. Without a runtime file, only the image
/// (`….AppImage.squashfs`) is written.
pub(super) fn appimage(ctx: &Ctx) -> Result<PathBuf> {
    require_tool(
        "mksquashfs",
        "-version",
        "Install squashfs-tools (apt-get install squashfs-tools, dnf install squashfs-tools).",
    )?;
    let (arch, pinned) = match ctx.arch {
        Arch::X86_64 => ("x86_64", APPIMAGE_RUNTIME_SHA256_X86_64),
        Arch::Aarch64 => ("aarch64", APPIMAGE_RUNTIME_SHA256_AARCH64),
    };

    let mut tree = Tree::default();
    tree.file(&format!("usr/bin/{MAIN_BIN}"), &ctx.main_exe, 0o755);
    if let Some(helper) = &ctx.helper_exe {
        tree.file(&format!("usr/bin/{HELPER_BIN}"), helper, 0o755);
    }
    add_desktop_files(
        ctx,
        &mut tree,
        "usr/share",
        &format!("{APP_ID}.appdata.xml"),
    );
    add_emoji_font(ctx, &mut tree, "usr/share");
    tree.file(
        "usr/share/licenses/glyphvane/LICENSE",
        &ctx.root.join("LICENSE"),
        0o644,
    );
    tree.file(
        "usr/share/licenses/glyphvane/THIRD-PARTY-NOTICES.txt",
        &ctx.packaging("THIRD-PARTY-NOTICES.txt"),
        0o644,
    );
    // The AppDir root: desktop file, icon, .DirIcon and the entry point.
    tree.symlink(
        &format!("{APP_ID}.desktop"),
        &format!("usr/share/applications/{APP_ID}.desktop"),
    );
    tree.symlink(
        &format!("{APP_ID}.png"),
        &format!("usr/share/icons/hicolor/256x256/apps/{APP_ID}.png"),
    );
    tree.symlink(".DirIcon", &format!("{APP_ID}.png"));
    tree.symlink("AppRun", &format!("usr/bin/{MAIN_BIN}"));

    let appdir = ctx.scratch("AppDir")?;
    tree.materialize(&appdir, ctx.epoch)?;
    let image = ctx.work.join("AppDir.squashfs");
    if image.exists() {
        fs::remove_file(&image)?;
    }
    let epoch = ctx.epoch.to_string();
    let mut cmd = ctx.command("mksquashfs");
    // Every timestamp and owner is fixed on the command line; the variable
    // would conflict with the options in some versions.
    cmd.env_remove("SOURCE_DATE_EPOCH")
        .arg(&appdir)
        .arg(&image)
        .args(["-noappend", "-all-root", "-no-xattrs", "-no-progress"])
        .args(["-comp", "zstd", "-Xcompression-level", "19", "-b", "1M"])
        .args(["-mkfs-time", &epoch, "-all-time", &epoch]);
    run_checked(&mut cmd, "mksquashfs")?;

    let name = format!("Glyphvane-{}-{arch}.AppImage", ctx.version);
    let Some(runtime) = &ctx.appimage_runtime else {
        let out = ctx.out.join(format!("{name}.squashfs"));
        fs::copy(&image, &out)?;
        eprintln!(
            "note: no AppImage runtime given; wrote only the squashfs image. Download \
             https://github.com/AppImage/type2-runtime/releases/download/{APPIMAGE_RUNTIME_RELEASE}/runtime-{arch} \
             (sha256 {pinned}) and pass it with --appimage-runtime."
        );
        return Ok(out);
    };

    let digest = sha256_file(runtime)?;
    if digest != pinned {
        bail!(
            "{} is not the pinned type2-runtime {APPIMAGE_RUNTIME_RELEASE} for {arch}: \
             sha256 {digest}, expected {pinned}",
            runtime.display()
        );
    }
    let mut bytes = fs::read(runtime)?;
    // Update information, as `appimagetool -u` writes it.
    let update =
        format!("gh-releases-zsync|sorrelfox|glyphvane|latest|Glyphvane-*-{arch}.AppImage.zsync");
    write_elf_section(&mut bytes, ".upd_info", update.as_bytes())?;
    bytes.extend_from_slice(&fs::read(&image)?);
    let out = ctx.out.join(name);
    fs::write(&out, bytes)?;
    set_executable(&out)?;
    Ok(out)
}

/// Writes `value` (NUL-padded) into the named section of a 64-bit
/// little-endian ELF file.
fn write_elf_section(elf: &mut [u8], name: &str, value: &[u8]) -> Result<()> {
    let (offset, size) = elf_section(elf, name)
        .with_context(|| format!("no `{name}` section in the AppImage runtime"))?;
    if value.len() >= size {
        bail!("`{name}` holds {size} bytes; {} needed", value.len() + 1);
    }
    let section = &mut elf[offset..offset + size];
    section.fill(0);
    section[..value.len()].copy_from_slice(value);
    Ok(())
}

/// The file offset and size of a section in a 64-bit little-endian ELF file.
fn elf_section(elf: &[u8], name: &str) -> Option<(usize, usize)> {
    if elf.get(..6)? != b"\x7fELF\x02\x01" {
        return None;
    }
    let u16_at = |o: usize| Some(u16::from_le_bytes(elf.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_le_bytes(elf.get(o..o + 4)?.try_into().ok()?));
    let u64_at = |o: usize| Some(u64::from_le_bytes(elf.get(o..o + 8)?.try_into().ok()?));
    let shoff = usize::try_from(u64_at(0x28)?).ok()?;
    let shentsize = usize::from(u16_at(0x3a)?);
    let shnum = usize::from(u16_at(0x3c)?);
    let shstrndx = usize::from(u16_at(0x3e)?);
    let header = |i: usize| shoff + i * shentsize;
    let strtab = usize::try_from(u64_at(header(shstrndx) + 0x18)?).ok()?;
    (0..shnum).find_map(|i| {
        let h = header(i);
        let name_off = strtab + usize::try_from(u32_at(h)?).ok()?;
        let rest = elf.get(name_off..)?;
        let end = rest.iter().position(|&b| b == 0)?;
        if &rest[..end] != name.as_bytes() {
            return None;
        }
        let offset = usize::try_from(u64_at(h + 0x18)?).ok()?;
        let size = usize::try_from(u64_at(h + 0x20)?).ok()?;
        (offset + size <= elf.len()).then_some((offset, size))
    })
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ELF64 file: header, `.shstrtab`, `.upd_info`, and three
    /// section headers (null, `.shstrtab`, `.upd_info`).
    fn tiny_elf() -> Vec<u8> {
        let strtab = b"\0.shstrtab\0.upd_info\0";
        let (strtab_off, upd_off, upd_size) = (0x40usize, 0x60usize, 32usize);
        let shoff = upd_off + upd_size;
        let mut elf = vec![0u8; shoff + 3 * 64];
        elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
        elf[0x28..0x30].copy_from_slice(&(shoff as u64).to_le_bytes());
        elf[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
        elf[0x3c..0x3e].copy_from_slice(&3u16.to_le_bytes());
        elf[0x3e..0x40].copy_from_slice(&1u16.to_le_bytes());
        elf[strtab_off..strtab_off + strtab.len()].copy_from_slice(strtab);
        let mut section = |i: usize, name: u32, off: usize, size: usize| {
            let h = shoff + i * 64;
            elf[h..h + 4].copy_from_slice(&name.to_le_bytes());
            elf[h + 0x18..h + 0x20].copy_from_slice(&(off as u64).to_le_bytes());
            elf[h + 0x20..h + 0x28].copy_from_slice(&(size as u64).to_le_bytes());
        };
        section(1, 1, strtab_off, strtab.len());
        section(2, 11, upd_off, upd_size);
        elf
    }

    #[test]
    fn finds_and_writes_elf_sections() {
        let mut elf = tiny_elf();
        assert_eq!(elf_section(&elf, ".upd_info"), Some((0x60, 32)));
        assert_eq!(elf_section(&elf, ".missing"), None);
        write_elf_section(&mut elf, ".upd_info", b"gh-releases-zsync|a|b").expect("fits");
        assert_eq!(&elf[0x60..0x75], b"gh-releases-zsync|a|b");
        assert_eq!(elf[0x75], 0);
        assert!(write_elf_section(&mut elf, ".upd_info", &[b'x'; 32]).is_err());
        assert_eq!(elf_section(b"not an elf", ".upd_info"), None);
    }

    #[test]
    fn maps_needed_libraries_to_debian_packages() {
        let readelf = "\
 0x0000000000000001 (NEEDED)             Shared library: [libxcb.so.1]
 0x0000000000000001 (NEEDED)             Shared library: [libxkbcommon.so.0]
 0x0000000000000001 (NEEDED)             Shared library: [libxkbcommon-x11.so.0]
 0x0000000000000001 (NEEDED)             Shared library: [libgcc_s.so.1]
 0x0000000000000001 (NEEDED)             Shared library: [libm.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [libxcb-xkb.so.1]
  0x0010:   Name: GLIBC_2.34  Flags: none  Version: 4
  0x0020:   Name: GLIBC_2.39  Flags: none  Version: 5
  0x0030:   Name: GLIBC_2.4  Flags: none  Version: 6
";
        assert_eq!(
            depends_from_readelf(readelf).expect("known libraries"),
            [
                "libxcb1 (>= 1.14)",
                "libxkbcommon0 (>= 1.0.0)",
                "libxkbcommon-x11-0 (>= 1.0.0)",
                "libgcc-s1 (>= 3.0)",
                "libc6 (>= 2.39)",
                "libxcb-xkb1 (>= 1.14)",
            ]
        );
        let old_glibc = " 0x1 (NEEDED) Shared library: [libc.so.6]\n  Name: GLIBC_2.17\n";
        assert_eq!(
            depends_from_readelf(old_glibc).expect("known"),
            ["libc6 (>= 2.35)"]
        );
        assert!(depends_from_readelf(" 0x1 (NEEDED) Shared library: [libfoo.so.3]\n").is_err());
    }
}
