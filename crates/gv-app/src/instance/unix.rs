// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The single-instance endpoint on Linux and macOS: a Unix domain socket in
//! a private folder.
//!
//! | Runtime folder | Socket |
//! |---|---|
//! | `XDG_RUNTIME_DIR` set (Linux) | `$XDG_RUNTIME_DIR/glyphvane/<key>.sock` |
//! | otherwise | `DATA/run/instance.sock` |
//! | otherwise, if that path is too long for a socket | `$TMPDIR/glyphvane-<uid>/<key>.sock` |
//!
//! `<key>` is a hash of the data folder, so each `--config-dir` is its own
//! instance. The lock file sits next to the socket, named alike.

use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, FileTypeExt as _, MetadataExt as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gv_settings::Locations;

use super::protocol::Response;
use super::{Handler, client_exchange, remaining, serve_connection};

/// The name of the socket and lock in the data folder, which is the
/// instance's own.
const NAME_IN_DATA: &str = "instance";

/// The longest socket path every platform accepts: `sun_path` holds 104
/// bytes on macOS and 108 on Linux, with a terminating NUL.
const MAX_SOCKET_PATH: usize = 103;

/// How often a client retries while the first instance has its lock but is
/// not listening yet.
const RETRY: Duration = Duration::from_millis(20);

/// How long the server waits for a connected client to send its request.
const SERVER_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// The folder holding the socket and the lock file, and their name.
#[derive(Debug, Clone)]
pub struct Endpoint {
    dir: PathBuf,
    name: String,
    uid: u32,
}

/// What a path in the endpoint folder must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Dir,
    Socket,
    File,
}

impl Endpoint {
    /// The endpoint for `locations` (see the module documentation).
    pub fn new(locations: &Locations, runtime_dir: Option<PathBuf>) -> Self {
        let uid = rustix::process::geteuid().as_raw();
        let data = locations.data_dir();
        let key = format!("{:016x}", super::fnv1a(data.as_os_str().as_encoded_bytes()));
        let endpoint = |dir: PathBuf, name: &str| Self {
            dir,
            name: name.to_owned(),
            uid,
        };
        if let Some(runtime) = runtime_dir {
            return endpoint(runtime.join("glyphvane"), &key);
        }
        let in_data = endpoint(data.join("run"), NAME_IN_DATA);
        if in_data.socket().as_os_str().len() <= MAX_SOCKET_PATH {
            return in_data;
        }
        endpoint(std::env::temp_dir().join(format!("glyphvane-{uid}")), &key)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub(super) fn socket(&self) -> PathBuf {
        self.dir.join(format!("{}.sock", self.name))
    }

    pub(super) fn lock_path(&self) -> PathBuf {
        self.dir.join(format!("{}.lock", self.name))
    }

    pub(super) fn describe(&self) -> String {
        self.socket().display().to_string()
    }

    /// Creates the folder, mode 0700, or checks that the existing one is a
    /// real folder owned by the user, and takes away any access others have.
    pub(super) fn prepare(&self) -> io::Result<()> {
        if let Some(parent) = self.dir.parent() {
            fs::create_dir_all(parent)?;
        }
        match DirBuilder::new().mode(0o700).create(&self.dir) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
        let meta = fs::symlink_metadata(&self.dir)?;
        check_owned(&self.dir, &meta, self.uid, Kind::Dir)?;
        if meta.mode() & 0o077 != 0 {
            tracing::warn!(
                "single instance: {} was open to other users; making it private",
                self.dir.display()
            );
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Takes the instance lock; `None` when another process holds it.
    pub(super) fn try_lock(&self) -> io::Result<Option<File>> {
        let path = self.lock_path();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)?;
        check_owned(&path, &file.metadata()?, self.uid, Kind::File)?;
        super::try_lock_file(file)
    }

    /// Listens on the socket. Called with the lock held, so a socket that
    /// is there already belongs to an instance that has ended: it is
    /// removed first.
    pub(super) fn listen(&self) -> io::Result<Listener> {
        let socket = self.socket();
        match fs::symlink_metadata(&socket) {
            Ok(meta) => {
                check_owned(&socket, &meta, self.uid, Kind::Socket)?;
                fs::remove_file(&socket)?;
                tracing::info!(
                    "single instance: removed the stale socket {}",
                    socket.display()
                );
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let listener = UnixListener::bind(&socket)?;
        // Nobody else can enter the folder; the mode is a second fence.
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        Ok(Listener {
            listener,
            uid: self.uid,
        })
    }

    /// Connects, sends `body` and reads the answer by `deadline`. While the
    /// first instance is still starting (lock taken, socket not yet there)
    /// the connection is retried.
    pub(super) fn exchange(&self, body: &[u8], deadline: Instant) -> io::Result<Response> {
        let socket = self.socket();
        let mut stream = loop {
            match self.connect(&socket) {
                Ok(stream) => break stream,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    remaining(deadline)?;
                    std::thread::sleep(RETRY);
                }
                Err(err) => return Err(err),
            }
        };
        let left = remaining(deadline)?;
        stream.set_read_timeout(Some(left))?;
        stream.set_write_timeout(Some(left))?;
        client_exchange(&mut stream, body).map_err(|err| match err.kind() {
            io::ErrorKind::WouldBlock => {
                io::Error::new(io::ErrorKind::TimedOut, "no answer in time")
            }
            _ => err,
        })
    }

    /// Connects to the socket after checking that it is the user's.
    fn connect(&self, socket: &Path) -> io::Result<UnixStream> {
        let meta = fs::symlink_metadata(socket)?;
        check_owned(socket, &meta, self.uid, Kind::Socket)?;
        if meta.mode() & 0o077 != 0 {
            return Err(permission(socket, "is open to other users"));
        }
        UnixStream::connect(socket)
    }

    /// Removes the socket at exit.
    pub(super) fn remove(&self) {
        let _ = fs::remove_file(self.socket());
    }
}

/// The listening socket.
#[derive(Debug)]
pub struct Listener {
    listener: UnixListener,
    /// Checked against the client's credentials where the system reports
    /// them (Linux).
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    uid: u32,
}

impl Listener {
    /// Serves connections one after another, so requests open in the order
    /// they came. Runs until the process ends.
    pub(super) fn run(self, handler: &Handler) {
        for stream in self.listener.incoming() {
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(err) => {
                    tracing::warn!("single instance: accept failed: {err}");
                    // Do not spin when, for example, file descriptors run out.
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            if !self.peer_is_user(&stream) {
                continue;
            }
            let timeouts = stream
                .set_read_timeout(Some(SERVER_READ_TIMEOUT))
                .and_then(|()| stream.set_write_timeout(Some(SERVER_READ_TIMEOUT)));
            if let Err(err) = timeouts {
                tracing::warn!("single instance: {err}");
                continue;
            }
            serve_connection(&mut stream, handler);
        }
    }

    /// Whether the connecting process runs as the user. Only Linux reports
    /// it; elsewhere the private folder is the check.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn peer_is_user(&self, stream: &UnixStream) -> bool {
        match rustix::net::sockopt::socket_peercred(stream) {
            Ok(cred) if cred.uid.as_raw() == self.uid => true,
            Ok(cred) => {
                tracing::warn!(
                    "single instance: refused a connection from user {}",
                    cred.uid.as_raw()
                );
                false
            }
            Err(err) => {
                tracing::warn!("single instance: cannot identify a client: {err}");
                false
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn peer_is_user(&self, _stream: &UnixStream) -> bool {
        true
    }
}

/// Checks that `meta` (of `path`, not followed if a link) is a `kind` owned
/// by `uid`.
pub(super) fn check_owned(path: &Path, meta: &Metadata, uid: u32, kind: Kind) -> io::Result<()> {
    let file_type = meta.file_type();
    let right_kind = match kind {
        Kind::Dir => file_type.is_dir(),
        Kind::Socket => file_type.is_socket(),
        Kind::File => file_type.is_file(),
    };
    if !right_kind {
        return Err(permission(
            path,
            &format!("is not a {kind:?}").to_lowercase(),
        ));
    }
    if meta.uid() != uid {
        return Err(permission(
            path,
            &format!("belongs to user {}, not {uid}", meta.uid()),
        ));
    }
    Ok(())
}

fn permission(path: &Path, what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{} {what}", path.display()),
    )
}
