// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Remote images in the Reading view: the remote-content setting
//! (FR-SET-08) and the fetches it allows.
//!
//! - **The policy** ([`Policy::decide`]), per site, from the settings in
//!   effect: Block (also in a restricted vault and in safe mode) never
//!   fetches and offers nothing; a site on a block list never loads; Allow
//!   loads, except private and loopback hosts, which ask; Ask loads the
//!   sites on an allow list and those approved for this note this
//!   session, and waits for the rest. A global Block is absolute and
//!   in-vault files only tighten: both are the settings layers' rules.
//! - **Placeholders**: a remote image that is not shown is a card in its
//!   box that says why (waiting, blocked, loading, failed), with its alt
//!   text and site.
//! - **The infobar** ([`Reader::remote_notice`]): the sites waiting, with
//!   *Load once* (this note, this session), *Always allow* (the allow
//!   list), *Allow for this vault* (the vault's mode) and *Block* (the
//!   block list). Lists and modes are written to the vault's overlay in
//!   the app data dir, or the global file without a vault: never into
//!   the vault.
//! - **Fetches** run on the background executor through one
//!   [`RemoteFetcher`] (caps, disk cache, no cookies); each is cancelled
//!   when the policy no longer allows it or another file is shown. A
//!   fetched image is drawn through the same path as a local one, decoded
//!   by an engine that gives SVGs no file access.
//!
//! Only the Reading view fetches: the Source view and exports never do.
//! Nothing is logged about the sites or URLs.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use gpui::{App, Context, Global, Task};
use gv_media::remote::{FetchError, FetchOptions, FetcherConfig, ProxyMode, RemoteFetcher};
use gv_media::{CancelFlag, MediaError, Probe};
use gv_render_tree::{LinkTarget, Sources};
use gv_settings::{RemoteContent, WorkspaceTrust};

use super::Reader;
use super::links::Resolver;
use super::media::{ImageSizes, ImageTarget};
use crate::files::notice::{Notice, NoticeAction};
use crate::settings::{AppSettings, Target};

const ALLOW_KEY: &str = "privacy_and_security.remote_allow_sites";
const BLOCK_KEY: &str = "privacy_and_security.remote_block_sites";
const MODE_KEY: &str = "privacy_and_security.remote_content";

/// A site entry as written in a list: `example.com`, `*.example.com`,
/// `https://example.com/path` or `example.com:8080` all name
/// `example.com`.
pub(crate) fn site_of(entry: &str) -> Option<String> {
    let mut s = entry.trim().to_ascii_lowercase();
    for scheme in ["https://", "http://"] {
        if let Some(rest) = s.strip_prefix(scheme) {
            s = rest.to_owned();
        }
    }
    let s = s.split(['/', '?', '#']).next().unwrap_or("");
    let s = s.rsplit_once('@').map_or(s, |(_, h)| h);
    let host = if s.starts_with('[') {
        // An IPv6 literal, with or without a port.
        s.split_once(']').map(|(h, _)| format!("{h}]"))?
    } else {
        s.split(':').next().unwrap_or("").to_owned()
    };
    let host = host
        .trim_start_matches("*.")
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_owned();
    (!host.is_empty()).then_some(host)
}

/// Whether the list entry `entry` covers `host`: the site itself and its
/// subdomains (`example.com` covers `img.example.com`).
fn covers(entry: &str, host: &str) -> bool {
    site_of(entry).is_some_and(|site| {
        host == site || (host.ends_with(&site) && host[..host.len() - site.len()].ends_with('.'))
    })
}

/// What the policy says about one site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Fetch. `private_ok`: the site was named by the user (allow list or
    /// an approval), so it may be on a private address.
    Load { private_ok: bool },
    /// Wait for approval.
    Ask,
    /// Never fetch: the mode is Block (`by_setting`), or the site is on a
    /// block list.
    Blocked { by_setting: bool },
}

/// The remote-content settings in effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Policy {
    pub mode: RemoteContent,
    pub allow: Vec<String>,
    pub block: Vec<String>,
}

impl Policy {
    /// The policy of `settings`: a restricted vault is Block.
    pub fn of(settings: &gv_settings::Settings) -> Self {
        let p = &settings.privacy_and_security;
        let mode = if p.workspace_trust == WorkspaceTrust::Restricted {
            RemoteContent::Block
        } else {
            p.remote_content
        };
        Self {
            mode,
            allow: p.remote_allow_sites.clone(),
            block: p.remote_block_sites.clone(),
        }
    }

    /// The policy in effect in the app (Block without settings).
    pub fn of_app(cx: &App) -> Self {
        cx.try_global::<AppSettings>()
            .map_or_else(Self::blocking, |s| Self::of(s.settings()))
    }

    fn blocking() -> Self {
        Self {
            mode: RemoteContent::Block,
            allow: Vec::new(),
            block: Vec::new(),
        }
    }

    pub fn is_blocked_site(&self, host: &str) -> bool {
        self.block.iter().any(|e| covers(e, host))
    }

    /// What to do with images from `host`; `approved`: the user chose
    /// *Load once* for it in this note.
    pub fn decide(&self, host: &str, approved: bool) -> Decision {
        if self.mode == RemoteContent::Block {
            return Decision::Blocked { by_setting: true };
        }
        if self.is_blocked_site(host) {
            return Decision::Blocked { by_setting: false };
        }
        let named = approved || self.allow.iter().any(|e| covers(e, host));
        match self.mode {
            RemoteContent::Allow if named => Decision::Load { private_ok: true },
            RemoteContent::Allow if gv_media::remote::is_private_host(host) => Decision::Ask,
            RemoteContent::Allow => Decision::Load { private_ok: false },
            _ if named => Decision::Load { private_ok: true },
            _ => Decision::Ask,
        }
    }
}

/// What a remote image that is not shown says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteShown {
    Waiting { host: String },
    Blocked { host: String, by_setting: bool },
    Loading { host: String },
    Failed(String),
}

impl RemoteShown {
    /// Before the view has asked the policy.
    pub fn unknown(url: &str) -> Self {
        Self::Waiting {
            host: gv_media::remote::host_of(url).unwrap_or_default(),
        }
    }

    /// The card's line; `alt` is the image's description.
    pub fn card_message(&self, alt: &str) -> String {
        let alt = alt.trim();
        let with_alt = |s: String| {
            if alt.is_empty() {
                s
            } else {
                format!("{alt}: {s}")
            }
        };
        match self {
            Self::Waiting { host } => with_alt(format!("Remote image from {host}, not loaded.")),
            Self::Blocked {
                by_setting: true, ..
            } => with_alt("Remote content is blocked in your settings.".to_owned()),
            Self::Blocked { host, .. } => with_alt(format!("Images from {host} are blocked.")),
            Self::Loading { host } => with_alt(format!("Loading the image from {host}…")),
            Self::Failed(message) => with_alt(message.clone()),
        }
    }
}

/// A fetched image's cached file and header facts.
type CachedImage = (PathBuf, Result<Probe, MediaError>);

/// Remote images fetched in this run, by URL: a document built again shows
/// them at their size at once (the view applies the policy before
/// drawing).
fn registry() -> &'static Mutex<HashMap<String, CachedImage>> {
    static FETCHED: OnceLock<Mutex<HashMap<String, CachedImage>>> = OnceLock::new();
    FETCHED.get_or_init(Mutex::default)
}

/// The cached file of `url` and its header facts, if it was fetched in this
/// run and is still in the cache.
pub(crate) fn fetched(url: &str) -> Option<CachedImage> {
    let mut map = registry().lock().unwrap_or_else(PoisonError::into_inner);
    match map.get(url) {
        Some((path, probe)) if path.is_file() => Some((path.clone(), probe.clone())),
        Some(_) => {
            map.remove(url);
            None
        }
        None => None,
    }
}

/// The app's fetcher, for views other than the Reading view (Bases card
/// covers) that fetch what the policy allows without asking.
pub(crate) fn shared_fetcher(cx: &mut App) -> Arc<RemoteFetcher> {
    RemoteSession::fetcher(cx)
}

/// Fetches `url` as the Reading view does (blocking: the background
/// executor only): redirects may not reach a blocked site, nor a private
/// host unless `private_ok`. The image is remembered for this run (see
/// [`fetched`]). Returns the cached file.
pub(crate) fn fetch_image(
    fetcher: &RemoteFetcher,
    url: &str,
    private_ok: bool,
    policy: &Policy,
    cancel: &CancelFlag,
) -> Result<PathBuf, FetchError> {
    let options = FetchOptions {
        allow_private: private_ok,
        ..FetchOptions::default()
    };
    let hop = |u: &gv_media::remote::Url| {
        let host = u.host_str().unwrap_or("").to_ascii_lowercase();
        !policy.is_blocked_site(&host) && (private_ok || !gv_media::remote::is_private_host(&host))
    };
    let fetched = fetcher.fetch(url, &options, &hop, cancel)?;
    let probe = super::media::remote_engine().probe_path(&fetched.path);
    registry()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(url.to_owned(), (fetched.path.clone(), probe));
    Ok(fetched.path)
}

/// The app's remote-content state: the fetcher and this session's
/// approvals (never written anywhere).
#[derive(Default)]
pub struct RemoteSession {
    fetcher: Option<Arc<RemoteFetcher>>,
    /// Where the cache goes instead of the app's cache dir (tests).
    cache_dir: Option<PathBuf>,
    /// Connect directly whatever the environment says (tests).
    direct: bool,
    /// *Load once*: (note, site).
    approved: HashSet<(PathBuf, String)>,
    /// Fetches started, for tests.
    started: usize,
}

impl Global for RemoteSession {}

impl RemoteSession {
    /// Puts the cache in `dir` and connects without a proxy (tests).
    #[cfg(test)]
    pub(crate) fn use_cache_dir(cx: &mut App, dir: PathBuf) {
        let s = cx.default_global::<Self>();
        s.cache_dir = Some(dir);
        s.direct = true;
        s.fetcher = None;
    }

    /// Fetches started in this app.
    #[cfg(test)]
    pub(crate) fn started(cx: &App) -> usize {
        cx.try_global::<Self>().map_or(0, |s| s.started)
    }

    fn fetcher(cx: &mut App) -> Arc<RemoteFetcher> {
        let app_dir = cx
            .try_global::<AppSettings>()
            .and_then(AppSettings::locations)
            .map(|l| l.cache_dir().join("remote-images"));
        let s = cx.default_global::<Self>();
        if let Some(f) = &s.fetcher {
            return Arc::clone(f);
        }
        let dir = s
            .cache_dir
            .clone()
            .or(app_dir)
            .unwrap_or_else(|| std::env::temp_dir().join("glyphvane-remote-images"));
        let mut config = FetcherConfig::new(dir);
        if s.direct {
            config.proxy = ProxyMode::Direct;
        }
        let f = Arc::new(RemoteFetcher::new(config));
        s.fetcher = Some(Arc::clone(&f));
        f
    }

    fn approved(cx: &App, doc: &Path, host: &str) -> bool {
        cx.try_global::<Self>()
            .is_some_and(|s| s.approved.contains(&(doc.to_path_buf(), host.to_owned())))
    }
}

/// Adds `hosts` to the list `key` of the vault's overlay (or the global
/// file without a vault).
fn add_sites(cx: &mut App, key: &str, hosts: &[String]) {
    let Some(settings) = cx.try_global::<AppSettings>() else {
        return;
    };
    let target = if settings.vault().is_some() {
        Target::Vault
    } else {
        Target::AllVaults
    };
    let mut list: Vec<toml::Value> = settings
        .own_value(target, key)
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    for host in hosts {
        let value = toml::Value::String(host.clone());
        if !list.contains(&value) {
            list.push(value);
        }
    }
    if let Err(e) = AppSettings::set_value(cx, target, key, &toml::Value::Array(list)) {
        tracing::warn!("remote content: the site list was not changed: {e}");
    }
}

/// A finished fetch: the cached file and its header facts.
type FetchResult = Result<CachedImage, FetchError>;

/// The view's remote images.
#[derive(Default)]
pub(crate) struct RemoteImages {
    /// The note these are for.
    doc: Option<PathBuf>,
    /// Fetches running, by URL.
    running: HashMap<String, (CancelFlag, Task<()>)>,
    /// Fetches that failed for this note, by URL: the card's line.
    failed: HashMap<String, String>,
    /// Sites waiting for approval.
    waiting: Vec<String>,
    /// The infobar was dismissed for these sites.
    dismissed: Vec<String>,
}

impl std::fmt::Debug for RemoteImages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteImages")
            .field("running", &self.running.len())
            .field("waiting", &self.waiting.len())
            .finish_non_exhaustive()
    }
}

impl RemoteImages {
    fn cancel(&mut self, url: &str) {
        if let Some((cancel, _task)) = self.running.remove(url) {
            cancel.cancel();
        }
    }

    fn reset(&mut self, doc: Option<PathBuf>) {
        for (cancel, _) in self.running.values() {
            cancel.cancel();
        }
        *self = Self {
            doc,
            ..Self::default()
        };
    }
}

impl Reader {
    /// Applies the remote-content policy to the shown document's remote
    /// images: starts the fetches it allows, cancels and hides what it no
    /// longer allows, and updates the placeholders and the sites waiting.
    pub(crate) fn sync_remote(&mut self, cx: &mut Context<'_, Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if self.remote.doc.as_ref() != Some(&path) {
            self.remote.reset(Some(path.clone()));
        }
        let Some(targets) = self
            .state
            .borrow()
            .doc
            .as_ref()
            .map(|d| Arc::clone(&d.images))
        else {
            return;
        };
        let policy = Policy::of_app(cx);
        let mut shown = HashMap::new();
        let mut waiting = BTreeSet::new();
        let mut hidden: HashSet<String> = HashSet::new();
        let urls = targets.remote_urls();
        for (url, dests) in &urls {
            let Some(host) = gv_media::remote::host_of(url) else {
                shown.insert(
                    url.clone(),
                    RemoteShown::Failed(FetchError::InvalidUrl.card_message()),
                );
                continue;
            };
            let approved = RemoteSession::approved(cx, &path, &host);
            let decision = policy.decide(&host, approved);
            if !matches!(decision, Decision::Load { .. }) {
                self.remote.cancel(url);
                if targets.is_fetched(url) {
                    for d in dests {
                        targets.set(d, ImageTarget::Remote(url.clone()));
                        hidden.insert(d.clone());
                    }
                }
            }
            match decision {
                Decision::Load { private_ok } => {
                    if targets.is_fetched(url) {
                        continue;
                    }
                    if let Some(message) = self.remote.failed.get(url) {
                        shown.insert(url.clone(), RemoteShown::Failed(message.clone()));
                        continue;
                    }
                    if !self.remote.running.contains_key(url) {
                        self.start_fetch(url.clone(), private_ok, policy.clone(), cx);
                    }
                    shown.insert(url.clone(), RemoteShown::Loading { host });
                }
                Decision::Ask => {
                    waiting.insert(host.clone());
                    shown.insert(url.clone(), RemoteShown::Waiting { host });
                }
                Decision::Blocked { by_setting } => {
                    shown.insert(url.clone(), RemoteShown::Blocked { host, by_setting });
                }
            }
        }
        // Fetches for images no longer in the note stop.
        let gone: Vec<String> = self
            .remote
            .running
            .keys()
            .filter(|u| !urls.contains_key(*u))
            .cloned()
            .collect();
        for url in gone {
            self.remote.cancel(&url);
        }
        self.remote.waiting = waiting.into_iter().collect();
        if !hidden.is_empty() {
            self.refresh_images(&hidden);
        }
        let stale = self.state.borrow_mut().cache.set_remote(shown);
        if !stale.is_empty() {
            self.state.borrow_mut().stale.extend(stale);
        }
        cx.notify();
    }

    fn start_fetch(
        &mut self,
        url: String,
        private_ok: bool,
        policy: Policy,
        cx: &mut Context<'_, Self>,
    ) {
        let fetcher = RemoteSession::fetcher(cx);
        cx.default_global::<RemoteSession>().started += 1;
        let cancel = CancelFlag::new();
        let flag = cancel.clone();
        let job_url = url.clone();
        let key = url.clone();
        let task = cx.spawn(async move |this, cx| {
            let result: FetchResult = cx
                .background_executor()
                .spawn_with_priority(gpui::Priority::Low, async move {
                    let options = FetchOptions {
                        allow_private: private_ok,
                        ..FetchOptions::default()
                    };
                    // A redirect may not go to a blocked site, nor to a
                    // private host the user did not name.
                    let hop = |u: &gv_media::remote::Url| {
                        let host = u.host_str().unwrap_or("").to_ascii_lowercase();
                        !policy.is_blocked_site(&host)
                            && (private_ok || !gv_media::remote::is_private_host(&host))
                    };
                    let fetched = fetcher.fetch(&job_url, &options, &hop, &flag)?;
                    let probe = super::media::remote_engine().probe_path(&fetched.path);
                    Ok((fetched.path, probe))
                })
                .await;
            this.update(cx, |reader, cx| reader.fetch_landed(&url, result, cx))
                .ok();
        });
        self.remote.running.insert(key, (cancel, task));
    }

    /// A fetch ended: its image shows at its size, or its card says why.
    fn fetch_landed(&mut self, url: &str, result: FetchResult, cx: &mut Context<'_, Self>) {
        if self.remote.running.remove(url).is_none() {
            // Cancelled meanwhile.
            return;
        }
        match result {
            Ok((path, probe)) => {
                registry()
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(url.to_owned(), (path.clone(), probe.clone()));
                let Some(targets) = self
                    .state
                    .borrow()
                    .doc
                    .as_ref()
                    .map(|d| Arc::clone(&d.images))
                else {
                    return;
                };
                let dests: HashSet<String> = targets
                    .remote_urls()
                    .remove(url)
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                for d in &dests {
                    targets.set(
                        d,
                        ImageTarget::Fetched {
                            url: url.to_owned(),
                            path: path.clone(),
                            probe: probe.clone(),
                        },
                    );
                }
                if !self.refresh_images(&dests) {
                    // Not in a paragraph (a `<picture>`): built again; the
                    // image comes from the cache.
                    self.rebuild(cx);
                }
            }
            Err(FetchError::Cancelled) => {}
            Err(e) => {
                self.remote.failed.insert(url.to_owned(), e.card_message());
            }
        }
        self.sync_remote(cx);
    }

    /// Asks the render tree for the sizes of the images at `dests` again
    /// (their targets changed). Returns whether any item changed.
    fn refresh_images(&mut self, dests: &HashSet<String>) -> bool {
        let Some(path) = self.path.clone() else {
            return false;
        };
        let mut st = self.state.borrow_mut();
        let Some(doc) = st.doc.as_mut() else {
            return false;
        };
        let resolver = Resolver::statuses(&path, self.index.as_ref(), doc.profile);
        let sizes = ImageSizes {
            resolver: resolver.for_images(),
            targets: &doc.images,
        };
        let sources = Sources {
            links: &resolver,
            images: &sizes,
        };
        let which = |t: &LinkTarget<'_>| t.image && dests.contains(t.destination);
        let changed = Arc::make_mut(&mut doc.tree)
            .refresh_where(&doc.syntax, &*doc.text, sources, &which)
            .changed;
        if changed.is_empty() {
            return false;
        }
        st.cache.forget(changed.iter().copied());
        st.stale.extend(changed);
        true
    }

    /// The infobar for the sites whose images wait for approval, unless
    /// it was dismissed for them.
    pub fn remote_notice(&self, cx: &App) -> Option<Notice> {
        if self.remote.waiting.is_empty() || self.remote.waiting == self.remote.dismissed {
            return None;
        }
        let vault = cx
            .try_global::<AppSettings>()
            .is_some_and(|s| s.vault().is_some());
        Some(Notice::remote_content(&self.remote.waiting, vault))
    }

    /// The infobar's dismiss button: hidden until other sites wait.
    pub fn dismiss_remote(&mut self, cx: &mut Context<'_, Self>) {
        self.remote.dismissed = self.remote.waiting.clone();
        cx.notify();
    }

    /// An infobar button about remote content.
    pub fn answer_remote(&mut self, action: &NoticeAction, cx: &mut Context<'_, Self>) {
        let hosts = self.remote.waiting.clone();
        if hosts.is_empty() {
            return;
        }
        match action {
            NoticeAction::RemoteLoadOnce => {
                let Some(doc) = self.path.clone() else {
                    return;
                };
                let s = cx.default_global::<RemoteSession>();
                for host in &hosts {
                    s.approved.insert((doc.clone(), host.clone()));
                }
            }
            NoticeAction::RemoteAlwaysAllow => add_sites(cx, ALLOW_KEY, &hosts),
            NoticeAction::RemoteBlock => add_sites(cx, BLOCK_KEY, &hosts),
            NoticeAction::RemoteAllowVault => {
                let value = toml::Value::String("allow".to_owned());
                if let Err(e) = AppSettings::set_value(cx, Target::Vault, MODE_KEY, &value) {
                    tracing::warn!("remote content: the vault's mode was not changed: {e}");
                }
            }
            _ => return,
        }
        // Settings edits reach every view through its settings observer;
        // approvals only concern this one.
        self.sync_remote(cx);
    }

    /// Sites waiting for approval (tests).
    #[cfg(test)]
    pub(crate) fn remote_waiting(&self) -> &[String] {
        &self.remote.waiting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: RemoteContent, allow: &[&str], block: &[&str]) -> Policy {
        let list = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect();
        Policy {
            mode,
            allow: list(allow),
            block: list(block),
        }
    }

    #[test]
    fn site_entries_name_hosts() {
        assert_eq!(site_of("Example.com").as_deref(), Some("example.com"));
        assert_eq!(site_of("*.example.com").as_deref(), Some("example.com"));
        assert_eq!(
            site_of("https://img.example.com:8443/a/b.png").as_deref(),
            Some("img.example.com")
        );
        assert_eq!(site_of("[::1]:8080").as_deref(), Some("[::1]"));
        assert_eq!(site_of("  "), None);
        assert!(covers("example.com", "img.example.com"));
        assert!(covers("example.com", "example.com"));
        assert!(!covers("example.com", "badexample.com"));
        assert!(!covers("img.example.com", "example.com"));
    }

    #[test]
    fn block_is_absolute_and_block_lists_win() {
        let p = policy(RemoteContent::Block, &["a.example"], &[]);
        assert_eq!(
            p.decide("a.example", true),
            Decision::Blocked { by_setting: true }
        );
        for mode in [RemoteContent::Allow, RemoteContent::Ask] {
            let p = policy(mode, &["a.example"], &["a.example", "ads.example"]);
            assert_eq!(
                p.decide("a.example", true),
                Decision::Blocked { by_setting: false }
            );
            assert_eq!(
                p.decide("x.ads.example", false),
                Decision::Blocked { by_setting: false }
            );
        }
    }

    #[test]
    fn ask_loads_only_named_sites() {
        let p = policy(RemoteContent::Ask, &["img.example"], &[]);
        assert_eq!(p.decide("other.example", false), Decision::Ask);
        assert_eq!(
            p.decide("cdn.img.example", false),
            Decision::Load { private_ok: true }
        );
        assert_eq!(
            p.decide("other.example", true),
            Decision::Load { private_ok: true }
        );
    }

    #[test]
    fn allow_asks_for_private_hosts() {
        let p = policy(RemoteContent::Allow, &[], &[]);
        assert_eq!(
            p.decide("example.org", false),
            Decision::Load { private_ok: false }
        );
        for host in ["localhost", "127.0.0.1", "192.168.1.1", "router", "[::1]"] {
            assert_eq!(p.decide(host, false), Decision::Ask, "{host}");
        }
        assert_eq!(
            p.decide("127.0.0.1", true),
            Decision::Load { private_ok: true }
        );
    }

    #[test]
    fn a_restricted_vault_blocks_remote_content() {
        let mut s = gv_settings::Settings::default();
        s.privacy_and_security.remote_content = RemoteContent::Allow;
        s.privacy_and_security.workspace_trust = WorkspaceTrust::Restricted;
        assert_eq!(Policy::of(&s).mode, RemoteContent::Block);
    }

    #[test]
    fn placeholders_say_why() {
        let host = "example.org".to_owned();
        assert_eq!(
            RemoteShown::Waiting { host: host.clone() }.card_message("Logo"),
            "Logo: Remote image from example.org, not loaded."
        );
        assert_eq!(
            RemoteShown::Blocked {
                host: host.clone(),
                by_setting: true
            }
            .card_message(""),
            "Remote content is blocked in your settings."
        );
        assert_eq!(
            RemoteShown::Blocked {
                host,
                by_setting: false
            }
            .card_message(""),
            "Images from example.org are blocked."
        );
    }
}
