// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `cargo xtask check-headers`: every Rust source file starts with the
//! SPDX license identifier and the copyright line.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

const SPDX_LINE: &str = "// SPDX-License-Identifier: GPL-3.0-or-later";
const SPDX_PREFIX: &str = "// SPDX-License-Identifier:";
const SPDX_MARKER: &str = "SPDX-License-Identifier";
const COPYRIGHT_PREFIX: &str = "// Copyright (C) ";
const COPYRIGHT_SUFFIX: &str = " Glyphvane contributors";
/// The header `--fix` inserts.
const HEADER: &str = "// SPDX-License-Identifier: GPL-3.0-or-later\n\
                      // Copyright (C) 2026 Glyphvane contributors\n";

/// Top-level directories that are never checked.
const SKIP_DIRS: &[&str] = &["target", "third_party", ".git"];

/// Result of checking a single file's contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderStatus {
    /// Both header lines are present and well-formed.
    Ok,
    /// No SPDX line at the top; the header can be inserted automatically.
    Missing,
    /// Some header is present but malformed; needs a manual fix.
    Malformed,
}

/// Checks whether `contents` starts with the two required header lines.
pub(crate) fn check(contents: &str) -> HeaderStatus {
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    let mut lines = contents.lines();
    let first = lines.next().unwrap_or("");
    let second = lines.next().unwrap_or("");
    if first == SPDX_LINE && is_copyright_line(second) {
        HeaderStatus::Ok
    } else if first.starts_with(SPDX_PREFIX)
        || first.starts_with(COPYRIGHT_PREFIX)
        || contents.lines().take(10).any(|l| l.contains(SPDX_MARKER))
    {
        // Some header exists; inserting another would duplicate it.
        HeaderStatus::Malformed
    } else {
        HeaderStatus::Missing
    }
}

/// `// Copyright (C) <year>[-<year>] Glyphvane contributors`
fn is_copyright_line(line: &str) -> bool {
    line.strip_prefix(COPYRIGHT_PREFIX)
        .and_then(|rest| rest.strip_suffix(COPYRIGHT_SUFFIX))
        .is_some_and(is_year_or_range)
}

fn is_year_or_range(s: &str) -> bool {
    fn is_year(y: &str) -> bool {
        y.len() == 4 && y.bytes().all(|b| b.is_ascii_digit())
    }
    match s.split_once('-') {
        Some((from, to)) => is_year(from) && is_year(to) && from < to,
        None => is_year(s),
    }
}

/// Returns `contents` with the header inserted at the top.
pub(crate) fn with_header(contents: &str) -> String {
    let body = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    let mut out = String::with_capacity(HEADER.len() + body.len() + 1);
    out.push_str(HEADER);
    if !body.is_empty() && !body.starts_with('\n') && !body.starts_with("\r\n") {
        out.push('\n');
    }
    out.push_str(body);
    out
}

/// Whether a repository-relative path should be checked.
fn is_checked_path(rel: &Path) -> bool {
    rel.extension().is_some_and(|e| e == "rs")
        && !rel
            .components()
            .next()
            .is_some_and(|c| SKIP_DIRS.iter().any(|s| c.as_os_str() == *s))
}

/// Tracked and untracked-but-not-ignored `.rs` files, relative to `root`.
fn rust_files(root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            "*.rs",
        ])
        .current_dir(root)
        .output();
    let mut files: Vec<PathBuf> = match output {
        Ok(out) if out.status.success() => String::from_utf8(out.stdout)
            .context("git ls-files produced non-UTF-8 output")?
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect(),
        // No git (e.g. a source tarball): walk the tree instead.
        _ => {
            let mut files = Vec::new();
            walk(root, root, &mut files)?;
            files
        }
    };
    files.retain(|p| is_checked_path(p) && root.join(p).is_file());
    files.sort();
    files.dedup();
    Ok(files)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if path.is_dir() {
            let skipped =
                rel.components().count() == 1 && SKIP_DIRS.iter().any(|s| rel.as_os_str() == *s);
            if !skipped {
                walk(root, &path, out)?;
            }
        } else {
            out.push(rel);
        }
    }
    Ok(())
}

pub(crate) fn run(root: &Path, fix: bool) -> Result<bool> {
    let files = rust_files(root)?;
    let mut missing = Vec::new();
    let mut malformed = Vec::new();
    let mut fixed = Vec::new();
    for rel in &files {
        let path = root.join(rel);
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        match check(&contents) {
            HeaderStatus::Ok => {}
            HeaderStatus::Missing if fix => {
                std::fs::write(&path, with_header(&contents))
                    .with_context(|| format!("writing {}", path.display()))?;
                fixed.push(rel);
            }
            HeaderStatus::Missing => missing.push(rel),
            HeaderStatus::Malformed => malformed.push(rel),
        }
    }
    for rel in &fixed {
        println!("added header: {}", rel.display());
    }
    if missing.is_empty() && malformed.is_empty() {
        println!("check-headers: {} files OK", files.len());
        return Ok(true);
    }
    eprintln!(
        "check-headers: files must start with:\n  {SPDX_LINE}\n  {}",
        HEADER.lines().nth(1).unwrap_or("")
    );
    for rel in &missing {
        eprintln!("  missing header:   {}", rel.display());
    }
    for rel in &malformed {
        eprintln!("  malformed header: {}  (fix manually)", rel.display());
    }
    if !missing.is_empty() {
        eprintln!("run `cargo xtask check-headers --fix` to insert missing headers");
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exact_header() {
        let src = "// SPDX-License-Identifier: GPL-3.0-or-later\n\
                   // Copyright (C) 2026 Glyphvane contributors\n\nfn main() {}\n";
        assert_eq!(check(src), HeaderStatus::Ok);
    }

    #[test]
    fn accepts_year_range_and_crlf() {
        let src = "// SPDX-License-Identifier: GPL-3.0-or-later\r\n\
                   // Copyright (C) 2026-2027 Glyphvane contributors\r\n";
        assert_eq!(check(src), HeaderStatus::Ok);
    }

    #[test]
    fn missing_header() {
        assert_eq!(check(""), HeaderStatus::Missing);
        assert_eq!(check("//! Docs\nfn main() {}\n"), HeaderStatus::Missing);
    }

    #[test]
    fn malformed_headers() {
        for src in [
            "// SPDX-License-Identifier: MIT\n// Copyright (C) 2026 Glyphvane contributors\n",
            "// SPDX-License-Identifier: GPL-3.0-or-later\n",
            "// SPDX-License-Identifier: GPL-3.0-or-later\n// Copyright (C) 26 Glyphvane contributors\n",
            "// SPDX-License-Identifier: GPL-3.0-or-later\n// Copyright (C) 2027-2026 Glyphvane contributors\n",
            "// SPDX-License-Identifier: GPL-3.0-or-later\n// Copyright (C) 2026- Glyphvane contributors\n",
            "// SPDX-License-Identifier: GPL-3.0-or-later\n// Copyright (C) 2026 Someone Else\n",
            "// SPDX-License-Identifier: GPL-3.0-or-later \n// Copyright (C) 2026 Glyphvane contributors\n",
            "// Copyright (C) 2026 Glyphvane contributors\n// SPDX-License-Identifier: GPL-3.0-or-later\n",
            "\n// SPDX-License-Identifier: GPL-3.0-or-later\n// Copyright (C) 2026 Glyphvane contributors\n",
            "#![allow(dead_code)]\n// SPDX-License-Identifier: GPL-3.0-or-later\n",
        ] {
            assert_eq!(check(src), HeaderStatus::Malformed, "{src:?}");
        }
    }

    #[test]
    fn fix_inserts_header_with_blank_line() {
        let fixed = with_header("//! Docs\n");
        assert_eq!(check(&fixed), HeaderStatus::Ok);
        assert!(fixed.ends_with("contributors\n\n//! Docs\n"));
        assert_eq!(with_header(""), HEADER);
        assert!(with_header("\nfn a() {}\n").ends_with("contributors\n\nfn a() {}\n"));
    }

    #[test]
    fn skips_target_and_third_party() {
        assert!(is_checked_path(Path::new("crates/gv-text/src/lib.rs")));
        assert!(is_checked_path(Path::new("xtask/src/main.rs")));
        assert!(is_checked_path(Path::new("tools/foo/build.rs")));
        assert!(!is_checked_path(Path::new("target/debug/build/out.rs")));
        assert!(!is_checked_path(Path::new("third_party/zed/src/lib.rs")));
        assert!(!is_checked_path(Path::new("crates/gv-text/Cargo.toml")));
    }
}
