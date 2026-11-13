// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The vendored spec fixtures in `tests/fixtures/spec/`: loading the
//! examples, converting the GFM `spec.txt` to JSON, checking SHA-256 sums,
//! and `cargo xtask fetch-spec`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Fixture directory, relative to the workspace root.
pub(crate) const DIR: &str = "tests/fixtures/spec";
/// Checksums of every fixture file, in `sha256sum` format.
const SUMS: &str = "SHA256SUMS";

/// A file fetched verbatim from upstream.
struct Source {
    file: &'static str,
    url: &'static str,
}

const SOURCES: &[Source] = &[
    Source {
        file: COMMONMARK_JSON,
        url: "https://spec.commonmark.org/0.31.2/spec.json",
    },
    Source {
        file: GFM_TXT,
        url: "https://raw.githubusercontent.com/github/cmark-gfm/0.29.0.gfm.13/test/spec.txt",
    },
];

pub(crate) const COMMONMARK_JSON: &str = "commonmark-0.31.2.json";
const GFM_TXT: &str = "gfm-0.29.0.gfm.13.txt";
/// Generated from [`GFM_TXT`] by [`gfm_examples_from_spec_txt`].
pub(crate) const GFM_JSON: &str = "gfm-0.29.0.gfm.13.json";

/// One spec example, in the format of `spec.json` (and of
/// `spec_tests.py --dump-tests`, which adds `extensions` for GFM).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Example {
    pub(crate) markdown: String,
    pub(crate) html: String,
    pub(crate) example: u32,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
    pub(crate) section: String,
    /// cmark-gfm extensions the example needs (GFM only).
    #[serde(default)]
    pub(crate) extensions: Vec<String>,
}

pub(crate) fn path(root: &Path, file: &str) -> PathBuf {
    root.join(DIR).join(file)
}

/// Loads the examples of a JSON fixture.
pub(crate) fn load(root: &Path, file: &str) -> Result<Vec<Example>> {
    let p = path(root, file);
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))
}

/// Extracts the examples of a spec in `spec.txt` format, as cmark-gfm's
/// `test/spec_tests.py` (`get_tests`) does: examples are fenced by 32
/// backticks plus ` example [extensions…]`, a `.` line separates Markdown
/// from HTML, `→` stands for a tab, the section is the latest ATX heading
/// outside examples, and `disabled` examples are numbered but skipped.
pub(crate) fn gfm_examples_from_spec_txt(text: &str) -> Vec<Example> {
    const FENCE: &str = "````````````````````````````````";
    let mut tests = Vec::new();
    let (mut state, mut example_number, mut start_line) = (0u8, 0u32, 0u32);
    let mut extensions: Vec<String> = Vec::new();
    let (mut markdown, mut html) = (String::new(), String::new());
    let mut header = String::new();
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let l = line.trim();
        if let Some(rest) = l
            .strip_prefix(FENCE)
            .and_then(|r| r.strip_prefix(" example"))
        {
            state = 1;
            extensions = rest.split_whitespace().map(str::to_owned).collect();
        } else if l == FENCE {
            state = 0;
            example_number += 1;
            if !extensions.iter().any(|e| e == "disabled") {
                tests.push(Example {
                    markdown: markdown.replace('→', "\t"),
                    html: html.replace('→', "\t"),
                    example: example_number,
                    start_line,
                    end_line: line_number,
                    section: header.clone(),
                    extensions: extensions.clone(),
                });
            }
            start_line = 0;
            markdown.clear();
            html.clear();
        } else if l == "." {
            state = 2;
        } else if state == 1 {
            if start_line == 0 {
                start_line = line_number - 1;
            }
            markdown.push_str(line);
        } else if state == 2 {
            html.push_str(line);
        } else if state == 0 && is_atx_header_line(line) {
            header = remove_header_marks(line).trim().to_owned();
        }
    }
    tests
}

/// `re.match('#+ ', line)`.
fn is_atx_header_line(line: &str) -> bool {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    hashes > 0 && line.as_bytes().get(hashes) == Some(&b' ')
}

/// `re.sub('#+ ', '', line)`: removes every run of `#` followed by a space.
fn remove_header_marks(line: &str) -> String {
    let b = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        if b[i] == b'#' {
            let run_end = i + b[i..].iter().take_while(|&&c| c == b'#').count();
            if b.get(run_end) == Some(&b' ') {
                out.push_str(&line[copied..i]);
                i = run_end + 1;
                copied = i;
            } else {
                i = run_end;
            }
        } else {
            i += 1;
        }
    }
    out.push_str(&line[copied..]);
    out
}

/// The GFM JSON fixture as generated from the vendored `spec.txt`.
fn generate_gfm_json(spec_txt: &str) -> Result<String> {
    let examples = gfm_examples_from_spec_txt(spec_txt);
    let mut json = serde_json::to_string_pretty(&examples)?;
    json.push('\n');
    Ok(json)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parses `SHA256SUMS` into (sum, file) pairs.
fn read_sums(root: &Path) -> Result<Vec<(String, String)>> {
    let p = path(root, SUMS);
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((sum, file)) = line.split_once(char::is_whitespace) else {
            bail!("{}: malformed line `{line}`", p.display());
        };
        out.push((
            sum.to_owned(),
            file.trim_start().trim_start_matches('*').to_owned(),
        ));
    }
    Ok(out)
}

fn write_sums(root: &Path) -> Result<()> {
    let mut text = String::new();
    for file in SOURCES.iter().map(|s| s.file).chain([GFM_JSON]) {
        let bytes = std::fs::read(path(root, file))?;
        let _ = writeln!(text, "{}  {file}", sha256_hex(&bytes));
    }
    std::fs::write(path(root, SUMS), text)?;
    Ok(())
}

/// Offline check: every file in `SHA256SUMS` matches its sum, every fixture
/// is listed, and the GFM JSON is exactly what the vendored `spec.txt`
/// converts to. Prints problems; returns `Ok(false)` if there are any.
pub(crate) fn verify_local(root: &Path) -> Result<bool> {
    let mut ok = true;
    let sums = read_sums(root)?;
    for file in SOURCES.iter().map(|s| s.file).chain([GFM_JSON]) {
        if !sums.iter().any(|(_, f)| f == file) {
            eprintln!("fetch-spec: {file} is not listed in {DIR}/{SUMS}");
            ok = false;
        }
    }
    for (sum, file) in &sums {
        let p = path(root, file);
        match std::fs::read(&p) {
            Ok(bytes) => {
                let actual = sha256_hex(&bytes);
                if &actual != sum {
                    eprintln!("fetch-spec: {file}: SHA-256 {actual}, expected {sum}");
                    ok = false;
                }
            }
            Err(e) => {
                eprintln!("fetch-spec: {}: {e}", p.display());
                ok = false;
            }
        }
    }
    let spec_txt = std::fs::read_to_string(path(root, GFM_TXT))?;
    let json = std::fs::read_to_string(path(root, GFM_JSON)).unwrap_or_default();
    if generate_gfm_json(&spec_txt)? != json {
        eprintln!("fetch-spec: {GFM_JSON} differs from the conversion of {GFM_TXT}");
        ok = false;
    }
    Ok(ok)
}

/// Downloads `url` with curl into memory (through a temporary file).
fn download(url: &str, tmp: &Path) -> Result<Vec<u8>> {
    let status = Command::new("curl")
        .args([
            "-fsSL",
            "--retry",
            "2",
            "--connect-timeout",
            "20",
            "--max-time",
            "120",
            "-o",
        ])
        .arg(tmp)
        .arg(url)
        .status()
        .context("running curl (needed to download the spec fixtures)")?;
    if !status.success() {
        bail!("curl failed for {url} ({status})");
    }
    let bytes = std::fs::read(tmp)?;
    let _ = std::fs::remove_file(tmp);
    Ok(bytes)
}

/// Downloaded sources: (file name, contents).
type Downloads = Vec<(&'static str, Vec<u8>)>;

/// Downloads every source; returns (file, bytes) pairs, or `None` with a
/// warning if the network is not reachable.
fn download_all(soft_fail: bool) -> Result<Option<Downloads>> {
    let tmp_dir = std::env::temp_dir().join(format!("glyphvane-fetch-spec-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir)?;
    let mut out = Vec::new();
    let mut result = Ok(());
    for src in SOURCES {
        eprintln!("fetch-spec: downloading {}", src.url);
        match download(src.url, &tmp_dir.join(src.file)) {
            Ok(bytes) => out.push((src.file, bytes)),
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp_dir);
    match result {
        Ok(()) => Ok(Some(out)),
        Err(e) if soft_fail => {
            eprintln!(
                "fetch-spec: warning: download skipped ({e:#}); local files were verified offline"
            );
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

const USAGE: &str = "usage: cargo xtask fetch-spec [--verify [--offline] | --update-sums]";

/// `cargo xtask fetch-spec [--verify [--offline] | --update-sums]`
pub(crate) fn run(root: &Path, args: &[String]) -> Result<bool> {
    let (mut verify, mut offline, mut update) = (false, false, false);
    for a in args {
        match a.as_str() {
            "--verify" => verify = true,
            "--offline" => offline = true,
            "--update-sums" => update = true,
            _ => bail!("unknown option `{a}`\n{USAGE}"),
        }
    }
    if (offline && !verify) || (update && verify) {
        bail!(USAGE);
    }

    if verify {
        let mut ok = verify_local(root)?;
        if ok {
            eprintln!("fetch-spec: local fixtures match {DIR}/{SUMS}");
        }
        if !offline && let Some(files) = download_all(true)? {
            let sums = read_sums(root)?;
            for (file, bytes) in files {
                let actual = sha256_hex(&bytes);
                let expected = sums
                    .iter()
                    .find(|(_, f)| f == file)
                    .map(|(s, _)| s.as_str());
                if expected == Some(actual.as_str()) {
                    eprintln!("fetch-spec: upstream {file} matches");
                } else {
                    eprintln!(
                        "fetch-spec: upstream {file} has SHA-256 {actual}, recorded {expected:?}"
                    );
                    ok = false;
                }
            }
        }
        return Ok(ok);
    }

    let Some(files) = download_all(false)? else {
        return Ok(false);
    };
    let sums = if update { Vec::new() } else { read_sums(root)? };
    for (file, bytes) in &files {
        let actual = sha256_hex(bytes);
        if !update && !sums.iter().any(|(s, f)| f == file && *s == actual) {
            eprintln!(
                "fetch-spec: upstream {file} has SHA-256 {actual}, which is not the recorded sum.\n\
                 If this is an intended upgrade, run `cargo xtask fetch-spec --update-sums`."
            );
            return Ok(false);
        }
    }
    std::fs::create_dir_all(root.join(DIR))?;
    for (file, bytes) in &files {
        std::fs::write(path(root, file), bytes)?;
    }
    let spec_txt = std::fs::read_to_string(path(root, GFM_TXT))?;
    std::fs::write(path(root, GFM_JSON), generate_gfm_json(&spec_txt)?)?;
    if update {
        write_sums(root)?;
        eprintln!("fetch-spec: wrote {DIR}/{SUMS}");
    }
    let ok = verify_local(root)?;
    if ok {
        eprintln!("fetch-spec: fixtures written and verified");
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_marks_are_removed_like_re_sub() {
        assert_eq!(remove_header_marks("## Tabs\n"), "Tabs\n");
        assert_eq!(remove_header_marks("# A ## B #x\n"), "A B #x\n");
        assert!(is_atx_header_line("### X"));
        assert!(!is_atx_header_line("#X"));
    }

    #[test]
    fn spec_txt_examples_are_extracted() {
        let fence = "`".repeat(32);
        let text = format!(
            "# Intro\n\n## Tables\n\n{fence} example table\n| a |\n→b\n.\n<p>x</p>\n{fence}\n\n\
             {fence} example disabled\nz\n.\n<p>z</p>\n{fence}\n\n{fence} example\n.\n\n{fence}\n"
        );
        let ex = gfm_examples_from_spec_txt(&text);
        assert_eq!(ex.len(), 2);
        assert_eq!(ex[0].example, 1);
        assert_eq!(ex[0].section, "Tables");
        assert_eq!(ex[0].markdown, "| a |\n\tb\n");
        assert_eq!(ex[0].html, "<p>x</p>\n");
        assert_eq!(ex[0].extensions, ["table"]);
        assert_eq!((ex[0].start_line, ex[0].end_line), (5, 10));
        assert_eq!(ex[1].example, 3);
        assert_eq!(ex[1].markdown, "");
        assert_eq!(ex[1].html, "\n");
    }

    #[test]
    fn sha256_of_empty_input() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
