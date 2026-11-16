// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The remote fetcher against a loopback HTTP server started by each test
//! (127.0.0.1, an ephemeral port): nothing leaves the machine.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gv_media::remote::{FetchError, FetchOptions, FetcherConfig, ProxyMode, RemoteFetcher};
use gv_media::{CancelFlag, ImageFormat};

/// What the server answers for a path.
#[derive(Clone)]
enum Reply {
    Ok {
        content_type: &'static str,
        body: Vec<u8>,
        extra: Vec<(&'static str, String)>,
    },
    Redirect(String),
    Status(u16),
    /// Waits, then answers with the reply.
    Slow(Duration, Box<Reply>),
}

/// A request the server saw: its path and headers (lower-case names).
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: HashMap<String, String>,
}

struct Server {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    fn start(routes: Vec<(&'static str, Reply)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let routes: Arc<HashMap<&'static str, Reply>> = Arc::new(routes.into_iter().collect());
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let (routes, log) = (Arc::clone(&routes), Arc::clone(&log));
                std::thread::spawn(move || serve(stream, &routes, &log));
            }
        });
        Self { addr, seen }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().expect("log").clone()
    }
}

fn serve(stream: TcpStream, routes: &HashMap<&'static str, Reply>, log: &Mutex<Vec<Seen>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    log.lock().expect("log").push(Seen {
        path: path.clone(),
        headers,
    });
    let reply = routes
        .get(path.as_str())
        .cloned()
        .unwrap_or(Reply::Status(404));
    answer(stream, reply);
}

fn answer(mut stream: TcpStream, reply: Reply) {
    let (head, body) = match reply {
        Reply::Slow(wait, reply) => {
            std::thread::sleep(wait);
            return answer(stream, *reply);
        }
        Reply::Ok {
            content_type,
            body,
            extra,
        } => {
            let mut head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n",
                body.len()
            );
            for (k, v) in extra {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            (head, body)
        }
        Reply::Redirect(to) => (
            format!("HTTP/1.1 302 Found\r\nlocation: {to}\r\ncontent-length: 0\r\n"),
            Vec::new(),
        ),
        Reply::Status(code) => (
            format!("HTTP/1.1 {code} X\r\ncontent-length: 0\r\n"),
            Vec::new(),
        ),
    };
    let _ = stream.write_all(format!("{head}connection: close\r\n\r\n").as_bytes());
    let _ = stream.write_all(&body);
}

fn png() -> Vec<u8> {
    let mut out = Vec::new();
    let img = image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]));
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("png");
    out
}

fn image(body: Vec<u8>) -> Reply {
    Reply::Ok {
        content_type: "image/png",
        body,
        extra: Vec::new(),
    }
}

fn fetcher(dir: &tempfile::TempDir) -> RemoteFetcher {
    RemoteFetcher::new(FetcherConfig {
        cache_dir: dir.path().join("remote"),
        cache_max_bytes: 1 << 20,
        proxy: ProxyMode::Direct,
    })
}

/// The test server is on loopback: the user approved it.
fn options() -> FetchOptions {
    FetchOptions {
        allow_private: true,
        ..FetchOptions::default()
    }
}

fn any(_: &url::Url) -> bool {
    true
}

#[test]
fn an_image_is_fetched_once_and_then_served_from_the_cache() {
    let server = Server::start(vec![("/a.png", image(png()))]);
    let dir = tempfile::tempdir().expect("temp");
    let f = fetcher(&dir);
    let got = f
        .fetch(&server.url("/a.png"), &options(), &any, &CancelFlag::new())
        .expect("fetched");
    assert_eq!(got.format, ImageFormat::Png);
    assert!(!got.from_cache);
    assert_eq!(std::fs::read(&got.path).expect("cached file"), png());
    // Polite and anonymous: our user agent, nothing else about the user.
    let seen = server.requests();
    assert_eq!(seen.len(), 1);
    let h = &seen[0].headers;
    assert!(h["user-agent"].starts_with("Glyphvane/"), "{h:?}");
    for private in ["cookie", "referer", "authorization"] {
        assert!(!h.contains_key(private), "{private} sent: {h:?}");
    }
    // Fresh in the cache: the site is not asked again.
    let again = f
        .fetch(&server.url("/a.png"), &options(), &any, &CancelFlag::new())
        .expect("cached");
    assert!(again.from_cache);
    assert_eq!(server.requests().len(), 1);
    assert!(f.cached(&server.url("/a.png")).is_some());
}

#[test]
fn credentials_in_the_url_are_never_sent() {
    let server = Server::start(vec![("/a.png", image(png()))]);
    let dir = tempfile::tempdir().expect("temp");
    let url = server.url("/a.png").replace("http://", "http://me:secret@");
    fetcher(&dir)
        .fetch(&url, &options(), &any, &CancelFlag::new())
        .expect("fetched");
    let h = &server.requests()[0].headers;
    assert!(!h.contains_key("authorization"), "{h:?}");
}

#[test]
fn redirects_are_followed_up_to_the_cap() {
    let server = Server::start(vec![
        ("/r1", Reply::Redirect("/r2".into())),
        ("/r2", Reply::Redirect("/a.png".into())),
        ("/a.png", image(png())),
        ("/loop", Reply::Redirect("/loop".into())),
    ]);
    let dir = tempfile::tempdir().expect("temp");
    let f = fetcher(&dir);
    let got = f
        .fetch(&server.url("/r1"), &options(), &any, &CancelFlag::new())
        .expect("two redirects are fine");
    assert_eq!(got.format, ImageFormat::Png);
    let capped = FetchOptions {
        max_redirects: 3,
        ..options()
    };
    let err = f
        .fetch(&server.url("/loop"), &capped, &any, &CancelFlag::new())
        .expect_err("a redirect loop");
    assert_eq!(err, FetchError::TooManyRedirects);
    // The first request and three redirects, nothing more.
    let loops = server
        .requests()
        .iter()
        .filter(|s| s.path == "/loop")
        .count();
    assert_eq!(loops, 4);
}

#[test]
fn the_policy_sees_every_redirect_before_it_is_followed() {
    let server = Server::start(vec![
        ("/r", Reply::Redirect("/blocked.png".into())),
        ("/blocked.png", image(png())),
    ]);
    let dir = tempfile::tempdir().expect("temp");
    let refuse = |u: &url::Url| !u.path().contains("blocked");
    let err = fetcher(&dir)
        .fetch(&server.url("/r"), &options(), &refuse, &CancelFlag::new())
        .expect_err("refused");
    assert_eq!(err, FetchError::Refused);
    assert!(server.requests().iter().all(|s| s.path != "/blocked.png"));
}

#[test]
fn bodies_over_the_cap_are_refused() {
    let mut big = png();
    big.resize(64 * 1024, 0);
    let server = Server::start(vec![("/big.png", image(big))]);
    let dir = tempfile::tempdir().expect("temp");
    let small = FetchOptions {
        max_bytes: 16 * 1024,
        ..options()
    };
    let err = fetcher(&dir)
        .fetch(&server.url("/big.png"), &small, &any, &CancelFlag::new())
        .expect_err("too large");
    assert!(
        matches!(err, FetchError::TooLarge { max: 16384 }),
        "{err:?}"
    );
    assert!(
        !dir.path().join("remote").exists() || {
            std::fs::read_dir(dir.path().join("remote"))
                .expect("dir")
                .next()
                .is_none()
        }
    );
}

#[test]
fn a_slow_site_times_out() {
    let server = Server::start(vec![(
        "/slow.png",
        Reply::Slow(Duration::from_secs(3), Box::new(image(png()))),
    )]);
    let dir = tempfile::tempdir().expect("temp");
    let quick = FetchOptions {
        total_timeout: Duration::from_millis(300),
        ..options()
    };
    let start = Instant::now();
    let err = fetcher(&dir)
        .fetch(&server.url("/slow.png"), &quick, &any, &CancelFlag::new())
        .expect_err("timed out");
    assert_eq!(err, FetchError::Timeout);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn html_posing_as_an_image_is_refused() {
    let page = b"<!doctype html><html><body>not an image</body></html>".to_vec();
    let server = Server::start(vec![(
        "/fake.png",
        Reply::Ok {
            content_type: "image/png",
            body: page,
            extra: Vec::new(),
        },
    )]);
    let dir = tempfile::tempdir().expect("temp");
    let err = fetcher(&dir)
        .fetch(
            &server.url("/fake.png"),
            &options(),
            &any,
            &CancelFlag::new(),
        )
        .expect_err("not an image");
    assert_eq!(err, FetchError::NotAnImage(Some("image/png".into())));
}

#[test]
fn http_errors_are_reported() {
    let server = Server::start(vec![("/gone.png", Reply::Status(410))]);
    let dir = tempfile::tempdir().expect("temp");
    let err = fetcher(&dir)
        .fetch(
            &server.url("/gone.png"),
            &options(),
            &any,
            &CancelFlag::new(),
        )
        .expect_err("gone");
    assert_eq!(err, FetchError::Status(410));
}

#[test]
fn loopback_is_not_contacted_unless_allowed() {
    let server = Server::start(vec![("/a.png", image(png()))]);
    let dir = tempfile::tempdir().expect("temp");
    // `localhost` resolves to loopback: the guard refuses before
    // connecting.
    let url = format!("http://localhost:{}/a.png", server.addr.port());
    let err = fetcher(&dir)
        .fetch(&url, &FetchOptions::default(), &any, &CancelFlag::new())
        .expect_err("private");
    assert_eq!(err, FetchError::PrivateAddress);
    assert!(server.requests().is_empty());
}

#[test]
fn no_store_responses_are_not_reused() {
    let server = Server::start(vec![(
        "/n.png",
        Reply::Ok {
            content_type: "image/png",
            body: png(),
            extra: vec![("cache-control", "no-store".into())],
        },
    )]);
    let dir = tempfile::tempdir().expect("temp");
    let f = fetcher(&dir);
    for _ in 0..2 {
        let got = f
            .fetch(&server.url("/n.png"), &options(), &any, &CancelFlag::new())
            .expect("fetched");
        assert!(!got.from_cache);
    }
    assert_eq!(server.requests().len(), 2);
    assert!(f.cached(&server.url("/n.png")).is_none());
}

#[test]
fn a_cancelled_fetch_contacts_nobody() {
    let server = Server::start(vec![("/a.png", image(png()))]);
    let dir = tempfile::tempdir().expect("temp");
    let cancel = CancelFlag::new();
    cancel.cancel();
    let err = fetcher(&dir)
        .fetch(&server.url("/a.png"), &options(), &any, &cancel)
        .expect_err("cancelled");
    assert_eq!(err, FetchError::Cancelled);
    assert!(server.requests().is_empty());
}

#[test]
fn the_cache_stays_under_its_cap() {
    let mut big = png();
    big.resize(300 * 1024, 0);
    let routes: Vec<(&'static str, Reply)> = ["/1.png", "/2.png", "/3.png", "/4.png", "/5.png"]
        .into_iter()
        .map(|p| (p, image(big.clone())))
        .collect();
    let server = Server::start(routes);
    let dir = tempfile::tempdir().expect("temp");
    let f = fetcher(&dir);
    for p in ["/1.png", "/2.png", "/3.png", "/4.png", "/5.png"] {
        f.fetch(&server.url(p), &options(), &any, &CancelFlag::new())
            .expect("fetched");
        std::thread::sleep(Duration::from_millis(20));
    }
    let total: u64 = std::fs::read_dir(dir.path().join("remote"))
        .expect("dir")
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();
    assert!(total <= 1 << 20, "{total}");
    // The most recent stays.
    assert!(f.cached(&server.url("/5.png")).is_some());
}
