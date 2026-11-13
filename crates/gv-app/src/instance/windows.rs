// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The single-instance endpoint on Windows: a named pipe per user.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gv_settings::Locations;
use interprocess::ConnectWaitMode;
use interprocess::os::windows::named_pipe::{
    DuplexPipeStream, PipeListener, PipeListenerOptions, PipeMode, pipe_mode,
};

use super::protocol::Response;
use super::{Handler, client_exchange, remaining, serve_connection};

const LOCK: &str = "instance.lock";

/// How often a client retries while the first instance has its lock but is
/// not listening yet.
const RETRY: Duration = Duration::from_millis(20);

type Stream = DuplexPipeStream<pipe_mode::Bytes>;

/// The pipe and the folder holding the lock file.
#[derive(Debug, Clone)]
pub struct Endpoint {
    dir: PathBuf,
    pipe: String,
}

impl Endpoint {
    /// The lock lives in `DATA/run`; the pipe's name is derived from the
    /// user and the data folder, so each user (and each `--config-dir`) has
    /// its own. Windows has no runtime folder, so `runtime_dir` is unused.
    pub fn new(locations: &Locations, _runtime_dir: Option<PathBuf>) -> Self {
        let user = format!(
            "{}\\{}",
            std::env::var("USERDOMAIN").unwrap_or_default(),
            std::env::var("USERNAME").unwrap_or_default()
        );
        let data = locations.data_dir();
        let key = format!("{user}\0{}", data.display());
        Self {
            dir: data.join("run"),
            pipe: format!(
                r"\\.\pipe\io.github.sorrelfox.Glyphvane-{:016x}",
                super::fnv1a(key.as_bytes())
            ),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub(super) fn describe(&self) -> String {
        self.pipe.clone()
    }

    /// The folder inherits the private access of the user's local app data.
    pub(super) fn prepare(&self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)
    }

    /// Takes the instance lock; `None` when another process holds it.
    pub(super) fn try_lock(&self) -> io::Result<Option<File>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join(LOCK))?;
        super::try_lock_file(file)
    }

    /// Creates the pipe as the first instance of its name, with the default
    /// security and remote clients refused.
    pub(super) fn listen(&self) -> io::Result<Listener> {
        let listener = PipeListenerOptions::new()
            .path(self.pipe.as_str())
            .mode(PipeMode::Bytes)
            .accept_remote(false)
            .create_duplex::<pipe_mode::Bytes>()?;
        Ok(Listener { listener })
    }

    /// Connects, sends `body` and reads the answer by `deadline`. Pipe I/O
    /// has no timeouts, so the exchange runs on a helper thread that is
    /// abandoned when the deadline passes.
    pub(super) fn exchange(&self, body: &[u8], deadline: Instant) -> io::Result<Response> {
        let pipe = self.pipe.clone();
        let body = body.to_vec();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("glyphvane-instance-client".to_owned())
            .spawn(move || {
                let result = connect(&pipe, deadline)
                    .and_then(|mut stream| client_exchange(&mut stream, &body));
                let _ = sender.send(result);
            })?;
        match receiver.recv_timeout(remaining(deadline)?) {
            Ok(result) => result,
            Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "no answer in time")),
        }
    }

    /// Named pipes disappear with their process: nothing to remove.
    pub(super) fn remove(&self) {}
}

fn connect(pipe: &str, deadline: Instant) -> io::Result<Stream> {
    loop {
        let wait = ConnectWaitMode::Timeout(remaining(deadline)?);
        match Stream::connect_by_path_with_wait_mode(pipe, wait) {
            Ok(stream) => return Ok(stream),
            Err(err) if err.kind() == io::ErrorKind::NotFound => std::thread::sleep(RETRY),
            Err(err) => return Err(err),
        }
    }
}

/// The listening pipe.
#[derive(Debug)]
pub struct Listener {
    listener: PipeListener<pipe_mode::Bytes, pipe_mode::Bytes>,
}

impl Listener {
    /// Serves each connection on its own thread, as pipe reads cannot time
    /// out. Runs until the process ends.
    pub(super) fn run(self, handler: &Handler) {
        for stream in self.listener.incoming() {
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(err) => {
                    tracing::warn!("single instance: accept failed: {err}");
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            if !same_session(&stream) {
                continue;
            }
            let handler = handler.clone();
            let spawned = std::thread::Builder::new()
                .name("glyphvane-instance-client".to_owned())
                .spawn(move || serve_connection(&mut stream, &handler));
            if let Err(err) = spawned {
                tracing::warn!("single instance: {err}");
            }
        }
    }
}

/// Whether the client runs in this process's session (the pipe's default
/// security already limits it to the user).
fn same_session(stream: &Stream) -> bool {
    match (stream.client_session_id(), stream.server_session_id()) {
        (Ok(client), Ok(server)) if client == server => true,
        (Ok(client), Ok(server)) => {
            tracing::warn!(
                "single instance: refused a client from session {client} (this is {server})"
            );
            false
        }
        (Err(err), _) | (_, Err(err)) => {
            tracing::warn!("single instance: cannot identify a client: {err}");
            false
        }
    }
}
