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

mod protocol;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gv_settings::Locations;

use crate::cli::OpenRequest;
use protocol::{Response, Status};
#[cfg(unix)]
pub use unix::Endpoint;
#[cfg(windows)]
pub use windows::Endpoint;

/// How long a second instance waits for the first to take its request
/// before starting on its own.
pub const TIMEOUT: Duration = Duration::from_millis(1500);

/// Takes a request on the listening thread and passes it to the UI. The
/// error is the reason given to the client.
pub type Handler = Arc<dyn Fn(OpenRequest) -> Result<(), String> + Send + Sync>;

/// The outcome of [`claim`].
#[derive(Debug)]
pub enum Claim {
    /// This process is the first instance. Start serving with
    /// [`Primary::serve`].
    Primary(Primary),
    /// The running instance took the request; this process can exit.
    Delivered,
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
}

impl Drop for Guard {
    fn drop(&mut self) {
        // The lock is still held here, so the endpoint is ours to remove.
        self.endpoint.remove();
    }
}

#[cfg(unix)]
use unix::Listener;
#[cfg(windows)]
use windows::Listener;

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
            Ok(()) => {
                tracing::info!("single instance: handed over to the running instance");
                Claim::Delivered
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
        let spawned = std::thread::Builder::new()
            .name("glyphvane-instance".to_owned())
            .spawn(move || listener.run(&handler));
        if let Err(err) = spawned {
            tracing::warn!("single instance: cannot start the listener thread: {err}");
        }
        Guard {
            _lock: lock,
            endpoint,
        }
    }
}

/// Sends `request` and waits for the answer, all within `timeout`.
fn send(endpoint: &Endpoint, request: &OpenRequest, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let body = protocol::encode_request(request);
    let response = endpoint.exchange(&body, deadline)?;
    match response.status {
        Status::Accepted => Ok(()),
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
/// answer.
fn serve_connection(stream: &mut (impl Read + Write), handler: &Handler) {
    let response = match protocol::read_frame(stream) {
        Ok(body) => match protocol::decode_request(&body) {
            Ok(request) if request.wait => {
                Response::rejected("--wait is not supported by this version")
            }
            Ok(request) => {
                tracing::info!(
                    "single instance: request for {} file(s) and {} folder(s){}",
                    request.files.len(),
                    request.folders.len(),
                    if request.new_window {
                        " in a new window"
                    } else {
                        ""
                    }
                );
                match handler(request) {
                    Ok(()) => Response::accepted(),
                    Err(reason) => Response::rejected(reason),
                }
            }
            Err(reason) => {
                tracing::warn!("single instance: rejected a request: {reason}");
                Response::rejected(reason)
            }
        },
        Err(err) => {
            tracing::warn!("single instance: unreadable request: {err}");
            return;
        }
    };
    if let Err(err) = protocol::write_frame(stream, &response.encode()) {
        tracing::warn!("single instance: cannot answer: {err}");
    }
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
