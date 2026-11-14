// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Single instance: the first GUI process serves a per-user local endpoint,
//! and later invocations hand it their paths and exit.
//!
//! Who is first is decided by an exclusive lock on a lock file in the
//! endpoint's folder, held for the life of the process. The operating
//! system drops the lock when the process ends, however it ends, so an
//! endpoint left behind by a crash is recognised as stale: whoever gets the
//! lock removes it and listens afresh.
//!
//! - Linux and macOS: a Unix domain socket in a per-user runtime folder that
//!   only the user can enter (mode 0700, owned by the user, checked before
//!   use), the socket itself mode 0600 and checked by the client. On Linux
//!   the server also checks the connecting process's user id.
//! - Windows: a named pipe whose name is derived from the user and the data
//!   folder, created with the default security (only the user, SYSTEM and
//!   administrators may write to it), refusing remote clients and clients
//!   from other sessions, and created as the first instance of its name so
//!   that a pipe squatted by another process is never joined.
//!
//! A `--wait` request keeps its connection: the server reports each file as
//! its tab lets go of it, then that all are done ([`protocol`]), from a
//! thread of its own so that other invocations are not held up.

mod protocol;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gv_settings::Locations;

use crate::cli::OpenRequest;
pub use protocol::WaitEvent;
use protocol::{Response, Status};
#[cfg(unix)]
pub use unix::Endpoint;
#[cfg(windows)]
pub use windows::Endpoint;

/// How long a second instance waits for the first to take its request
/// before starting on its own.
pub const TIMEOUT: Duration = Duration::from_millis(1500);

/// Takes a request on the listening thread and passes it to the UI. A
/// `--wait` request comes with the [`Reporter`] to tell its files' closing
/// to. The error is the reason given to the client.
pub type Handler = Arc<dyn Fn(OpenRequest, Option<Reporter>) -> Result<(), String> + Send + Sync>;

/// Tells a waiting client (`--wait`) about its files. Dropping it before
/// [`Reporter::done`] ends the connection without `done`, which the client
/// reports as a failure.
#[derive(Debug)]
pub struct Reporter(mpsc::Sender<WaitEvent>);

impl Reporter {
    pub(crate) fn channel() -> (Self, mpsc::Receiver<WaitEvent>) {
        let (sender, receiver) = mpsc::channel();
        (Self(sender), receiver)
    }

    /// The tab let go of the file at `path`.
    pub fn closed(&self, path: PathBuf, unsaved: bool) {
        // The client may be gone; nothing is left to tell then.
        let _ = self.0.send(WaitEvent::Closed { path, unsaved });
    }

    /// Every file is closed.
    pub fn done(self) {
        let _ = self.0.send(WaitEvent::Done);
    }
}

/// The outcome of [`claim`].
#[derive(Debug)]
pub enum Claim {
    /// This process is the first instance. Start serving with
    /// [`Primary::serve`].
    Primary(Primary),
    /// The running instance took the request; this process can exit.
    Delivered,
    /// The running instance took a `--wait` request: wait with
    /// [`Waiting::wait`] until it reports the files closed.
    Waiting(Waiting),
    /// There is no usable single-instance endpoint, or the running instance
    /// did not answer: this process runs on its own without serving.
    Standalone,
}

/// The first instance's lock and listener.
#[derive(Debug)]
pub struct Primary {
    lock: File,
    listener: Listener,
    endpoint: Endpoint,
}

/// Keeps the instance's lock until dropped, then removes the endpoint.
#[derive(Debug)]
pub struct Guard {
    _lock: File,
    endpoint: Endpoint,
    waiters: Waiters,
}

impl Guard {
    /// The threads that report to waiting clients.
    pub fn waiters(&self) -> Waiters {
        self.waiters.clone()
    }
}

/// The threads that report to waiting clients (`--wait`), so that the
/// process can let them finish before it exits.
#[derive(Debug, Clone, Default)]
pub struct Waiters(Arc<Mutex<Vec<JoinHandle<()>>>>);

impl Waiters {
    /// Runs `work` on a thread of its own.
    fn spawn(&self, work: impl FnOnce() + Send + 'static) {
        let spawned = std::thread::Builder::new()
            .name("glyphvane-instance-wait".to_owned())
            .spawn(work);
        match spawned {
            Ok(handle) => {
                let mut threads = self.0.lock().unwrap_or_else(PoisonError::into_inner);
                threads.retain(|t| !t.is_finished());
                threads.push(handle);
            }
            Err(err) => tracing::warn!("single instance: cannot start a thread: {err}"),
        }
    }

    /// Waits until every thread has finished, for at most `limit`. Returns
    /// whether they all did.
    pub fn finish(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            let running = self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .any(|t| !t.is_finished());
            if !running {
                return true;
            }
            if Instant::now() >= deadline {
                tracing::warn!("single instance: a waiting client was not told in time");
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// A connection to the running instance, which has accepted a `--wait`
/// request.
#[derive(Debug)]
pub struct Waiting {
    stream: Connection,
}

impl Waiting {
    /// Blocks until the running instance reports every file closed, passing
    /// each event to `on_event`. An error when the connection ends before
    /// that: the instance ended, or could not open the files.
    pub fn wait(mut self, mut on_event: impl FnMut(&WaitEvent)) -> io::Result<()> {
        unbounded(&self.stream)?;
        loop {
            let body = protocol::read_frame(&mut self.stream).map_err(|err| match err.kind() {
                io::ErrorKind::UnexpectedEof => io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the running instance closed the connection",
                ),
                _ => err,
            })?;
            match protocol::decode_event(&body)? {
                Some(event) => {
                    on_event(&event);
                    if event == WaitEvent::Done {
                        return Ok(());
                    }
                }
                None => tracing::debug!("single instance: skipped an unknown event"),
            }
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // The lock is still held here, so the endpoint is ours to remove.
        self.endpoint.remove();
    }
}

#[cfg(unix)]
use unix::{Connection, Listener, unbounded};
#[cfg(windows)]
use windows::{Connection, Listener, unbounded};

/// The endpoint for `locations`, which identify the instance. `runtime_dir`
/// is a per-user folder for sockets (`XDG_RUNTIME_DIR`); without one the
/// data folder is used.
pub fn endpoint(locations: &Locations, runtime_dir: Option<PathBuf>) -> Endpoint {
    Endpoint::new(locations, runtime_dir)
}

/// The runtime folder to use: `XDG_RUNTIME_DIR` on Linux and the other
/// freedesktop systems when it is set to an absolute path.
pub fn runtime_dir_from_env() -> Option<PathBuf> {
    if cfg!(any(target_os = "macos", windows)) {
        return None;
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

/// Becomes the first instance, or hands `request` to the running one.
pub fn claim(endpoint: Endpoint, request: &OpenRequest, timeout: Duration) -> Claim {
    if let Err(err) = endpoint.prepare() {
        tracing::warn!(
            "single instance: cannot use {}, so this window runs on its own: {err}",
            endpoint.dir().display()
        );
        return Claim::Standalone;
    }
    match endpoint.try_lock() {
        Ok(Some(lock)) => match endpoint.listen() {
            Ok(listener) => {
                tracing::info!("single instance: listening at {}", endpoint.describe());
                Claim::Primary(Primary {
                    lock,
                    listener,
                    endpoint,
                })
            }
            Err(err) => {
                tracing::warn!(
                    "single instance: cannot listen at {}, so other invocations start their own process: {err}",
                    endpoint.describe()
                );
                Claim::Standalone
            }
        },
        Ok(None) => match send(&endpoint, request, timeout) {
            Ok(None) => {
                tracing::info!("single instance: handed over to the running instance");
                Claim::Delivered
            }
            Ok(Some(waiting)) => {
                tracing::info!(
                    "single instance: handed over to the running instance, waiting for the files to close"
                );
                Claim::Waiting(waiting)
            }
            Err(err) => {
                tracing::warn!(
                    "single instance: the running instance did not take the request, so this one starts on its own: {err}"
                );
                Claim::Standalone
            }
        },
        Err(err) => {
            tracing::warn!("single instance: cannot lock the instance file: {err}");
            Claim::Standalone
        }
    }
}

impl Primary {
    /// Serves later invocations on a background thread, handing their
    /// requests to `handler`. The returned guard holds the instance lock;
    /// keep it for the life of the process.
    pub fn serve(self, handler: Handler) -> Guard {
        let Self {
            lock,
            listener,
            endpoint,
        } = self;
        let waiters = Waiters::default();
        let spawned = std::thread::Builder::new()
            .name("glyphvane-instance".to_owned())
            .spawn({
                let waiters = waiters.clone();
                move || listener.run(&handler, &waiters)
            });
        if let Err(err) = spawned {
            tracing::warn!("single instance: cannot start the listener thread: {err}");
        }
        Guard {
            _lock: lock,
            endpoint,
            waiters,
        }
    }
}

/// Sends `request` and waits for the answer, all within `timeout`. For a
/// `--wait` request, the connection to wait on.
fn send(
    endpoint: &Endpoint,
    request: &OpenRequest,
    timeout: Duration,
) -> io::Result<Option<Waiting>> {
    let deadline = Instant::now() + timeout;
    let body = protocol::encode_request(request);
    let (response, stream) = endpoint.exchange(&body, deadline)?;
    match response.status {
        Status::Accepted => Ok(request.wait.then_some(Waiting { stream })),
        Status::Rejected => Err(io::Error::other(format!(
            "rejected: {}",
            response.reason.unwrap_or_default()
        ))),
    }
}

/// The client's side of one exchange on a connected stream.
fn client_exchange(stream: &mut (impl Read + Write), body: &[u8]) -> io::Result<Response> {
    protocol::write_frame(stream, body)?;
    Response::decode(&protocol::read_frame(stream)?)
}

/// The server's side of one connection: read the request, hand it over,
/// answer. For an accepted `--wait` request, returns the events to pass on
/// with [`report`].
fn serve_connection(
    stream: &mut (impl Read + Write),
    handler: &Handler,
) -> Option<mpsc::Receiver<WaitEvent>> {
    let mut events = None;
    let response = match protocol::read_frame(stream) {
        Ok(body) => match protocol::decode_request(&body) {
            Ok(request) => {
                tracing::info!(
                    "single instance: request for {} file(s) and {} folder(s){}{}",
                    request.files.len(),
                    request.folders.len(),
                    if request.new_window {
                        " in a new window"
                    } else {
                        ""
                    },
                    if request.wait { ", waiting" } else { "" }
                );
                let reporter = request.wait.then(|| {
                    let (reporter, receiver) = Reporter::channel();
                    events = Some(receiver);
                    reporter
                });
                match handler(request, reporter) {
                    Ok(()) => Response::accepted(),
                    Err(reason) => {
                        events = None;
                        Response::rejected(reason)
                    }
                }
            }
            Err(reason) => {
                tracing::warn!("single instance: rejected a request: {reason}");
                Response::rejected(reason)
            }
        },
        Err(err) => {
            tracing::warn!("single instance: unreadable request: {err}");
            return None;
        }
    };
    if let Err(err) = protocol::write_frame(stream, &response.encode()) {
        tracing::warn!("single instance: cannot answer: {err}");
        return None;
    }
    events
}

/// Passes a waiting client its events until `done`. When the UI drops its
/// reporter first (it could not open the files, or the application ends),
/// the connection ends without `done`.
fn report(stream: &mut impl Write, events: &mpsc::Receiver<WaitEvent>) {
    while let Ok(event) = events.recv() {
        if let Err(err) = protocol::write_frame(stream, &protocol::encode_event(&event)) {
            tracing::info!("single instance: a waiting client is gone: {err}");
            return;
        }
        if event == WaitEvent::Done {
            return;
        }
    }
    tracing::warn!("single instance: a waiting client was let go before its files closed");
}

/// Time left until `deadline`, or a timeout error.
fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "no answer in time"))
}

/// Opens the lock file and tries to take its exclusive lock. `None` when
/// another process holds it.
fn try_lock_file(file: File) -> io::Result<Option<File>> {
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(err)) => Err(err),
    }
}

/// 64-bit FNV-1a: a hash that is the same in every build, so every version
/// finds the same endpoint.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests;
