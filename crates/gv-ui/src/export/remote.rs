// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The *embed remote images* option of an HTML export (FR-EXPORT-01):
//! remote images fetched through the Reading view's fetcher, only as the
//! remote-content setting allows.
//!
//! - A site the setting loads without asking (Allow mode, the allow
//!   list) is fetched the way the Reading view fetches it: redirects may
//!   not reach a blocked site, nor a private host the user did not name;
//!   the fetcher's caps, cache and privacy rules apply.
//! - A site that waits for approval (Ask) is not fetched; an image the
//!   Reading view already fetched in this run (the user chose *Load once*
//!   or the site was allowed then) is taken from the cache, which sends
//!   nothing.
//! - Block mode, a restricted vault and sites on a block list fetch
//!   nothing.
//!
//! Whatever is not embedded keeps its URL and is reported with the
//! export's other images.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::App;
use gv_media::CancelFlag;
use gv_media::remote::RemoteFetcher;

use crate::reader::remote::{Decision, Policy, fetch_image, fetched, shared_fetcher};

/// Fetches an export's remote images under the remote-content setting in
/// effect when the export was asked for.
#[derive(Clone)]
pub struct RemoteEmbed {
    fetcher: Arc<RemoteFetcher>,
    policy: Policy,
}

impl RemoteEmbed {
    /// The app's fetcher and the remote-content setting in effect now.
    pub fn of_app(cx: &mut App) -> Self {
        Self {
            fetcher: shared_fetcher(cx),
            policy: Policy::of_app(cx),
        }
    }
}

impl std::fmt::Debug for RemoteEmbed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteEmbed")
            .field("mode", &self.policy.mode)
            .finish_non_exhaustive()
    }
}

/// Two are the same when they apply the same setting.
impl PartialEq for RemoteEmbed {
    fn eq(&self, other: &Self) -> bool {
        self.policy == other.policy
    }
}

impl gv_export::RemoteImages for RemoteEmbed {
    fn fetch(&self, url: &str) -> Result<PathBuf, String> {
        let Some(host) = gv_media::remote::host_of(url) else {
            return Err("not a valid address".to_owned());
        };
        match self.policy.decide(&host, false) {
            Decision::Load { private_ok } => fetch_image(
                &self.fetcher,
                url,
                private_ok,
                &self.policy,
                &CancelFlag::new(),
            )
            .map_err(|e| e.card_message()),
            Decision::Ask => fetched(url).map(|(path, _)| path).ok_or_else(|| {
                format!("images from {host} wait for approval in the remote-content setting")
            }),
            Decision::Blocked { by_setting: true } => {
                Err("remote content is blocked in the settings".to_owned())
            }
            Decision::Blocked { by_setting: false } => {
                Err(format!("images from {host} are blocked"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gv_export::RemoteImages as _;
    use gv_settings::RemoteContent;

    fn embed(mode: RemoteContent, allow: &[&str], block: &[&str]) -> RemoteEmbed {
        let dir = std::env::temp_dir().join("glyphvane-export-remote-test");
        RemoteEmbed {
            fetcher: Arc::new(RemoteFetcher::new(gv_media::remote::FetcherConfig::new(
                dir,
            ))),
            policy: Policy {
                mode,
                allow: allow.iter().map(|s| (*s).to_owned()).collect(),
                block: block.iter().map(|s| (*s).to_owned()).collect(),
            },
        }
    }

    #[test]
    fn only_what_the_setting_allows_is_fetched() {
        // None of these reaches the network: refused before any request.
        let e = embed(RemoteContent::Block, &["img.example"], &[]);
        assert_eq!(
            e.fetch("https://img.example/a.png"),
            Err("remote content is blocked in the settings".to_owned())
        );
        let e = embed(RemoteContent::Allow, &[], &["ads.example"]);
        assert_eq!(
            e.fetch("https://x.ads.example/a.png"),
            Err("images from x.ads.example are blocked".to_owned())
        );
        let e = embed(RemoteContent::Ask, &[], &[]);
        assert_eq!(
            e.fetch("https://never-fetched.example/a.png"),
            Err(
                "images from never-fetched.example wait for approval in the remote-content setting"
                    .to_owned()
            )
        );
        // Allow mode still asks for private hosts.
        let e = embed(RemoteContent::Allow, &[], &[]);
        assert!(e.fetch("http://127.0.0.1:9/a.png").is_err());
        assert_eq!(e.fetch("nope"), Err("not a valid address".to_owned()));
    }
}
