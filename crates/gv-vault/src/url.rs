// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! URL normalization, classification and policy.
//!
//! [`classify`] says **what** a destination is (a fragment, a relative
//! path, a web URL, a network path, a script URL, …). [`UrlClass::policy`]
//! then says what a given context may do with it: allow, ask first, or
//! block. Nothing here fetches anything.
//!
//! The checks run on the raw string as well as on the parsed URL. Two
//! regressions are pinned by tests: the `url` crate collapses
//! `file:////host/share` to an empty host plus the path `//host/share`
//! (which Windows opens as `\\host\share`), and a Markdown destination
//! `\\host\…` reaches us percent-encoded as `%5C%5Chost…`. So four or more
//! slashes after `file:`, any backslash there, or a decoded `\\` or `//`
//! prefix all mean a network path.
//!
//! Input values are expected with character references already decoded
//! (the parser and the HTML tokenizer do that exactly once).

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

/// Where a URL is used. The same URL can be fine to click and forbidden
/// to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlContext {
    /// Clickable: `a[href]`, Markdown links, wikilinks, autolinks.
    Link,
    /// Loaded automatically: images, `srcset`, audio, video posters, embeds.
    Media,
    /// Shown as a card from its URL, never loaded (`iframe[src]`, `video[src]`).
    Card,
}

/// The kind of network path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkForm {
    /// `\\host`, `/\host`, `\/host`, `///host` and their percent-encoded forms.
    Unc,
    /// `file://host/…`, `file:////host/…`, `file:\\host`.
    FileHost,
    /// `smb:`, `cifs:`, `afp:`, `nfs:`, `webdav:`, `dav:`.
    ShareScheme,
}

/// Why a destination is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidUrl {
    /// Control characters other than tab and line breaks.
    ControlChars,
    /// Nothing left after trimming.
    Empty,
    /// An `http(s)` URL that does not parse or has no host.
    BadHttp,
    /// A `file:` URL with percent-encoded separators, or one that does not parse.
    BadFile,
}

/// What a destination is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlClass {
    /// `#id` in the current document.
    Fragment(String),
    /// A relative path (the link resolver handles it).
    Relative(String),
    /// `http(s)` with a host. `lan` marks loopback and private hosts.
    Web {
        /// The parsed, serialized URL.
        url: String,
        /// The host.
        host: String,
        /// Loopback, private, link-local or `.local` host.
        lan: bool,
        /// Written as `//host/…` and resolved against `https:`.
        protocol_relative: bool,
    },
    /// `mailto:`.
    Mailto(String),
    /// `data:` with a raster image type (`png`, `gif`, `jpeg`, `webp`, `avif`).
    DataImage {
        /// The MIME type.
        mime: String,
        /// Length of the whole URL.
        len: usize,
    },
    /// Any other `data:` URL.
    Data,
    /// A local absolute path: `file:///abs`, `file://localhost/abs`, or a
    /// Windows drive path `C:\…` / `C:/…`.
    LocalFile {
        /// The path as written (decoded for `file:` URLs).
        path: String,
        /// Written with a drive letter.
        drive: bool,
    },
    /// A network path. Never loaded; following needs confirmation.
    Network {
        /// Which form.
        form: NetworkForm,
        /// The normalized destination.
        url: String,
    },
    /// `javascript:`, `vbscript:` and look-alikes, including obfuscated forms.
    Script(String),
    /// A Windows protocol handler with a record of remote code execution.
    DangerousHandler(String),
    /// `obsidian://…`.
    Obsidian(String),
    /// A known external scheme that needs confirmation (`tel`, `ssh`, …).
    External {
        /// The lower-cased scheme.
        scheme: String,
        /// The normalized destination.
        url: String,
    },
    /// Any other scheme.
    Unknown {
        /// The lower-cased scheme.
        scheme: String,
        /// The normalized destination.
        url: String,
    },
    /// Not usable.
    Invalid(InvalidUrl),
}

/// Why something needs the user's confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmReason {
    /// A scheme from the confirm list (`tel`, `ftp`, `ssh`, …).
    ExternalScheme,
    /// A scheme Glyphvane does not know.
    UnknownScheme,
    /// An `obsidian://` link to another vault or action.
    Obsidian,
    /// A local file outside the vault root.
    LocalFile,
    /// A network path or share: following may send credentials (NTLM).
    NetworkPath,
    /// A loopback or private-network host (media in Allow mode still asks).
    LanHost,
}

/// Why something is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    /// A script URL.
    Script,
    /// A dangerous OS protocol handler.
    DangerousHandler,
    /// A `data:` URL that is not an allowed image, or any `data:` link.
    Data,
    /// A `data:` image over 10 MB.
    DataTooLarge,
    /// A network path in a context that loads automatically.
    NetworkPath,
    /// A scheme a media or card context cannot load.
    SchemeNotLoadable,
    /// A local file on a card.
    LocalFileOnCard,
    /// Control characters, empty, unparsable.
    Invalid,
    /// A path segment that decodes to `/`, `\` or NUL.
    EncodedSeparator,
    /// A path that is not valid UTF-8 after percent-decoding.
    InvalidUtf8,
    /// A backslash in a path (a separator on Windows, so never trusted).
    Backslash,
    /// An embed of a target outside the root (setting).
    OutsideRootEmbed,
}

impl fmt::Display for BlockReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Script => "script URL",
            Self::DangerousHandler => "dangerous OS protocol handler",
            Self::Data => "data: URL",
            Self::DataTooLarge => "data: image over 10 MB",
            Self::NetworkPath => "network path (UNC); never loaded automatically",
            Self::SchemeNotLoadable => "scheme cannot be loaded here",
            Self::LocalFileOnCard => "local file on a card",
            Self::Invalid => "invalid URL",
            Self::EncodedSeparator => "a path segment decodes to `/`, `\\` or NUL",
            Self::InvalidUtf8 => "path is not valid UTF-8 after decoding",
            Self::Backslash => "backslash in a path",
            Self::OutsideRootEmbed => "embed outside the root (blocked by setting)",
        })
    }
}

/// What a context may do with a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlPolicy {
    /// Follow or load without asking (remote media still go through the
    /// remote-content mode).
    Allow,
    /// Ask first.
    Confirm(ConfirmReason),
    /// Never.
    Block(BlockReason),
}

const DATA_IMAGE_MAX: usize = 10 * 1024 * 1024;

const CONFIRM_SCHEMES: &[&str] = &[
    "tel", "sms", "xmpp", "irc", "ircs", "ftp", "sftp", "ssh", "news", "geo", "magnet",
];

const SCRIPT_SCHEMES: &[&str] = &["javascript", "vbscript", "livescript", "mocha"];

const DANGEROUS_HANDLERS: &[&str] = &[
    "ms-msdt",
    "search-ms",
    "search",
    "ms-officecmd",
    "ms-cxh",
    "ms-cxh-full",
    "ms-appinstaller",
    "its",
    "ms-its",
    "mk",
    "hcp",
    "shell",
];

const SHARE_SCHEMES: &[&str] = &["smb", "cifs", "afp", "nfs", "webdav", "dav"];

/// WHATWG-style cleanup: trims C0 controls and spaces at both ends, removes
/// tab and line breaks anywhere. `None` if other control characters remain
/// (they are an obfuscation signal, not something to clean up).
#[must_use]
pub fn normalize(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches(|c: char| c <= ' ');
    let mut out = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        match c {
            '\t' | '\n' | '\r' => {}
            c if (c as u32) < 0x20 || c == '\u{7f}' => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

/// Splits off a scheme matching `^[A-Za-z][A-Za-z0-9+.-]*:` (any length;
/// see [`has_scheme`] for the link-resolution rule), lower-cased.
fn split_scheme(s: &str) -> Option<(String, &str)> {
    let colon = s.find(':')?;
    let (scheme, rest) = s.split_at(colon);
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), &rest[1..]))
}

/// `true` if `s` starts with a URL scheme by the link-resolution rule
/// `^[A-Za-z][A-Za-z0-9+.-]{1,31}:` (a single letter is a drive, not a scheme).
#[must_use]
pub fn has_scheme(s: &str) -> bool {
    match split_scheme(s) {
        Some((scheme, _)) => (2..=32).contains(&scheme.len()),
        None => false,
    }
}

/// `true` for loopback, private, link-local, unspecified and `.local` /
/// `localhost` hosts. A document must not make the app send requests into
/// the LAN without asking.
#[must_use]
pub fn is_lan_host(host: &str) -> bool {
    let h = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") {
        return true;
    }
    if let Ok(ip) = h.parse::<Ipv4Addr>() {
        let o = ip.octets();
        let cgnat = o[0] == 100 && (64..128).contains(&o[1]);
        return ip.is_loopback()
            || ip.is_private()
            || ip.is_link_local()
            || ip.is_unspecified()
            || cgnat;
    }
    if let Ok(ip) = h.parse::<Ipv6Addr>() {
        if let Some(v4) = ip.to_ipv4_mapped() {
            return is_lan_host(&v4.to_string());
        }
        let seg0 = ip.segments()[0];
        return ip.is_loopback()
            || ip.is_unspecified()
            || (seg0 & 0xfe00) == 0xfc00
            || (seg0 & 0xffc0) == 0xfe80;
    }
    false
}

fn web(url_str: &str, protocol_relative: bool) -> UrlClass {
    match url::Url::parse(url_str) {
        Ok(u) => match u.host_str() {
            Some(h) if !h.is_empty() => UrlClass::Web {
                lan: is_lan_host(h),
                host: h.to_owned(),
                url: u.to_string(),
                protocol_relative,
            },
            _ => UrlClass::Invalid(InvalidUrl::BadHttp),
        },
        Err(_) => UrlClass::Invalid(InvalidUrl::BadHttp),
    }
}

fn starts_with_two_separators(b: &[u8]) -> bool {
    b.len() >= 2 && matches!(b[0], b'/' | b'\\') && matches!(b[1], b'/' | b'\\')
}

fn classify_file(s: &str, rest: &str) -> UrlClass {
    let lower = s.to_ascii_lowercase();
    if lower.contains("%5c") || lower.contains("%2f") {
        return UrlClass::Invalid(InvalidUrl::BadFile);
    }
    // Count separators on the raw string: the url crate collapses
    // `file:////host` to an empty host plus `//host`.
    let seps = rest.chars().take_while(|c| matches!(c, '/' | '\\')).count();
    if seps >= 4 || (seps >= 2 && rest.contains('\\')) {
        return UrlClass::Network {
            form: NetworkForm::FileHost,
            url: s.to_owned(),
        };
    }
    match url::Url::parse(s) {
        Ok(u) => {
            let host = u.host_str().unwrap_or("");
            if (!host.is_empty() && !host.eq_ignore_ascii_case("localhost"))
                || u.path().starts_with("//")
            {
                return UrlClass::Network {
                    form: NetworkForm::FileHost,
                    url: s.to_owned(),
                };
            }
            let path = crate::link::percent_decode_lossy(u.path());
            // `file:///C:/x` is a drive path.
            let drive = path.len() >= 3
                && path.as_bytes()[0] == b'/'
                && path.as_bytes()[1].is_ascii_alphabetic()
                && path.as_bytes()[2] == b':';
            let path = if drive { path[1..].to_owned() } else { path };
            UrlClass::LocalFile { path, drive }
        }
        Err(_) => UrlClass::Invalid(InvalidUrl::BadFile),
    }
}

/// Classifies a link destination or URL attribute value.
#[must_use]
pub fn classify(raw: &str) -> UrlClass {
    let Some(s) = normalize(raw) else {
        return UrlClass::Invalid(InvalidUrl::ControlChars);
    };
    if s.is_empty() {
        return UrlClass::Invalid(InvalidUrl::Empty);
    }

    // Network-path references: two leading separators in any mix of `/`
    // and `\`. Browsers treat `\` as `/` here; against a `file:` base that
    // is UNC. Only a plain `//host` is protocol-relative (https).
    let b = s.as_bytes();
    if starts_with_two_separators(b) {
        if b[0] == b'\\' || b[1] == b'\\' || s[2..].starts_with(['/', '\\']) {
            return UrlClass::Network {
                form: NetworkForm::Unc,
                url: s,
            };
        }
        return web(&format!("https:{s}"), true);
    }

    // A scheme is `^[A-Za-z][A-Za-z0-9+.-]{1,31}:`; a single letter followed
    // by `:\` or `:/` is a Windows drive path. Anything else with a colon is
    // a relative path (checked for obfuscation below).
    if let Some((scheme, rest)) = split_scheme(&s)
        && (scheme.len() > 1 || rest.starts_with(['\\', '/']))
        && scheme.len() <= 32
    {
        if scheme.len() == 1 {
            return UrlClass::LocalFile {
                path: s.clone(),
                drive: true,
            };
        }
        return match scheme.as_str() {
            "http" | "https" => web(&s, false),
            "mailto" => UrlClass::Mailto(s.clone()),
            sch if SCRIPT_SCHEMES.contains(&sch) => UrlClass::Script(scheme),
            sch if DANGEROUS_HANDLERS.contains(&sch) => UrlClass::DangerousHandler(scheme),
            "data" => {
                let lower = rest.to_ascii_lowercase();
                let mime_end = lower.find([';', ',']).unwrap_or(lower.len());
                let mime = lower[..mime_end].trim().to_owned();
                if matches!(
                    mime.as_str(),
                    "image/png"
                        | "image/gif"
                        | "image/jpeg"
                        | "image/jpg"
                        | "image/webp"
                        | "image/avif"
                ) {
                    UrlClass::DataImage { mime, len: s.len() }
                } else {
                    UrlClass::Data
                }
            }
            "file" => classify_file(&s, rest),
            sch if SHARE_SCHEMES.contains(&sch) => UrlClass::Network {
                form: NetworkForm::ShareScheme,
                url: s.clone(),
            },
            "obsidian" => UrlClass::Obsidian(s.clone()),
            sch if CONFIRM_SCHEMES.contains(&sch) => UrlClass::External {
                scheme,
                url: s.clone(),
            },
            _ => UrlClass::Unknown {
                scheme,
                url: s.clone(),
            },
        };
    }

    if let Some(id) = s.strip_prefix('#') {
        return UrlClass::Fragment(id.to_owned());
    }
    // A colon before any `/?#` that did not parse as a scheme (`ja va:`) is
    // a relative path for browsers; refuse obvious obfuscations that some
    // legacy parsers accept.
    let head: String = s
        .chars()
        .take_while(|c| !matches!(c, '/' | '?' | '#'))
        .collect();
    if head.contains(':') {
        let squeezed: String = head
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == ':')
            .collect::<String>()
            .to_ascii_lowercase();
        if SCRIPT_SCHEMES
            .iter()
            .any(|sch| squeezed.starts_with(&format!("{sch}:")))
        {
            return UrlClass::Script(squeezed.split(':').next().unwrap_or("").to_owned());
        }
        if squeezed.starts_with("data:") {
            return UrlClass::Data;
        }
    }
    // A decoded leading `\\` or `//` turns a "relative" path into UNC.
    let decoded_head: String = {
        let l = s.to_ascii_lowercase();
        l.replace("%5c", "\\")
            .replace("%2f", "/")
            .chars()
            .take(3)
            .collect()
    };
    if starts_with_two_separators(decoded_head.as_bytes()) {
        return UrlClass::Network {
            form: NetworkForm::Unc,
            url: s,
        };
    }
    UrlClass::Relative(s)
}

impl UrlClass {
    /// What `ctx` may do with this destination.
    #[must_use]
    pub fn policy(&self, ctx: UrlContext) -> UrlPolicy {
        use UrlPolicy::{Allow, Block, Confirm};
        match (self, ctx) {
            (Self::Fragment(_) | Self::Relative(_), _) => Allow,
            (Self::Web { .. }, UrlContext::Link | UrlContext::Card) => Allow,
            (Self::Web { lan: true, .. }, UrlContext::Media) => Confirm(ConfirmReason::LanHost),
            (Self::Web { .. }, UrlContext::Media) => Allow,
            (Self::Mailto(_), UrlContext::Link) => Allow,
            (Self::DataImage { len, .. }, UrlContext::Media) => {
                if *len > DATA_IMAGE_MAX * 4 / 3 + 64 {
                    Block(BlockReason::DataTooLarge)
                } else {
                    Allow
                }
            }
            (Self::DataImage { .. } | Self::Data, _) => Block(BlockReason::Data),
            (Self::LocalFile { .. }, UrlContext::Link | UrlContext::Media) => {
                Confirm(ConfirmReason::LocalFile)
            }
            (Self::LocalFile { .. }, UrlContext::Card) => Block(BlockReason::LocalFileOnCard),
            (Self::Network { .. }, UrlContext::Link) => Confirm(ConfirmReason::NetworkPath),
            (Self::Network { .. }, _) => Block(BlockReason::NetworkPath),
            (Self::Script(_), _) => Block(BlockReason::Script),
            (Self::DangerousHandler(_), _) => Block(BlockReason::DangerousHandler),
            (Self::Obsidian(_), UrlContext::Link) => Confirm(ConfirmReason::Obsidian),
            (Self::External { .. }, UrlContext::Link) => Confirm(ConfirmReason::ExternalScheme),
            (Self::Unknown { .. }, UrlContext::Link) => Confirm(ConfirmReason::UnknownScheme),
            (Self::Invalid(_), _) => Block(BlockReason::Invalid),
            (
                Self::Mailto(_) | Self::Obsidian(_) | Self::External { .. } | Self::Unknown { .. },
                _,
            ) => Block(BlockReason::SchemeNotLoadable),
        }
    }

    /// `true` for every network-path form.
    #[must_use]
    pub fn is_network(&self) -> bool {
        matches!(self, Self::Network { .. })
    }
}

/// Parses an HTML `srcset` value into `(url, descriptor)` candidates
/// (simplified spec algorithm). Every candidate must be classified.
#[must_use]
pub fn parse_srcset(v: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b: Vec<char> = v.chars().collect();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_whitespace() || b[i] == ',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let start = i;
        while i < b.len() && !b[i].is_whitespace() {
            i += 1;
        }
        let mut url: String = b[start..i].iter().collect();
        let mut desc = String::new();
        if url.ends_with(',') {
            url = url.trim_end_matches(',').to_owned();
        } else {
            let mut depth = 0;
            while i < b.len() {
                let c = b[i];
                if c == '(' {
                    depth += 1;
                } else if c == ')' && depth > 0 {
                    depth -= 1;
                } else if c == ',' && depth == 0 {
                    i += 1;
                    break;
                }
                desc.push(c);
                i += 1;
            }
        }
        out.push((url, desc.trim().to_owned()));
    }
    out
}
