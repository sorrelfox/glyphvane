// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Links as the resolver receives them, and their decoding by kind.
//!
//! Decoding **depends on the link kind**:
//!
//! - **Markdown and HTML destinations** (inline, reference, image, autolink,
//!   HTML attributes) arrive as the parser produced them: backslash escapes
//!   and character references already decoded, angle brackets removed.
//!   Scheme and network-path checks run first, on the whole value. A local
//!   destination is split at the first `#` (fragment), then at the first `?`
//!   (query), and **then** each path segment is percent-decoded once, as
//!   UTF-8. A segment that decodes to `/`, `\` or NUL, a raw backslash, or
//!   invalid UTF-8 makes the link `Blocked`.
//! - **Wikilinks and embeds** are never percent-decoded: `[[My%20Note]]`
//!   names a file literally called `My%20Note`. The display (or image size)
//!   slot is split off at `|` in the profile's pipe order, then the target
//!   at the first `#` into path and subpath chain.

use std::borrow::Cow;
use std::fmt;

use crate::fragment::Fragment;
use crate::path::{VaultPath, extension_of};
use crate::profile::{PageNameMapping, PipeOrder, ProfileId, ProfileParams};
use crate::url::{BlockReason, NetworkForm, UrlClass, UrlContext, classify, has_scheme, normalize};

/// The syntactic kind of a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// `[t](dest)`.
    Inline,
    /// `[t][label]` with `[label]: dest`.
    Reference,
    /// `![alt](src)` and its reference forms.
    Image,
    /// `<scheme:…>`, `<a@b.c>`, and extended autolinks (`www.x.com`, bare URLs
    /// and emails).
    Autolink,
    /// `[[target#sub|display]]`.
    Wikilink,
    /// `![[target#sub|size-or-display]]`.
    Embed,
    /// `<a href>`.
    HtmlHref,
    /// `<img src>`, `<source srcset>` candidates, `<audio src>`, `<video poster>`.
    HtmlSrc,
    /// `<iframe src>`, `<video src>`: shown as a card, never loaded.
    HtmlCard,
}

impl LinkKind {
    /// Wikilink syntax (no percent-decoding, pipe slots).
    #[must_use]
    pub fn is_wiki(self) -> bool {
        matches!(self, Self::Wikilink | Self::Embed)
    }

    /// The URL-policy context.
    #[must_use]
    pub fn url_context(self) -> UrlContext {
        match self {
            Self::Image | Self::Embed | Self::HtmlSrc => UrlContext::Media,
            Self::HtmlCard => UrlContext::Card,
            Self::Inline | Self::Reference | Self::Autolink | Self::Wikilink | Self::HtmlHref => {
                UrlContext::Link
            }
        }
    }

    /// `true` if the target's content is loaded into the document (embeds,
    /// images, media sources).
    #[must_use]
    pub fn loads_content(self) -> bool {
        self.url_context() == UrlContext::Media
    }
}

impl fmt::Display for LinkKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Inline => "inline",
            Self::Reference => "reference",
            Self::Image => "image",
            Self::Autolink => "autolink",
            Self::Wikilink => "wikilink",
            Self::Embed => "embed",
            Self::HtmlHref => "html-href",
            Self::HtmlSrc => "html-src",
            Self::HtmlCard => "html-card",
        })
    }
}

/// A link to resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRef {
    /// Its syntactic kind.
    pub kind: LinkKind,
    /// For Markdown and HTML kinds: the destination as the parser produced
    /// it (escapes and entities decoded, angle brackets removed). For
    /// wikilinks and embeds: the text between the brackets, with `\|`
    /// already unescaped inside tables.
    pub raw_destination: String,
    /// The file the link is in. Links inside an embedded note use the
    /// embedded file.
    pub source_file: VaultPath,
    /// The profile of the source document.
    pub profile: ProfileId,
}

impl LinkRef {
    /// Creates a link.
    #[must_use]
    pub fn new(
        kind: LinkKind,
        raw: impl Into<String>,
        source: VaultPath,
        profile: ProfileId,
    ) -> Self {
        Self {
            kind,
            raw_destination: raw.into(),
            source_file: source,
            profile,
        }
    }
}

/// A local link path after decoding (not yet normalized).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPath {
    /// The decoded path text, for traces.
    pub text: String,
    /// Decoded, non-empty segments; `.` and `..` are kept for the lexical
    /// normalization step.
    pub segments: Vec<String>,
    /// Written with a leading `/`: resolves against the link root.
    pub absolute: bool,
    /// Written with a trailing `/`: only a directory can match.
    pub trailing_slash: bool,
}

impl LinkPath {
    /// No path at all: the current document.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty() && !self.absolute
    }

    /// Starts with `./` or `../`: relative to the source file only.
    #[must_use]
    pub fn is_dot_relative(&self) -> bool {
        matches!(self.segments.first().map(String::as_str), Some("." | ".."))
    }

    pub(crate) fn parse(text: &str, decode: bool) -> Result<Self, BlockReason> {
        let mut segments = Vec::new();
        let mut shown = Vec::new();
        for piece in text.split('/') {
            if piece.is_empty() {
                continue;
            }
            // A backslash is a separator on Windows, so it is never trusted
            // inside a segment. ORACLE-PENDING for wikilinks: Obsidian may
            // treat `[[Folder\Note]]` as `Folder/Note`.
            if piece.contains('\\') {
                return Err(BlockReason::Backslash);
            }
            let seg = if decode {
                let bytes = percent_decode_bytes(piece);
                if bytes.iter().any(|&b| matches!(b, b'/' | b'\\' | 0)) {
                    return Err(BlockReason::EncodedSeparator);
                }
                String::from_utf8(bytes).map_err(|_| BlockReason::InvalidUtf8)?
            } else {
                if piece.contains('\0') {
                    return Err(BlockReason::EncodedSeparator);
                }
                piece.to_owned()
            };
            shown.push(seg.clone());
            segments.push(seg);
        }
        Ok(Self {
            text: shown.join("/"),
            segments,
            absolute: text.starts_with('/'),
            trailing_slash: text.len() > 1 && text.ends_with('/'),
        })
    }
}

/// An image size from a wikilink embed slot (`|300` or `|300x200`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSize {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels, if given.
    pub height: Option<u32>,
}

/// The destination after decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedTarget {
    /// A local path (possibly empty: the current document).
    Path(LinkPath),
    /// A URL or other non-path destination.
    Url(UrlClass),
    /// Invalid encoding or a forbidden character.
    Blocked(BlockReason),
}

/// A decoded link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedLink {
    /// Path, URL or blocked.
    pub target: DecodedTarget,
    /// The query string (Markdown and HTML only); ignored for local
    /// resolution except `plain=1`.
    pub query: Option<String>,
    /// The fragment or wikilink subpath.
    pub fragment: Option<Fragment>,
    /// Wikilink display text.
    pub display: Option<String>,
    /// Wikilink image size.
    pub size: Option<ImageSize>,
}

impl DecodedLink {
    /// `?plain=1`: open in Source mode.
    #[must_use]
    pub fn wants_source_mode(&self) -> bool {
        self.query
            .as_deref()
            .is_some_and(|q| q.split('&').any(|p| p == "plain=1"))
    }
}

/// Decodes `link` by its kind (see the [module docs](self)).
#[must_use]
pub fn decode(link: &LinkRef, params: &ProfileParams) -> DecodedLink {
    if link.kind.is_wiki() {
        decode_wiki(&link.raw_destination, params)
    } else {
        decode_markdown(link.kind, &link.raw_destination, params)
    }
}

fn empty_link() -> DecodedLink {
    DecodedLink {
        target: DecodedTarget::Path(LinkPath {
            text: String::new(),
            segments: Vec::new(),
            absolute: false,
            trailing_slash: false,
        }),
        query: None,
        fragment: None,
        display: None,
        size: None,
    }
}

fn autolink_url(raw: &str) -> Cow<'_, str> {
    let t = raw.trim();
    if has_scheme(t) {
        Cow::Borrowed(t)
    } else if t.contains('@') && !t.contains('/') {
        Cow::Owned(format!("mailto:{t}"))
    } else if t.len() > 4 && t[..4].eq_ignore_ascii_case("www.") {
        Cow::Owned(format!("http://{t}"))
    } else {
        Cow::Borrowed(t)
    }
}

fn decode_markdown(kind: LinkKind, raw: &str, params: &ProfileParams) -> DecodedLink {
    let raw = if kind == LinkKind::Autolink {
        autolink_url(raw)
    } else {
        Cow::Borrowed(raw)
    };
    if raw.trim().is_empty() {
        return empty_link();
    }
    let class = classify(&raw);
    let local = match &class {
        UrlClass::Relative(_) | UrlClass::Fragment(_) => normalize(&raw),
        _ => None,
    };
    let Some(s) = local else {
        return DecodedLink {
            target: DecodedTarget::Url(class),
            ..empty_link()
        };
    };
    let (path_query, fragment) = match s.split_once('#') {
        Some((pq, f)) => (pq, Some(f)),
        None => (s.as_str(), None),
    };
    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p, Some(q.to_owned())),
        None => (path_query, None),
    };
    let target = match LinkPath::parse(path, true) {
        Ok(p) => DecodedTarget::Path(p),
        Err(reason) => DecodedTarget::Blocked(reason),
    };
    DecodedLink {
        target,
        query,
        fragment: fragment.map(|f| Fragment::from_markdown(f, params)),
        display: None,
        size: None,
    }
}

fn is_image_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "svg" | "webp" | "avif" | "tif" | "tiff" | "ico"
    )
}

fn parse_size(slot: &str) -> Option<ImageSize> {
    let s = slot.trim();
    let (w, h) = match s.split_once('x') {
        Some((w, h)) => (w, Some(h)),
        None => (s, None),
    };
    let num = |t: &str| -> Option<u32> {
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
            None
        } else {
            t.parse().ok()
        }
    };
    Some(ImageSize {
        width: num(w)?,
        height: match h {
            Some(h) => Some(num(h)?),
            None => None,
        },
    })
}

fn decode_wiki(raw: &str, params: &ProfileParams) -> DecodedLink {
    let (target_part, slot) = match params.pipe_order {
        PipeOrder::TargetFirst => match raw.split_once('|') {
            Some((t, d)) => (t, Some(d)),
            None => (raw, None),
        },
        PipeOrder::DisplayFirst => match raw.rsplit_once('|') {
            Some((d, t)) => (t, Some(d)),
            None => (raw, None),
        },
    };
    // ORACLE-PENDING: Obsidian is assumed to trim spaces around the target.
    let target_part = target_part.trim();
    let (path_text, subpath) = match target_part.split_once('#') {
        Some((p, s)) => (p.trim_end(), Some(s)),
        None => (target_part, None),
    };
    let mapped: Cow<'_, str> = match params.page_names {
        PageNameMapping::Literal => Cow::Borrowed(path_text),
        PageNameMapping::SpacesToUnderscore => Cow::Owned(path_text.replace(' ', "_")),
        PageNameMapping::SpacesToHyphen => Cow::Owned(path_text.replace(' ', "-")),
    };
    let target = if mapped.starts_with("//") {
        DecodedTarget::Url(UrlClass::Network {
            form: NetworkForm::Unc,
            url: mapped.to_string(),
        })
    } else {
        match LinkPath::parse(&mapped, false) {
            Ok(p) => DecodedTarget::Path(p),
            Err(reason) => DecodedTarget::Blocked(reason),
        }
    };
    let is_image = extension_of(path_text.rsplit('/').next().unwrap_or(path_text))
        .is_some_and(is_image_extension);
    let (display, size) = match slot {
        Some(slot) if is_image => match parse_size(slot) {
            Some(size) => (None, Some(size)),
            None => (Some(slot.to_owned()), None),
        },
        Some(slot) => (Some(slot.to_owned()), None),
        None => (None, None),
    };
    DecodedLink {
        target,
        query: None,
        fragment: subpath.map(Fragment::from_wikilink),
        display,
        size,
    }
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decodes once. Malformed escapes (`%zz`, a trailing `%`) are kept
/// literally, as browsers do.
#[must_use]
pub fn percent_decode_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Percent-decodes once; invalid UTF-8 becomes U+FFFD.
#[must_use]
pub fn percent_decode_lossy(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    String::from_utf8_lossy(&percent_decode_bytes(s)).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode_bytes("a%20b"), b"a b");
        assert_eq!(percent_decode_bytes("%zz%2"), b"%zz%2");
        assert_eq!(percent_decode_bytes("100%25"), b"100%");
        assert_eq!(percent_decode_lossy("%C3%A9"), "\u{e9}");
        assert_eq!(percent_decode_lossy("%FF"), "\u{fffd}");
    }

    #[test]
    fn sizes() {
        assert_eq!(
            parse_size("300"),
            Some(ImageSize {
                width: 300,
                height: None
            })
        );
        assert_eq!(
            parse_size("300x200"),
            Some(ImageSize {
                width: 300,
                height: Some(200)
            })
        );
        assert_eq!(parse_size("x200"), None);
        assert_eq!(parse_size("big"), None);
    }
}
