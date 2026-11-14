// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `glyphvane --wait`, run as a process against a stand-in for the running
//! instance: the test takes the instance lock and listens on the socket of
//! a temporary `--config-dir`, answers the request, and decides when the
//! files are closed. No window is opened (there is no display), so a
//! client that failed to reach the stand-in would fail instead of starting
//! a GUI.

#![cfg(unix)]

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long any step may take before the test gives up.
const LIMIT: Duration = Duration::from_secs(10);

/// `glyphvane` with no display and no runtime folder, so that the socket of
/// `--config-dir DIR` is `DIR/data/run/instance.sock`.
fn glyphvane(config: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_glyphvane"));
    cmd.env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("RUST_LOG")
        .arg("--config-dir")
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// A stand-in for the running instance of `--config-dir root`.
struct FakeInstance {
    _lock: File,
    listener: UnixListener,
}

impl FakeInstance {
    fn new(root: &Path) -> Self {
        let run = root.join("data").join("run");
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&run)
            .expect("run folder");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(run.join("instance.lock"))
            .expect("lock file");
        lock.try_lock().expect("the instance lock");
        let socket = run.join("instance.sock");
        let listener = UnixListener::bind(&socket).expect("socket");
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        listener.set_nonblocking(true).expect("non-blocking");
        Self {
            _lock: lock,
            listener,
        }
    }

    /// The next client, and the request it sent.
    fn accept(&self, child: &mut Child) -> (UnixStream, serde_json::Value) {
        let deadline = Instant::now() + LIMIT;
        let mut stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    if let Ok(Some(status)) = child.try_wait() {
                        panic!("glyphvane ended without connecting: {status}");
                    }
                    assert!(Instant::now() < deadline, "no client connected");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("accept: {err}"),
            }
        };
        stream.set_nonblocking(false).expect("blocking");
        stream.set_read_timeout(Some(LIMIT)).expect("timeout");
        let request = read_frame(&mut stream);
        (stream, request)
    }
}

fn read_frame(stream: &mut UnixStream) -> serde_json::Value {
    let mut header = [0; 8];
    stream.read_exact(&mut header).expect("frame header");
    assert_eq!(&header[..4], b"GLYV");
    let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let mut body = vec![0; len];
    stream.read_exact(&mut body).expect("frame body");
    serde_json::from_slice(&body).expect("a JSON body")
}

fn write_frame(stream: &mut UnixStream, body: &serde_json::Value) {
    let body = serde_json::to_vec(body).expect("json");
    let mut frame = b"GLYV".to_vec();
    frame.extend_from_slice(&u32::try_from(body.len()).expect("small").to_le_bytes());
    frame.extend_from_slice(&body);
    stream.write_all(&frame).expect("frame written");
}

/// Waits for `child` to end, killing it after [`LIMIT`].
fn finish(mut child: Child) -> Output {
    let deadline = Instant::now() + LIMIT;
    while child.try_wait().expect("status").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().expect("output")
}

/// Asserts that `child` is still running after a while.
fn still_waiting(child: &mut Child, how_long: Duration) {
    std::thread::sleep(how_long);
    if let Some(status) = child.try_wait().expect("status") {
        panic!("glyphvane stopped waiting: {status}");
    }
}

fn utf8(path: &Path) -> serde_json::Value {
    serde_json::json!({ "utf8": path.to_str().expect("a Unicode temp path") })
}

fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = dir.path().join("cfg");
    let message = dir.path().join("COMMIT_EDITMSG");
    std::fs::write(&message, "\n# Please enter the commit message.\n").expect("file");
    (dir, config, message)
}

#[test]
fn the_client_blocks_until_the_instance_reports_the_file_closed() {
    let (_dir, config, message) = setup();
    let instance = FakeInstance::new(&config);
    let mut child = glyphvane(&config)
        .arg("--wait")
        .arg(&message)
        .spawn()
        .expect("glyphvane starts");

    let (mut stream, request) = instance.accept(&mut child);
    assert_eq!(request["version"], 1);
    assert_eq!(request["command"], "open");
    assert_eq!(request["wait"], true);
    assert_eq!(request["files"][0]["path"], utf8(&message));
    write_frame(
        &mut stream,
        &serde_json::json!({"version": 1, "status": "accepted"}),
    );

    // Accepted, but the file is still open: longer than the time a plain
    // hand-over may take (1.5 s), the client waits.
    still_waiting(&mut child, Duration::from_secs(2));

    // An event of a later version is skipped.
    write_frame(
        &mut stream,
        &serde_json::json!({"version": 1, "event": "progress"}),
    );
    write_frame(
        &mut stream,
        &serde_json::json!({"version": 1, "event": "closed", "path": utf8(&message), "unsaved": true}),
    );
    still_waiting(&mut child, Duration::from_millis(300));
    write_frame(
        &mut stream,
        &serde_json::json!({"version": 1, "event": "done"}),
    );

    let output = finish(child);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{:?}: {stderr}", output.status);
    assert!(
        stderr.contains("closed with changes that are not saved"),
        "unsaved changes are mentioned: {stderr}"
    );
}

#[test]
fn a_saved_file_ends_the_wait_quietly() {
    let (_dir, config, message) = setup();
    let instance = FakeInstance::new(&config);
    let mut child = glyphvane(&config)
        .arg("--wait")
        .arg(&message)
        .spawn()
        .expect("glyphvane starts");
    let (mut stream, _) = instance.accept(&mut child);
    write_frame(
        &mut stream,
        &serde_json::json!({"version": 1, "status": "accepted"}),
    );
    write_frame(
        &mut stream,
        &serde_json::json!({"version": 1, "event": "closed", "path": utf8(&message), "unsaved": false}),
    );
    write_frame(
        &mut stream,
        &serde_json::json!({"version": 1, "event": "done"}),
    );
    let output = finish(child);
    assert!(output.status.success(), "{:?}", output.status);
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_client_fails_when_the_instance_ends_first() {
    let (_dir, config, message) = setup();
    let instance = FakeInstance::new(&config);
    let mut child = glyphvane(&config)
        .arg("--wait")
        .arg(&message)
        .spawn()
        .expect("glyphvane starts");
    let (mut stream, _) = instance.accept(&mut child);
    write_frame(
        &mut stream,
        &serde_json::json!({"version": 1, "status": "accepted"}),
    );
    still_waiting(&mut child, Duration::from_millis(300));
    // The instance crashes or quits without reporting.
    drop(stream);
    drop(instance);

    let output = finish(child);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("ended before the files were closed"),
        "{stderr}"
    );
}

#[test]
fn an_ordinary_open_does_not_wait() {
    let (_dir, config, message) = setup();
    let instance = FakeInstance::new(&config);
    let mut child = glyphvane(&config)
        .arg(&message)
        .spawn()
        .expect("glyphvane starts");
    let (mut stream, request) = instance.accept(&mut child);
    assert_eq!(request["wait"], false);
    write_frame(
        &mut stream,
        &serde_json::json!({"version": 1, "status": "accepted"}),
    );
    let output = finish(child);
    assert!(output.status.success(), "{:?}", output.status);
}

#[test]
fn wait_needs_a_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = finish(
        glyphvane(&dir.path().join("cfg"))
            .arg("--wait")
            .spawn()
            .expect("glyphvane starts"),
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--wait needs a file"));
}
