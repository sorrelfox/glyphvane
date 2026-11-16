// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `glyphvane check-links`, run as a process without a display.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// `glyphvane` with no display server: the headless commands must not need
/// one.
fn glyphvane() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_glyphvane"));
    cmd.env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null());
    cmd
}

fn run(args: &[&str]) -> Output {
    glyphvane().args(args).output().expect("glyphvane runs")
}

#[track_caller]
fn assert_status(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("UTF-8 output")
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
    std::fs::write(full, text).expect("write");
}

/// The vault of gv-vault's link checker golden files.
fn vault(root: &Path) {
    std::fs::create_dir_all(root.join(".obsidian")).expect("marker");
    write(
        root,
        "Index.md",
        "# Index\n[[Topic 1]] [[Topic 2#Overview]] [[Topic 3]] [[Leaf]]\n",
    );
    write(
        root,
        "Topic 1.md",
        "# Topic 1\n## Overview\n[[Index]] [[Topic 2#Missing part]]\n",
    );
    write(
        root,
        "Topic 2.md",
        "# Topic 2\n## Overview\n[[index]] [x](Topic%201.md#overview) [o](../outside.md)\n",
    );
    write(root, "sub/Topic 1.md", "copy of a name\n");
    write(root, "deep/er/Leaf.md", "another leaf\n");
    write(root, "sub/Leaf.md", "[[Topic 1]] [\"q\"](../nowhere.md)\n");
}

fn golden(name: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gv-vault/tests/data/check-links")
        .join(name);
    std::fs::read_to_string(path).expect("golden file")
}

#[test]
fn findings_and_exit_codes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    vault(root);
    let root_arg = root.to_str().expect("UTF-8 path");

    let out = run(&["check-links", root_arg, "--format", "json"]);
    assert_status(&out, 1);
    assert_eq!(stdout(&out), golden("vault.json"));

    let out = run(&["check-links", root_arg]);
    assert_status(&out, 1);
    assert_eq!(stdout(&out), golden("vault.txt"));

    let out = run(&["check-links", root_arg, "--profile", "gfm-file"]);
    assert_status(&out, 1);
    assert_eq!(stdout(&out), golden("vault-gfm-file.txt"));

    // A note with warnings only: 0, or 2 with --strict.
    let note = root.join("Topic 2.md");
    let note = note.to_str().expect("UTF-8 path");
    let out = run(&["check-links", note, "--format", "json"]);
    assert_status(&out, 0);
    assert_eq!(stdout(&out), golden("topic-2.json"));
    assert_status(&run(&["check-links", note, "--strict"]), 2);

    // A clean folder.
    let clean = tempfile::tempdir().expect("temp dir");
    write(clean.path(), "a.md", "[b](b.md)\n");
    write(clean.path(), "b.md", "b\n");
    let out = run(&[
        "check-links",
        clean.path().to_str().expect("UTF-8"),
        "--strict",
    ]);
    assert_status(&out, 0);
}

#[test]
fn usage_and_io_errors_exit_with_3() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("nope");
    let out = run(&["check-links", missing.to_str().expect("UTF-8")]);
    assert_status(&out, 3);
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("glyphvane: cannot check"));
    assert_status(&run(&["check-links"]), 3);
    assert_status(&run(&["check-links", ".", "--format", "xml"]), 3);
    let out = run(&["check-links", ".", "--profile", "mkdocs"]);
    assert_status(&out, 3);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not supported yet"));
    // Help is not an error.
    assert_status(&run(&["check-links", "--help"]), 0);
}
