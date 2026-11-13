// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use gv_settings::Locations;
use gv_ui::{OpenTarget, TextPosition};

use super::*;

fn endpoint_in(root: &Path) -> Endpoint {
    endpoint(&Locations::with_root(root), None)
}

fn request(root: &Path, name: &str, new_window: bool) -> OpenRequest {
    OpenRequest {
        cwd: root.to_path_buf(),
        files: vec![OpenTarget::new(root.join(name)).at(TextPosition {
            line: 7,
            column: None,
        })],
        folders: Vec::new(),
        new_window,
        wait: false,
    }
}

/// A handler that passes requests to the returned receiver.
fn channel_handler() -> (Handler, mpsc::Receiver<OpenRequest>) {
    let (sender, receiver) = mpsc::channel();
    let sender = Mutex::new(sender);
    let handler: Handler = Arc::new(move |request| {
        sender
            .lock()
            .map_err(|_| "poisoned".to_owned())?
            .send(request)
            .map_err(|_| "closed".to_owned())
    });
    (handler, receiver)
}

fn primary(endpoint: Endpoint, root: &Path) -> Primary {
    match claim(endpoint, &request(root, "first.md", false), TIMEOUT) {
        Claim::Primary(primary) => primary,
        other => panic!("expected to be the first instance, got {other:?}"),
    }
}

#[test]
fn a_second_instance_hands_its_request_to_the_first() {
    let root = tempfile::tempdir().expect("temp dir");
    let (handler, received) = channel_handler();
    let guard = primary(endpoint_in(root.path()), root.path()).serve(handler);

    let second = request(root.path(), "second.md", true);
    let started = Instant::now();
    let claim = claim(endpoint_in(root.path()), &second, TIMEOUT);
    assert!(matches!(claim, Claim::Delivered), "{claim:?}");
    assert!(started.elapsed() < Duration::from_secs(1));
    let got = received
        .recv_timeout(Duration::from_secs(5))
        .expect("request arrives");
    assert_eq!(got, second);

    // Another one, to show the listener keeps going.
    let third = request(root.path(), "third.md", false);
    assert!(matches!(
        super::claim(endpoint_in(root.path()), &third, TIMEOUT),
        Claim::Delivered
    ));
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)).ok(),
        Some(third)
    );
    drop(guard);
}

#[test]
fn a_rejected_request_starts_on_its_own() {
    let root = tempfile::tempdir().expect("temp dir");
    let handler: Handler = Arc::new(|_| Err("not now".to_owned()));
    let _guard = primary(endpoint_in(root.path()), root.path()).serve(handler);
    let claim = claim(
        endpoint_in(root.path()),
        &request(root.path(), "b.md", false),
        TIMEOUT,
    );
    assert!(matches!(claim, Claim::Standalone), "{claim:?}");
}

#[test]
fn wait_is_refused_until_it_is_supported() {
    let root = tempfile::tempdir().expect("temp dir");
    let (handler, received) = channel_handler();
    let _guard = primary(endpoint_in(root.path()), root.path()).serve(handler);
    let mut waiting = request(root.path(), "b.md", false);
    waiting.wait = true;
    let claim = claim(endpoint_in(root.path()), &waiting, TIMEOUT);
    assert!(matches!(claim, Claim::Standalone), "{claim:?}");
    assert!(received.try_recv().is_err());
}

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::os::unix::net::UnixListener;

    use super::super::unix::{Kind, check_owned};
    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).expect("metadata").mode() & 0o777
    }

    #[test]
    fn the_folder_and_socket_are_private() {
        let root = tempfile::tempdir().expect("temp dir");
        let endpoint = endpoint_in(root.path());
        // A folder someone left open is closed before use.
        fs::create_dir_all(endpoint.dir()).expect("folder");
        fs::set_permissions(endpoint.dir(), fs::Permissions::from_mode(0o755)).expect("chmod");

        let dir = endpoint.dir().to_path_buf();
        let socket = endpoint.socket();
        let lock = endpoint.lock_path();
        let primary = primary(endpoint, root.path());
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&socket), 0o600);
        assert_eq!(mode(&lock), 0o600);

        // The socket is removed at exit.
        let guard = primary.serve(channel_handler().0);
        drop(guard);
        assert!(!socket.exists());
    }

    #[test]
    fn each_data_folder_has_its_own_socket_in_the_runtime_folder() {
        let runtime = tempfile::tempdir().expect("runtime dir");
        let roots = tempfile::tempdir().expect("temp dir");
        let endpoint_for = |name: &str| {
            let locations = Locations::with_root(roots.path().join(name));
            endpoint(&locations, Some(runtime.path().to_path_buf()))
        };
        let (a, b) = (endpoint_for("a"), endpoint_for("b"));
        assert_eq!(a.dir(), runtime.path().join("glyphvane"));
        assert_eq!(a.dir(), b.dir());
        assert_ne!(a.socket(), b.socket());
        assert_eq!(a.socket(), endpoint_for("a").socket());

        // A round trip through the runtime folder.
        let (handler, received) = channel_handler();
        let _guard = primary(a, roots.path()).serve(handler);
        let second = request(roots.path(), "b.md", false);
        assert!(matches!(
            claim(endpoint_for("a"), &second, TIMEOUT),
            Claim::Delivered
        ));
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)).ok(),
            Some(second)
        );
        // The other data folder is another instance.
        assert!(matches!(
            claim(
                endpoint_for("b"),
                &request(roots.path(), "c.md", false),
                TIMEOUT
            ),
            Claim::Primary(_)
        ));
    }

    #[test]
    fn a_data_folder_too_deep_for_a_socket_falls_back_to_a_short_path() {
        let deep = Path::new("/").join("deep-folder-name".repeat(8));
        let endpoint = endpoint(&Locations::with_root(&deep), None);
        let uid = rustix::process::geteuid().as_raw();
        assert_eq!(
            endpoint.dir(),
            std::env::temp_dir().join(format!("glyphvane-{uid}"))
        );
        assert!(endpoint.socket().as_os_str().len() <= 103);
        // A short one stays in the data folder.
        let short = endpoint_in(Path::new("/r"));
        assert_eq!(short.socket(), Path::new("/r/data/run/instance.sock"));
    }

    #[test]
    fn a_new_folder_is_created_private() {
        let root = tempfile::tempdir().expect("temp dir");
        let endpoint = endpoint_in(root.path());
        let dir = endpoint.dir().to_path_buf();
        let _primary = primary(endpoint, root.path());
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let root = tempfile::tempdir().expect("temp dir");
        let endpoint = endpoint_in(root.path());
        endpoint.prepare().expect("folder");
        // An instance that crashed leaves its socket behind.
        drop(UnixListener::bind(endpoint.socket()).expect("bind"));
        assert!(endpoint.socket().exists());
        let stale = fs::metadata(endpoint.socket()).expect("socket").ino();

        let (handler, received) = channel_handler();
        let _guard = primary(endpoint.clone(), root.path()).serve(handler);
        assert_ne!(
            fs::metadata(endpoint.socket()).expect("socket").ino(),
            stale
        );
        let second = request(root.path(), "b.md", false);
        assert!(matches!(
            claim(endpoint_in(root.path()), &second, TIMEOUT),
            Claim::Delivered
        ));
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)).ok(),
            Some(second)
        );
    }

    #[test]
    fn an_unresponsive_instance_is_given_up_on() {
        let root = tempfile::tempdir().expect("temp dir");
        let endpoint = endpoint_in(root.path());
        endpoint.prepare().expect("folder");
        // The lock is held and the socket accepts connections into its
        // backlog, but nobody reads them: a hung first instance.
        let _lock = endpoint.try_lock().expect("lock file").expect("lock");
        let _hung = UnixListener::bind(endpoint.socket()).expect("bind");
        fs::set_permissions(endpoint.socket(), fs::Permissions::from_mode(0o600)).expect("chmod");

        let started = Instant::now();
        let timeout = Duration::from_millis(300);
        let claim = claim(
            endpoint_in(root.path()),
            &request(root.path(), "b.md", false),
            timeout,
        );
        assert!(matches!(claim, Claim::Standalone), "{claim:?}");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= timeout && elapsed < Duration::from_secs(2),
            "{elapsed:?}"
        );
    }

    #[test]
    fn a_locked_endpoint_without_a_socket_times_out() {
        let root = tempfile::tempdir().expect("temp dir");
        let endpoint = endpoint_in(root.path());
        endpoint.prepare().expect("folder");
        let _lock = endpoint.try_lock().expect("lock file").expect("lock");
        let claim = claim(
            endpoint_in(root.path()),
            &request(root.path(), "b.md", false),
            Duration::from_millis(200),
        );
        assert!(matches!(claim, Claim::Standalone), "{claim:?}");
    }

    #[test]
    fn a_socket_that_is_not_a_socket_is_not_trusted() {
        let root = tempfile::tempdir().expect("temp dir");
        let endpoint = endpoint_in(root.path());
        endpoint.prepare().expect("folder");
        let _lock = endpoint.try_lock().expect("lock file").expect("lock");
        fs::write(endpoint.socket(), "").expect("plain file");
        let started = Instant::now();
        let claim = claim(
            endpoint_in(root.path()),
            &request(root.path(), "b.md", false),
            TIMEOUT,
        );
        assert!(matches!(claim, Claim::Standalone), "{claim:?}");
        // Refused at once, not after the timeout.
        assert!(started.elapsed() < TIMEOUT);
    }

    #[test]
    fn a_socket_open_to_others_is_not_trusted() {
        let root = tempfile::tempdir().expect("temp dir");
        let endpoint = endpoint_in(root.path());
        endpoint.prepare().expect("folder");
        let _lock = endpoint.try_lock().expect("lock file").expect("lock");
        let _listener = UnixListener::bind(endpoint.socket()).expect("bind");
        fs::set_permissions(endpoint.socket(), fs::Permissions::from_mode(0o666)).expect("chmod");
        let claim = claim(
            endpoint_in(root.path()),
            &request(root.path(), "b.md", false),
            TIMEOUT,
        );
        assert!(matches!(claim, Claim::Standalone), "{claim:?}");
    }

    #[test]
    fn a_symlinked_folder_is_refused() {
        let root = tempfile::tempdir().expect("temp dir");
        let elsewhere = root.path().join("elsewhere");
        fs::create_dir(&elsewhere).expect("folder");
        let endpoint = endpoint_in(root.path());
        fs::create_dir_all(root.path().join("data")).expect("data folder");
        std::os::unix::fs::symlink(&elsewhere, endpoint.dir()).expect("symlink");
        let err = endpoint.prepare().expect_err("symlink refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let claim = claim(endpoint, &request(root.path(), "b.md", false), TIMEOUT);
        assert!(matches!(claim, Claim::Standalone), "{claim:?}");
    }

    #[test]
    fn ownership_and_kind_are_checked() {
        let root = tempfile::tempdir().expect("temp dir");
        let uid = rustix::process::geteuid().as_raw();
        let dir = root.path();
        let meta = fs::symlink_metadata(dir).expect("metadata");
        assert!(check_owned(dir, &meta, uid, Kind::Dir).is_ok());
        // Somebody else's folder.
        let err = check_owned(dir, &meta, uid.wrapping_add(1), Kind::Dir).expect_err("other user");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        // The wrong kind of file.
        let err = check_owned(dir, &meta, uid, Kind::Socket).expect_err("not a socket");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }
}
