// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The one request of the update check: the latest release of
//! `sorrelfox/glyphvane` from the GitHub Releases API, with the `ETag` of
//! the last answer so an unchanged answer costs nothing (plan 14 §
//! Updates). The request carries the generic user agent and nothing else
//! about the user or the system. Nothing is downloaded but this small
//! JSON answer.
//!
//! The network goes through a [`Transport`], so tests never touch it: the
//! real one is ureq over rustls with certificates checked against the
//! operating system's store, as for remote images.

use std::sync::Arc;
use std::time::Duration;

use gv_settings::{ReleaseRecord, UpdateState};

use super::version::Version;

/// The API address of the latest release.
pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/sorrelfox/glyphvane/releases/latest";

/// Every release page starts with this; nothing else is ever opened.
pub const RELEASES_PAGE: &str = "https://github.com/sorrelfox/glyphvane/releases";

/// The longest answer read.
const MAX_BODY: u64 = 1024 * 1024;

/// The longest release notes kept.
const MAX_NOTES: usize = 8 * 1024;

/// How long one request may take.
const TIMEOUT: Duration = Duration::from_secs(20);

/// An HTTP answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub body: Vec<u8>,
}

/// Makes the request. Called off the UI thread; it may block.
pub trait Transport: Send + Sync {
    /// `GET url`, with `If-None-Match: etag` when given.
    fn get(&self, url: &str, etag: Option<&str>) -> Result<HttpResponse, String>;
}

/// The real transport.
#[derive(Debug, Default)]
pub struct UreqTransport {
    agent: std::sync::OnceLock<ureq::Agent>,
}

impl UreqTransport {
    fn agent(&self) -> &ureq::Agent {
        self.agent.get_or_init(|| {
            use ureq::tls::{RootCerts, TlsConfig};
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let tls = TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build();
            let config = ureq::config::Config::builder()
                .tls_config(tls)
                .proxy(ureq::Proxy::try_from_env())
                .http_status_as_error(false)
                .https_only(true)
                .max_redirects(3)
                .save_redirect_history(false)
                .user_agent(gv_media::remote::user_agent())
                .accept("application/vnd.github+json")
                .timeout_global(Some(TIMEOUT))
                .build();
            ureq::Agent::new_with_config(config)
        })
    }
}

impl Transport for UreqTransport {
    fn get(&self, url: &str, etag: Option<&str>) -> Result<HttpResponse, String> {
        let mut request = self
            .agent()
            .get(url)
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        let response = request.call().map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = if status == 200 {
            response
                .into_body()
                .into_with_config()
                .limit(MAX_BODY)
                .read_to_vec()
                .map_err(|e| e.to_string())?
        } else {
            Vec::new()
        };
        Ok(HttpResponse { status, etag, body })
    }
}

/// A release, as the API describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// The release page, always below [`RELEASES_PAGE`].
    pub url: String,
    /// The release notes (Markdown), capped.
    pub notes: String,
}

impl Release {
    /// The release as the state file keeps it.
    pub fn record(&self) -> ReleaseRecord {
        ReleaseRecord {
            tag: self.tag.clone(),
            url: self.url.clone(),
            notes: self.notes.clone(),
        }
    }

    /// The release a record names, if it is still valid.
    pub fn from_record(record: &ReleaseRecord) -> Option<Self> {
        Some(Self {
            version: Version::parse(&record.tag)?,
            tag: record.tag.clone(),
            url: page_url(&record.url, &record.tag),
            notes: record.notes.clone(),
        })
    }
}

/// The release page to open: `url` when it is one of the project's release
/// pages, else the page the tag names.
fn page_url(url: &str, tag: &str) -> String {
    let ok = url
        .strip_prefix(RELEASES_PAGE)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        && !url.contains(['"', '\\', ' ', '\n']);
    if ok {
        url.to_owned()
    } else {
        format!("{RELEASES_PAGE}/tag/{}", percent_encode(tag))
    }
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~' | b'+') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Reads the API's answer for one release. Drafts and pre-releases are not
/// offered.
pub fn parse_release(body: &[u8]) -> Result<Release, String> {
    #[derive(serde::Deserialize)]
    struct Raw {
        tag_name: String,
        #[serde(default)]
        html_url: String,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
    }
    let raw: Raw =
        serde_json::from_slice(body).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    if raw.draft || raw.prerelease {
        return Err("the latest release is a draft or a pre-release".to_owned());
    }
    let version = Version::parse(&raw.tag_name)
        .ok_or_else(|| format!("the release tag {:?} is not a version", raw.tag_name))?;
    let mut notes = raw.body.unwrap_or_default().replace("\r\n", "\n");
    if notes.len() > MAX_NOTES {
        let mut end = MAX_NOTES;
        while !notes.is_char_boundary(end) {
            end -= 1;
        }
        notes.truncate(end);
        notes.push('…');
    }
    Ok(Release {
        url: page_url(&raw.html_url, &raw.tag_name),
        version,
        tag: raw.tag_name,
        notes,
    })
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A release newer than this build.
    Newer(Release),
    /// This build is the newest release (or newer).
    UpToDate,
    /// The request failed.
    Failed(String),
}

/// Makes the request and records the answer in `state` (`now` in Unix
/// seconds). Blocking.
pub fn check(
    transport: &dyn Transport,
    current: &Version,
    state: &mut UpdateState,
    now: u64,
) -> Outcome {
    state.last_attempt = Some(now);
    let cached = state.latest.as_ref().and_then(Release::from_record);
    // Without a release to fall back on, a 304 would tell nothing.
    let etag = cached.as_ref().and(state.etag.as_deref());
    let release = match transport.get(LATEST_RELEASE_URL, etag) {
        Err(err) => return fail(state, err),
        Ok(response) => match response.status {
            304 => match cached {
                Some(release) => release,
                None => return fail(state, "GitHub answered 304 without a release".into()),
            },
            200 => match parse_release(&response.body) {
                Ok(release) => {
                    state.etag = response.etag;
                    state.latest = Some(release.record());
                    release
                }
                Err(err) => return fail(state, err),
            },
            // No release published yet.
            404 => {
                state.failures = 0;
                state.last_success = Some(now);
                state.latest = None;
                state.etag = None;
                return Outcome::UpToDate;
            }
            status => return fail(state, format!("GitHub answered with status {status}")),
        },
    };
    state.failures = 0;
    state.last_success = Some(now);
    if release.version > *current {
        Outcome::Newer(release)
    } else {
        Outcome::UpToDate
    }
}

fn fail(state: &mut UpdateState, err: String) -> Outcome {
    state.failures = state.failures.saturating_add(1);
    Outcome::Failed(err)
}

/// At most one automatic check a day.
pub const INTERVAL: u64 = 24 * 60 * 60;

/// The first retry after a failure; each further failure doubles it, up to
/// [`INTERVAL`].
pub const FIRST_RETRY: u64 = 60 * 60;

/// When the next automatic check is due (Unix seconds).
pub fn next_due(state: &UpdateState) -> u64 {
    let Some(last) = state.last_attempt else {
        return 0;
    };
    let wait = if state.failures == 0 {
        INTERVAL
    } else {
        let doublings = (state.failures - 1).min(16);
        FIRST_RETRY.saturating_mul(1 << doublings).min(INTERVAL)
    };
    last.saturating_add(wait)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Answers from a list and records the requests.
    #[derive(Default)]
    pub(crate) struct FakeTransport {
        pub answers: Mutex<Vec<Result<HttpResponse, String>>>,
        pub requests: Mutex<Vec<(String, Option<String>)>>,
    }

    impl Transport for FakeTransport {
        fn get(&self, url: &str, etag: Option<&str>) -> Result<HttpResponse, String> {
            self.requests
                .lock()
                .expect("lock")
                .push((url.to_owned(), etag.map(str::to_owned)));
            self.answers.lock().expect("lock").remove(0)
        }
    }

    fn ok(tag: &str, etag: &str) -> Result<HttpResponse, String> {
        Ok(HttpResponse {
            status: 200,
            etag: Some(etag.to_owned()),
            body: format!(
                r#"{{"tag_name":"{tag}","html_url":"https://github.com/sorrelfox/glyphvane/releases/tag/{tag}","body":"Notes for {tag}","draft":false,"prerelease":false,"assets":[]}}"#
            )
            .into_bytes(),
        })
    }

    fn v(text: &str) -> Version {
        Version::parse(text).expect("version")
    }

    #[test]
    fn a_newer_release_is_found_and_remembered() {
        let fake = FakeTransport::default();
        fake.answers
            .lock()
            .expect("lock")
            .push(ok("v1.1.0", "\"e1\""));
        let mut state = UpdateState::default();
        let outcome = check(&fake, &v("1.0.0"), &mut state, 100);
        let Outcome::Newer(release) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(release.version, v("1.1.0"));
        assert_eq!(
            release.url,
            "https://github.com/sorrelfox/glyphvane/releases/tag/v1.1.0"
        );
        assert_eq!(release.notes, "Notes for v1.1.0");
        assert_eq!(state.etag.as_deref(), Some("\"e1\""));
        assert_eq!(state.last_success, Some(100));
        let requests = fake.requests.lock().expect("lock");
        assert_eq!(requests[0], (LATEST_RELEASE_URL.to_owned(), None));
    }

    #[test]
    fn not_modified_uses_the_remembered_release() {
        let fake = FakeTransport::default();
        fake.answers.lock().expect("lock").extend([
            ok("v1.0.0", "\"e1\""),
            Ok(HttpResponse {
                status: 304,
                etag: None,
                body: Vec::new(),
            }),
        ]);
        let mut state = UpdateState::default();
        assert_eq!(check(&fake, &v("1.0.0"), &mut state, 1), Outcome::UpToDate);
        // An older build learns from the same remembered answer.
        assert!(matches!(
            check(&fake, &v("0.9.0"), &mut state, 2),
            Outcome::Newer(r) if r.tag == "v1.0.0"
        ));
        let requests = fake.requests.lock().expect("lock");
        assert_eq!(requests[1].1.as_deref(), Some("\"e1\""));
    }

    #[test]
    fn failures_count_and_back_off() {
        let fake = FakeTransport::default();
        fake.answers.lock().expect("lock").extend([
            Err("offline".to_owned()),
            Ok(HttpResponse {
                status: 503,
                etag: None,
                body: Vec::new(),
            }),
            Ok(HttpResponse {
                status: 200,
                etag: None,
                body: b"<html>".to_vec(),
            }),
        ]);
        let mut state = UpdateState::default();
        assert_eq!(next_due(&state), 0, "never checked: due now");
        for (round, now) in [(1, 1000), (2, 2000), (3, 3000)] {
            assert!(matches!(
                check(&fake, &v("1.0.0"), &mut state, now),
                Outcome::Failed(_)
            ));
            assert_eq!(state.failures, round);
        }
        // 1 h, 2 h, then 4 h after the last attempt.
        assert_eq!(next_due(&state), 3000 + 4 * FIRST_RETRY);
        state.failures = 30;
        assert_eq!(next_due(&state), 3000 + INTERVAL, "capped at a day");
        state.failures = 0;
        assert_eq!(next_due(&state), 3000 + INTERVAL);
    }

    #[test]
    fn no_release_yet_is_up_to_date() {
        let fake = FakeTransport::default();
        fake.answers.lock().expect("lock").push(Ok(HttpResponse {
            status: 404,
            etag: None,
            body: Vec::new(),
        }));
        let mut state = UpdateState::default();
        assert_eq!(check(&fake, &v("1.0.0"), &mut state, 5), Outcome::UpToDate);
        assert_eq!(state.failures, 0);
    }

    #[test]
    fn releases_are_read_carefully() {
        let draft = br#"{"tag_name":"v2.0.0","html_url":"","draft":true,"prerelease":false}"#;
        assert!(parse_release(draft).is_err());
        let pre = br#"{"tag_name":"v2.0.0-rc.1","draft":false,"prerelease":true}"#;
        assert!(parse_release(pre).is_err());
        let bad_tag = br#"{"tag_name":"latest"}"#;
        assert!(parse_release(bad_tag).is_err());
        // A page outside the project's releases is never used.
        let elsewhere =
            br#"{"tag_name":"v2.0.0","html_url":"https://example.com/evil","body":null}"#;
        let release = parse_release(elsewhere).expect("readable");
        assert_eq!(
            release.url,
            "https://github.com/sorrelfox/glyphvane/releases/tag/v2.0.0"
        );
        let lookalike = br#"{"tag_name":"v2.0.0","html_url":"https://github.com/sorrelfox/glyphvane/releases.evil.com/x"}"#;
        assert!(
            parse_release(lookalike)
                .expect("readable")
                .url
                .ends_with("/tag/v2.0.0")
        );
        // Long notes are capped.
        let long = format!(
            r#"{{"tag_name":"v2.0.0","body":"{}"}}"#,
            "é".repeat(MAX_NOTES)
        );
        let notes = parse_release(long.as_bytes()).expect("readable").notes;
        assert!(notes.len() <= MAX_NOTES + '…'.len_utf8());
        assert!(notes.ends_with('…'));
    }
}
