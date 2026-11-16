// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `glyphvane export`, run as a process without a display.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

/// `glyphvane args…` with no display server, `stdin` as standard input.
fn run(args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_glyphvane"))
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
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

#[track_caller]
fn assert_status(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn exports_beside_the_input_by_default() {
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("note.md");
    std::fs::write(&input, "# Note\n\nSee [other](other.md) and $x^2$.\n").expect("write");
    let output = run(&["export", input.to_str().expect("path"), "--html"], b"");
    assert_status(&output, 0);
    let html = std::fs::read_to_string(dir.path().join("note.html")).expect("written");
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("<title>Note</title>"));
    assert!(html.contains("href=\"other.html\""));
    assert!(html.contains("<span class=\"gv-math\"><svg"));
    // The same input gives the same file.
    let again = run(
        &["export", input.to_str().expect("path"), "--html", "-o", "-"],
        b"",
    );
    assert_status(&again, 0);
    assert_eq!(String::from_utf8_lossy(&again.stdout), html);
}

#[test]
fn standard_input_to_a_named_file_with_a_profile() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = dir.path().join("out.html");
    let output = run(
        &[
            "export",
            "-",
            "--html",
            "--profile",
            "gfm-file",
            "-o",
            out.to_str().expect("path"),
        ],
        b"<span style=\"color: red\">x</span> <script>alert(1)</script>\n",
    );
    assert_status(&output, 0);
    let html = std::fs::read_to_string(&out).expect("written");
    // GitHub drops `style`; scripts never survive.
    assert!(html.contains("<span>x</span>"), "{html}");
    assert!(!html.contains("<script"));
}

#[test]
fn usage_errors() {
    // A format is required.
    assert_status(&run(&["export", "-"], b""), 2);
    assert_status(&run(&["export", "-", "--pdf"], b"# x\n"), 2);
    let output = run(&["export", "-", "--html", "--profile", "mkdocs"], b"# x\n");
    assert_status(&output, 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("not supported yet"));
    let missing = run(&["export", "/nonexistent/x.md", "--html"], b"");
    assert_status(&missing, 1);
}
