// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `glyphvane render`, run as a process without a display.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `glyphvane` with no display server: the headless commands must not need
/// one.
fn glyphvane() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_glyphvane"));
    cmd.env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null());
    cmd
}

/// Runs `glyphvane args…` with `stdin` as standard input.
fn run(args: &[&str], stdin: &[u8]) -> Output {
    let mut child = glyphvane()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("glyphvane starts");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin)
        .expect("write stdin");
    child.wait_with_output().expect("glyphvane runs")
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("UTF-8 output")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[track_caller]
fn assert_status(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        stderr(output)
    );
}

/// Spec examples rendered from files match the spec's HTML.
#[test]
fn renders_spec_examples() {
    let path = workspace_root().join("tests/fixtures/spec/commonmark-0.31.2.json");
    let text = std::fs::read_to_string(&path).expect("spec fixture");
    let examples: Vec<serde_json::Value> = serde_json::from_str(&text).expect("spec JSON");
    let dir = tempfile::tempdir().expect("temp dir");
    let mut checked = 0;
    for example in examples.iter().step_by(37) {
        let number = example["example"].as_u64().expect("number");
        let markdown = example["markdown"].as_str().expect("markdown");
        let html = example["html"].as_str().expect("html");
        let input = dir.path().join(format!("example-{number}.md"));
        std::fs::write(&input, markdown).expect("write example");
        for format in ["commonmark-html", "html"] {
            let output = run(
                &["render", input.to_str().expect("path"), "--format", format],
                b"",
            );
            assert_status(&output, 0);
            assert_eq!(
                gv_syntax::harness::normalize_html(stdout(&output)),
                gv_syntax::harness::normalize_html(html),
                "example {number} ({format})"
            );
        }
        checked += 1;
    }
    assert!(checked > 10);

    // Byte for byte, without normalizing.
    let output = run(&["render", "-"], b"- foo\n-\n- bar\n");
    assert_status(&output, 0);
    assert_eq!(
        stdout(&output),
        "<ul>\n<li>foo</li>\n<li></li>\n<li>bar</li>\n</ul>\n"
    );
    assert!(output.stderr.is_empty(), "{}", stderr(&output));
}

#[test]
fn reads_stdin_and_writes_stdout() {
    let output = run(
        &["render", "-", "--profile", "commonmark"],
        b"# Hi *there*\r\n",
    );
    assert_status(&output, 0);
    assert_eq!(stdout(&output), "<h1>Hi <em>there</em></h1>\n");

    // `-o -` is standard output as well.
    let output = run(&["render", "-", "-o", "-"], b"`code`");
    assert_status(&output, 0);
    assert_eq!(stdout(&output), "<p><code>code</code></p>\n");
}

#[test]
fn output_file_is_replaced_atomically() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = dir.path().join("out.html");
    std::fs::write(&out, "old contents that are longer than the new ones\n").expect("old");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o640)).expect("chmod");
    }
    let out_arg = out.to_str().expect("path");
    let output = run(&["render", "-", "-o", out_arg], b"> quote\n");
    assert_status(&output, 0);
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read_to_string(&out).expect("output"),
        "<blockquote>\n<p>quote</p>\n</blockquote>\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&out).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o640, "permissions are kept");
    }
    // No temporary file is left behind.
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .expect("list")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(names, ["out.html"]);

    // A folder that does not exist: I/O error, nothing written.
    let missing = dir.path().join("missing/out.html");
    let output = run(
        &["render", "-", "-o", missing.to_str().expect("path")],
        b"x",
    );
    assert_status(&output, 1);
    assert!(
        stderr(&output).contains("cannot write"),
        "{}",
        stderr(&output)
    );

    // A folder as the output: I/O error.
    let output = run(
        &["render", "-", "-o", dir.path().to_str().expect("path")],
        b"x",
    );
    assert_status(&output, 1);
}

#[test]
fn every_shipped_profile_renders() {
    let md = b"| a |\n| - |\n| ~~b~~ |\n\n$x$\n";
    let html = |profile: &str| {
        let output = run(&["render", "-", "--profile", profile], md);
        assert_status(&output, 0);
        stdout(&output).to_owned()
    };
    let cm = html("commonmark");
    assert!(cm.contains("<p>| a |"), "{cm}");
    for profile in ["gfm-file", "gfm-comment", "extended"] {
        let out = html(profile);
        assert!(out.contains("<table>"), "{profile}: {out}");
        assert!(out.contains("<del>b</del>"), "{profile}: {out}");
        assert!(
            out.contains("<span data-math-style=\"inline\">x</span>"),
            "{profile}: {out}"
        );
    }
    // Obsidian: tables need two dashes per cell, `$` math is Pandoc's.
    let ob = html("obsidian");
    assert!(
        !ob.contains("<table>") && ob.contains("data-math-style"),
        "{ob}"
    );
    let output = run(
        &["render", "-", "--profile", "gfm-file", "--format", "cst"],
        md,
    );
    assert_status(&output, 0);
    assert!(
        stdout(&output).contains("(text table"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn usage_errors_and_unsupported_profiles_exit_with_2() {
    for profile in ["mkdocs", "pandoc", "gitlab"] {
        let output = run(&["render", "-", "--profile", profile], b"x");
        assert_status(&output, 2);
        assert!(output.stdout.is_empty());
        assert!(
            stderr(&output).contains("not supported yet"),
            "{}",
            stderr(&output)
        );
    }
    let output = run(&["render", "-", "--profile", "markdown-it"], b"x");
    assert_status(&output, 2);
    assert!(stderr(&output).contains("unknown profile"));

    let output = run(&["render", "-", "--format", "pdf"], b"x");
    assert_status(&output, 2);
    let output = run(&["render"], b"");
    assert_status(&output, 2);
}

#[test]
fn unreadable_input_exits_with_1() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("missing.md");
    let output = run(&["render", missing.to_str().expect("path")], b"");
    assert_status(&output, 1);
    assert!(
        stderr(&output).contains("cannot read"),
        "{}",
        stderr(&output)
    );

    // Not UTF-8, with NUL bytes: a PNG header.
    let output = run(&["render", "-"], b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\xff");
    assert_status(&output, 1);
    assert!(stderr(&output).contains("binary"), "{}", stderr(&output));
}

/// UTF-16 and legacy encodings are decoded; a lossy decode warns, or fails
/// with `--strict`.
#[test]
fn decodes_utf16_and_legacy_input() {
    let utf16le: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain("# Grüße\n".encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    let output = run(&["render", "-"], &utf16le);
    assert_status(&output, 0);
    assert_eq!(stdout(&output), "<h1>Grüße</h1>\n");
    assert!(output.stderr.is_empty(), "{}", stderr(&output));

    // windows-1252, detected and decoded exactly: no warning.
    let latin = b"Le caf\xe9 est tr\xe8s chaud, d\xe9j\xe0 servi \xe0 la terrasse.\n";
    let output = run(&["render", "-"], latin);
    assert_status(&output, 0);
    assert_eq!(
        stdout(&output),
        "<p>Le café est très chaud, déjà servi à la terrasse.</p>\n"
    );
    assert!(output.stderr.is_empty(), "{}", stderr(&output));

    // UTF-16 with an odd length: lossy.
    let mut odd = utf16le.clone();
    odd.push(b'x');
    let output = run(&["render", "-"], &odd);
    assert_status(&output, 0);
    assert_eq!(stdout(&output), "<h1>Grüße</h1>\n<p>\u{FFFD}</p>\n");
    let warning = stderr(&output);
    assert!(
        warning.contains("warning") && warning.contains("UTF-16LE"),
        "{warning}"
    );
    let output = run(&["render", "-", "--strict"], &odd);
    assert_status(&output, 3);
    assert!(output.stdout.is_empty());

    // Invalid UTF-8 after a UTF-8 byte-order mark: lossy.
    let output = run(&["render", "-"], b"\xEF\xBB\xBFa \xFF b\n");
    assert_status(&output, 0);
    assert_eq!(stdout(&output), "<p>a \u{FFFD} b</p>\n");
    assert!(stderr(&output).contains("invalid UTF-8 at byte 2"));
    let output = run(
        &["render", "-", "--strict", "--format", "cst"],
        b"\xEF\xBB\xBFa \xFF b\n",
    );
    assert_status(&output, 3);
    assert!(output.stdout.is_empty());
}

#[test]
fn cst_format() {
    let output = run(&["render", "-", "--format", "cst"], b"- foo\n  *bar*\n");
    assert_status(&output, 0);
    assert_eq!(
        stdout(&output),
        r#"(document 0..14
  (bullet-list '-' tight 0..14
    (item 0..14
      (list-marker 0..2 "- ")
      (text paragraph 2..14
        (content 2..5 "foo")
        (line-ending 5..6 "\n")
        (prefix 6..8 "  ")
        (content 8..13 "*bar*")
        (line-ending 13..14 "\n")
        (inlines
          (Text 2..5 "foo")
          (SoftBreak 5..5 "\n")
          (Emphasis 8..13
            (Marker 8..9 "*")
            (Text 9..12 "bar")
            (Marker 12..13 "*")))))))
"#
    );
}
