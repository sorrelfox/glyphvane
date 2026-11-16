// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `glyphvane export`, run as a process without a display.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// `glyphvane args…` with no display server, `stdin` as standard input.
fn run(args: &[&str], stdin: &[u8]) -> Output {
    run_with(args, stdin, &[])
}

/// [`run`] with environment variables.
fn run_with(args: &[&str], stdin: &[u8], env: &[(&str, &str)]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_glyphvane"))
        .envs(env.iter().copied())
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
    // One format; a paper only for PDF, and a known one.
    assert_status(&run(&["export", "-", "--html", "--pdf"], b"# x\n"), 2);
    assert_status(
        &run(&["export", "-", "--html", "--paper", "a4"], b"# x\n"),
        2,
    );
    let paper = run(&["export", "-", "--pdf", "--paper", "b5"], b"# x\n");
    assert_status(&paper, 2);
    assert!(String::from_utf8_lossy(&paper.stderr).contains("unknown paper `b5`"));
    let output = run(&["export", "-", "--html", "--profile", "mkdocs"], b"# x\n");
    assert_status(&output, 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("not supported yet"));
    let missing = run(&["export", "/nonexistent/x.md", "--html"], b"");
    assert_status(&missing, 1);
}

/// `glyphvane-helper` beside the binary under test, when the workspace
/// built it (`cargo test --workspace`, or `cargo build -p gv-helper`).
fn helper() -> Option<PathBuf> {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_glyphvane"));
    let helper = exe.with_file_name(format!("glyphvane-helper{}", std::env::consts::EXE_SUFFIX));
    if helper.is_file() {
        Some(helper)
    } else {
        eprintln!("glyphvane-helper is not built: the PDF is not checked");
        None
    }
}

#[test]
fn pdf_beside_the_input_and_on_standard_output() {
    let Some(_) = helper() else {
        return;
    };
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("note.md");
    std::fs::write(
        &input,
        "# Note\n\nSee [the site](https://example.com) and $x^2$.\n\n![missing](gone.png)\n",
    )
    .expect("write");
    let output = run(&["export", input.to_str().expect("path"), "--pdf"], b"");
    assert_status(&output, 0);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("image gone.png not embedded"), "{stderr}");
    let pdf = std::fs::read(dir.path().join("note.pdf")).expect("written");
    assert!(pdf.starts_with(b"%PDF-"));
    // The same input gives the same bytes, on standard output too.
    let again = run(
        &["export", input.to_str().expect("path"), "--pdf", "-o", "-"],
        b"",
    );
    assert_status(&again, 0);
    assert_eq!(again.stdout, pdf);
    // Another paper gives another PDF.
    let letter = run(
        &["export", "-", "--pdf", "--paper", "letter", "-o", "-"],
        b"# Note\n",
    );
    let a4 = run(&["export", "-", "--pdf", "-o", "-"], b"# Note\n");
    assert_status(&letter, 0);
    assert_status(&a4, 0);
    assert!(letter.stdout.starts_with(b"%PDF-"));
    assert_ne!(letter.stdout, a4.stdout);
}

#[test]
fn pdf_without_the_helper_fails() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = dir.path().join("out.pdf");
    let output = run_with(
        &["export", "-", "--pdf", "-o", out.to_str().expect("path")],
        b"# x\n",
        &[("GLYPHVANE_HELPER", "/nonexistent/glyphvane-helper")],
    );
    assert_status(&output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("PDF engine"), "{stderr}");
    assert!(!out.exists());
}
