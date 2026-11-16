// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Remote images: a blocking HTTP fetcher with a disk cache.
//!
//! Whether a URL may be fetched at all is the caller's policy (the
//! remote-content setting); this module only fetches, carefully:
//!
//! - ureq over rustls (`ring`), certificates verified through the OS
//!   store; the proxy from the environment (`HTTPS_PROXY`, `NO_PROXY`…)
//!   unless turned off;
//! - no cookies, no credentials (user info in a URL is dropped), no
//!   `Referer`, and a user agent without personal details
//!   ([`user_agent`]);
//! - caps: a connect and a total timeout, a body size (counted after
//!   decompression), a number of redirects; an HTTPS page never redirects
//!   to plain HTTP, and the caller's policy sees each hop;
//! - the private-address guard: unless the caller allows it, a host name
//!   that resolves to a loopback, private, link-local or unique-local
//!   address is not contacted, so a document cannot reach into the LAN
//!   through DNS;
//! - the body is sniffed ([`ImageFormat::sniff`]) as it arrives: anything
//!   that is not an image (an HTML page posing as one) stops the download;
//! - a [`CancelFlag`] is checked between reads.
//!
//! The cache keeps one file per URL in its folder, with a size cap (least
//! recently used files go first) and the expiry the response's
//! `Cache-Control` gives (`no-store` is never written; no header: one day;
//! at most 30 days). An expired file is fetched again, and still used when
//! the site cannot be reached.
//!
//! Nothing here logs a URL or a host: document addresses can be personal.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ureq::Agent;
use ureq::config::Config;
use ureq::http::Uri;
use ureq::tls::{RootCerts, TlsConfig};
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};
pub use url::Url;

use crate::format::ImageFormat;
use crate::types::CancelFlag;

/// The user agent sent with every request: the application and its
/// version, nothing about the user or the system.
pub fn user_agent() -> String {
    format!("Glyphvane/{}", env!("CARGO_PKG_VERSION"))
}

/// Caps of one fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOptions {
    /// The largest body accepted, in bytes (after decompression).
    pub max_bytes: u64,
    /// Time to connect (and resolve).
    pub connect_timeout: Duration,
    /// Time for the whole fetch, redirects included.
    pub total_timeout: Duration,
    /// Redirects followed at most.
    pub max_redirects: u32,
    /// Hosts that resolve to private or loopback addresses may be
    /// contacted (the user approved such a host).
    pub allow_private: bool,
}

impl Default for FetchOptions {
    /// Images: 20 MB, 10 s to connect, 30 s in all, 5 redirects, no
    /// private addresses.
    fn default() -> Self {
        Self {
            max_bytes: 20 * 1024 * 1024,
            connect_timeout: Duration::from_secs(10),
            total_timeout: Duration::from_secs(30),
            max_redirects: 5,
            allow_private: false,
        }
    }
}

/// Why a fetch gave no image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// Not an `http` or `https` URL, or not one that parses.
    InvalidUrl,
    /// The host resolves to a private or loopback address and such
    /// addresses were not allowed.
    PrivateAddress,
    /// A redirect went where the policy does not allow (another site that
    /// is blocked, or from HTTPS to HTTP).
    Refused,
    /// More redirects than allowed.
    TooManyRedirects,
    /// The site answered with this HTTP status.
    Status(u16),
    /// The body is over the cap.
    TooLarge { max: u64 },
    /// The site did not answer in time.
    Timeout,
    /// The body is not an image (by its first bytes); the declared
    /// content type, if any.
    NotAnImage(Option<String>),
    /// Anything else on the way (DNS, connection, TLS, the cache folder).
    Network(String),
    /// Cancelled through the [`CancelFlag`].
    Cancelled,
}

impl FetchError {
    /// One line for the image card (English-only UI in 1.0).
    pub fn card_message(&self) -> String {
        match self {
            FetchError::InvalidUrl => "The image address is not valid.".into(),
            FetchError::PrivateAddress => {
                "The site is on a local network address; it was not contacted.".into()
            }
            FetchError::Refused => "The image redirected to an address that is not allowed.".into(),
            FetchError::TooManyRedirects => "The image redirected too many times.".into(),
            FetchError::Status(code) => format!("The site answered with HTTP {code}."),
            FetchError::TooLarge { max } => {
                format!("The image is larger than {} MB.", max / (1024 * 1024))
            }
            FetchError::Timeout => "The site did not answer in time.".into(),
            FetchError::NotAnImage(_) => "The site did not send an image.".into(),
            FetchError::Network(m) => format!("The image could not be downloaded: {m}"),
            FetchError::Cancelled => "Loading was cancelled.".into(),
        }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.card_message())
    }
}

impl std::error::Error for FetchError {}

/// A fetched image: its file in the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub path: PathBuf,
    pub format: ImageFormat,
    pub bytes: u64,
    /// Served from the cache without contacting the site.
    pub from_cache: bool,
}

/// How the fetcher finds a proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyMode {
    /// `ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY` and `NO_PROXY` (either
    /// case), as most command-line tools do.
    Environment,
    /// Always connect directly.
    Direct,
}

/// Where and how much the fetcher caches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetcherConfig {
    pub cache_dir: PathBuf,
    /// Bytes of cached files kept at most.
    pub cache_max_bytes: u64,
    pub proxy: ProxyMode,
}

impl FetcherConfig {
    /// A 256 MB cache in `cache_dir`, the proxy from the environment.
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            cache_max_bytes: 256 * 1024 * 1024,
            proxy: ProxyMode::Environment,
        }
    }
}

/// Fetches remote images into its cache. Share one per process (its
/// connections and TLS set-up are reused); every call blocks, so call it
/// from a background thread only.
#[derive(Debug)]
pub struct RemoteFetcher {
    config: FetcherConfig,
    /// Agents without and with private addresses, made on first use.
    agents: [OnceLock<Agent>; 2],
    /// Serializes cache writes and eviction.
    cache_lock: Mutex<()>,
}

/// Default expiry without `Cache-Control`.
const DEFAULT_FRESH: u64 = 24 * 60 * 60;
/// Longest expiry, whatever the site says.
const MAX_FRESH: u64 = 30 * 24 * 60 * 60;
/// Bytes read before the body is sniffed.
const SNIFF_BYTES: usize = 4096;

impl RemoteFetcher {
    pub fn new(config: FetcherConfig) -> Self {
        Self {
            config,
            agents: [OnceLock::new(), OnceLock::new()],
            cache_lock: Mutex::new(()),
        }
    }

    pub fn config(&self) -> &FetcherConfig {
        &self.config
    }

    fn agent(&self, allow_private: bool) -> &Agent {
        self.agents[usize::from(allow_private)].get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let tls = TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build();
            let proxy = match self.config.proxy {
                ProxyMode::Environment => ureq::Proxy::try_from_env(),
                ProxyMode::Direct => None,
            };
            let proxy_host = proxy.as_ref().map(|p| p.host().to_ascii_lowercase());
            let config = Config::builder()
                .tls_config(tls)
                .proxy(proxy)
                .http_status_as_error(false)
                .max_redirects(0)
                .max_redirects_will_error(false)
                .save_redirect_history(false)
                .user_agent(user_agent())
                .accept("image/*")
                .build();
            Agent::with_parts(
                config,
                DefaultConnector::default(),
                GuardedResolver {
                    allow_private,
                    proxy_host,
                },
            )
        })
    }

    /// The cached image for `url` while it is fresh (no network).
    pub fn cached(&self, url: &str) -> Option<Fetched> {
        let url = clean_url(url).ok()?;
        let entry = self.read_entry(&url)?;
        (entry.expires > now()).then(|| entry.fetched(true))
    }

    /// Fetches the image at `url` (blocking), or takes it from the cache
    /// while it is fresh. `allow_hop` sees every redirect's target before
    /// it is contacted (the caller's site lists).
    pub fn fetch(
        &self,
        url: &str,
        options: &FetchOptions,
        allow_hop: &dyn Fn(&Url) -> bool,
        cancel: &CancelFlag,
    ) -> Result<Fetched, FetchError> {
        let url = clean_url(url)?;
        let stale = match self.read_entry(&url) {
            Some(e) if e.expires > now() => return Ok(e.fetched(true)),
            other => other,
        };
        match self.download(&url, options, allow_hop, cancel) {
            Ok((body, format, fresh_for)) => self.store(&url, &body, format, fresh_for),
            // The site cannot be reached: an expired copy still serves.
            Err(FetchError::Network(_) | FetchError::Timeout) if stale.is_some() => {
                Ok(stale.map(|e| e.fetched(true)).expect("checked"))
            }
            Err(e) => Err(e),
        }
    }

    /// Downloads `url`, following redirects: the body, its format and how
    /// long it stays fresh (`None`: do not store).
    fn download(
        &self,
        start: &Url,
        options: &FetchOptions,
        allow_hop: &dyn Fn(&Url) -> bool,
        cancel: &CancelFlag,
    ) -> Result<(Vec<u8>, ImageFormat, Option<u64>), FetchError> {
        let agent = self.agent(options.allow_private);
        let deadline = std::time::Instant::now() + options.total_timeout;
        let mut url = start.clone();
        let mut hops = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(FetchError::Cancelled);
            }
            let left = deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or(FetchError::Timeout)?;
            let response = agent
                .get(url.as_str())
                .config()
                .timeout_global(Some(left))
                .timeout_connect(Some(options.connect_timeout.min(left)))
                .build()
                .call()
                .map_err(map_error)?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) && status != 304 {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(FetchError::Status(status))?;
                let next = url.join(location).map_err(|_| FetchError::InvalidUrl)?;
                let next = clean_url(next.as_str())?;
                if url.scheme() == "https" && next.scheme() != "https" {
                    return Err(FetchError::Refused);
                }
                hops += 1;
                if hops > options.max_redirects {
                    return Err(FetchError::TooManyRedirects);
                }
                if !allow_hop(&next) {
                    return Err(FetchError::Refused);
                }
                url = next;
                continue;
            }
            if !(200..300).contains(&status) {
                return Err(FetchError::Status(status));
            }
            let declared = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let fresh_for = freshness(
                response
                    .headers()
                    .get("cache-control")
                    .and_then(|v| v.to_str().ok()),
            );
            if let Some(len) = response
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                && len > options.max_bytes
            {
                return Err(FetchError::TooLarge {
                    max: options.max_bytes,
                });
            }
            let mut reader = response
                .into_body()
                .into_with_config()
                .limit(options.max_bytes.saturating_add(1))
                .reader();
            let mut body = Vec::new();
            let mut format = None;
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                if cancel.is_cancelled() {
                    return Err(FetchError::Cancelled);
                }
                let n = reader.read(&mut chunk).map_err(map_io)?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
                if body.len() as u64 > options.max_bytes {
                    return Err(FetchError::TooLarge {
                        max: options.max_bytes,
                    });
                }
                if format.is_none() && body.len() >= SNIFF_BYTES {
                    format = Some(
                        ImageFormat::sniff(&body)
                            .ok_or_else(|| FetchError::NotAnImage(declared.clone()))?,
                    );
                }
            }
            let format = match format {
                Some(f) => f,
                None => ImageFormat::sniff(&body).ok_or(FetchError::NotAnImage(declared))?,
            };
            return Ok((body, format, fresh_for));
        }
    }

    // ---- the cache ----------------------------------------------------

    fn names(&self, url: &Url) -> (PathBuf, PathBuf) {
        let key = cache_key(url.as_str());
        (
            self.config.cache_dir.join(format!("{key}.img")),
            self.config.cache_dir.join(format!("{key}.meta")),
        )
    }

    fn read_entry(&self, url: &Url) -> Option<Entry> {
        let (body, meta) = self.names(url);
        let text = fs::read_to_string(&meta).ok()?;
        let mut lines = text.lines();
        // The URL is kept to tell keys that collide apart.
        if lines.next()? != url.as_str() {
            return None;
        }
        let expires: u64 = lines.next()?.parse().ok()?;
        let bytes = fs::metadata(&body).ok()?.len();
        let mut head = vec![0u8; SNIFF_BYTES];
        let n = fs::File::open(&body)
            .and_then(|mut f| f.read(&mut head))
            .ok()?;
        let format = ImageFormat::sniff(&head[..n])?;
        // Recently used: evicted last.
        let _ = fs::File::options()
            .append(true)
            .open(&body)
            .and_then(|f| f.set_modified(SystemTime::now()));
        Some(Entry {
            path: body,
            format,
            bytes,
            expires,
        })
    }

    fn store(
        &self,
        url: &Url,
        body: &[u8],
        format: ImageFormat,
        fresh_for: Option<u64>,
    ) -> Result<Fetched, FetchError> {
        let (path, meta) = self.names(url);
        let _guard = self
            .cache_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let io = |e: io::Error| FetchError::Network(format!("cache: {e}"));
        fs::create_dir_all(&self.config.cache_dir).map_err(io)?;
        // `no-store`: a file of its own that is never looked up again (it
        // goes with the least recently used ones).
        let (path, meta) = match fresh_for {
            Some(_) => (path, Some(meta)),
            None => (
                self.config.cache_dir.join(format!(
                    "{}-{}.nostore",
                    cache_key(url.as_str()),
                    unique()
                )),
                None,
            ),
        };
        write_atomic(&path, body).map_err(io)?;
        if let (Some(meta), Some(fresh)) = (meta, fresh_for) {
            let text = format!("{}\n{}\n", url.as_str(), now() + fresh);
            write_atomic(&meta, text.as_bytes()).map_err(io)?;
        }
        self.evict();
        Ok(Fetched {
            path,
            format,
            bytes: body.len() as u64,
            from_cache: false,
        })
    }

    /// Over the cap: the least recently used files go until a quarter is
    /// free.
    fn evict(&self) {
        let Ok(dir) = fs::read_dir(&self.config.cache_dir) else {
            return;
        };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = dir
            .filter_map(Result::ok)
            .filter_map(|e| {
                let path = e.path();
                let ext = path.extension()?.to_str()?;
                if ext != "img" && ext != "nostore" {
                    return None;
                }
                let m = e.metadata().ok()?;
                Some((m.modified().unwrap_or(UNIX_EPOCH), m.len(), path))
            })
            .collect();
        let total: u64 = files.iter().map(|f| f.1).sum();
        if total <= self.config.cache_max_bytes {
            return;
        }
        files.sort_by_key(|f| f.0);
        let target = self.config.cache_max_bytes / 4 * 3;
        let mut total = total;
        for (_, len, path) in files {
            if total <= target {
                break;
            }
            let _ = fs::remove_file(path.with_extension("meta"));
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
    }

    /// Removes every cached file.
    pub fn clear_cache(&self) -> io::Result<()> {
        let _guard = self
            .cache_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match fs::remove_dir_all(&self.config.cache_dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

struct Entry {
    path: PathBuf,
    format: ImageFormat,
    bytes: u64,
    expires: u64,
}

impl Entry {
    fn fetched(self, from_cache: bool) -> Fetched {
        Fetched {
            path: self.path,
            format: self.format,
            bytes: self.bytes,
            from_cache,
        }
    }
}

/// The host of an `http` or `https` URL, lower-case (IPv6 in brackets);
/// `None` for anything else.
pub fn host_of(url: &str) -> Option<String> {
    clean_url(url).ok()?.host_str().map(str::to_ascii_lowercase)
}

/// `url` parsed: `http` or `https` with a host, without user info or
/// fragment.
fn clean_url(url: &str) -> Result<Url, FetchError> {
    let mut url = Url::parse(url.trim()).map_err(|_| FetchError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err(FetchError::InvalidUrl);
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    Ok(url)
}

/// Seconds a response stays fresh by its `Cache-Control`; `None` for
/// `no-store`.
fn freshness(cache_control: Option<&str>) -> Option<u64> {
    let Some(cc) = cache_control else {
        return Some(DEFAULT_FRESH);
    };
    let mut fresh = DEFAULT_FRESH;
    for directive in cc.split(',') {
        let d = directive.trim().to_ascii_lowercase();
        if d == "no-store" {
            return None;
        }
        if d == "no-cache" {
            fresh = 0;
        } else if let Some(v) = d.strip_prefix("max-age=") {
            fresh = v.trim_matches('"').parse().unwrap_or(0);
        }
    }
    Some(fresh.min(MAX_FRESH))
}

/// The cache file name of a URL: two 64-bit FNV-1a hashes.
fn cache_key(url: &str) -> String {
    let hash = |mut h: u64| {
        for b in url.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    };
    format!(
        "{:016x}{:016x}",
        hash(0xcbf2_9ce4_8422_2325),
        hash(0x6c62_272e_07bb_0142)
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Writes `bytes` to a temporary file next to `path`, then renames it.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", unique()));
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Marks the resolver's refusal inside a `ureq::Error::Io`.
#[derive(Debug)]
struct PrivateAddressRefused;

impl std::fmt::Display for PrivateAddressRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("private address refused")
    }
}

impl std::error::Error for PrivateAddressRefused {}

fn map_error(e: ureq::Error) -> FetchError {
    match e {
        ureq::Error::Timeout(_) => FetchError::Timeout,
        ureq::Error::TooManyRedirects => FetchError::TooManyRedirects,
        ureq::Error::BodyExceedsLimit(max) => FetchError::TooLarge {
            max: max.saturating_sub(1),
        },
        ureq::Error::StatusCode(code) => FetchError::Status(code),
        ureq::Error::BadUri(_) => FetchError::InvalidUrl,
        ureq::Error::Io(io) => map_io(io),
        other => FetchError::Network(other.to_string()),
    }
}

fn map_io(e: io::Error) -> FetchError {
    if e.get_ref()
        .is_some_and(|i| i.downcast_ref::<PrivateAddressRefused>().is_some())
    {
        return FetchError::PrivateAddress;
    }
    if let Some(inner) = e.get_ref().and_then(|i| i.downcast_ref::<ureq::Error>()) {
        return match inner {
            ureq::Error::Timeout(_) => FetchError::Timeout,
            ureq::Error::BodyExceedsLimit(max) => FetchError::TooLarge {
                max: max.saturating_sub(1),
            },
            other => FetchError::Network(other.to_string()),
        };
    }
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => FetchError::Timeout,
        _ => FetchError::Network(e.to_string()),
    }
}

/// Whether `ip` is on this machine or a local network: loopback, private,
/// link-local, unique-local, carrier-grade NAT, unspecified, broadcast or
/// multicast (IPv4 inside IPv6 counts as its IPv4 address).
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_v4(v4);
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg0 & 0xfe00) == 0xfc00
                || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

fn is_private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
}

/// Whether `host` (a URL's host) names this machine or a local network by
/// itself: `localhost` and its subdomains, `.local` and other single-label
/// names, and private IP literals. Names that only resolve to private
/// addresses are caught when they are resolved.
pub fn is_private_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return is_private_ip(ip);
    }
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".lan")
        || host.ends_with(".home.arpa")
        || !host.contains('.')
}

/// The default resolver, keeping only public addresses unless private
/// ones are allowed. The proxy's own host is always resolved as is.
#[derive(Debug)]
struct GuardedResolver {
    allow_private: bool,
    proxy_host: Option<String>,
}

impl Resolver for GuardedResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let all = DefaultResolver::default().resolve(uri, config, timeout)?;
        let host = uri.host().map(str::to_ascii_lowercase);
        if self.allow_private || (host.is_some() && host == self.proxy_host) {
            return Ok(all);
        }
        let mut public = self.empty();
        for addr in all.iter().filter(|a| !is_private_ip(a.ip())) {
            public.push(*addr);
        }
        if public.is_empty() {
            return Err(ureq::Error::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                PrivateAddressRefused,
            )));
        }
        Ok(public)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_control_sets_the_expiry() {
        assert_eq!(freshness(None), Some(DEFAULT_FRESH));
        assert_eq!(freshness(Some("max-age=60")), Some(60));
        assert_eq!(freshness(Some("public, max-age=\"120\"")), Some(120));
        assert_eq!(freshness(Some("no-cache")), Some(0));
        assert_eq!(freshness(Some("max-age=60, no-store")), None);
        assert_eq!(freshness(Some("max-age=999999999")), Some(MAX_FRESH));
    }

    #[test]
    fn urls_lose_credentials_and_fragments() {
        let u = clean_url("https://me:secret@example.com/a.png#x").expect("valid");
        assert_eq!(u.as_str(), "https://example.com/a.png");
        assert_eq!(clean_url("file:///etc/passwd"), Err(FetchError::InvalidUrl));
        assert_eq!(
            clean_url("ftp://example.com/a"),
            Err(FetchError::InvalidUrl)
        );
        assert_eq!(clean_url("not a url"), Err(FetchError::InvalidUrl));
    }

    #[test]
    fn private_hosts_and_addresses() {
        for h in [
            "localhost",
            "LOCALHOST.",
            "a.localhost",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "[::1]",
            "[fe80::1]",
            "[fd00::1]",
            "[::ffff:192.168.0.1]",
            "router",
            "printer.local",
        ] {
            assert!(is_private_host(h), "{h}");
        }
        for h in ["example.com", "8.8.8.8", "[2001:db8::1]", "img.shields.io"] {
            assert!(!is_private_host(h), "{h}");
        }
    }

    #[test]
    fn the_user_agent_names_only_the_app() {
        let ua = user_agent();
        assert!(ua.starts_with("Glyphvane/"), "{ua}");
        assert!(!ua.contains(' '), "{ua}");
    }
}
