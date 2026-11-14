// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! URL classification for pasted HTML.
//!
//! The same normalization and network-path rules as the vault's link
//! classifier, reduced to what the paste converter needs: is a value a
//! script URL, a `data:` URL, a network path (UNC, `file://host`, share
//! schemes) or a dangerous OS protocol handler; and if it is a web URL,
//! its serialized form.
//!
//! The checks look at the raw string as well as at the parsed URL: the
//! `url` crate collapses `file:////host/share` to an empty host plus the
//! path `//host/share`, and `\\host` can arrive percent-encoded as
//! `%5C%5Chost`. Four or more slashes after `file:`, any backslash there,
//! or a decoded leading `\\` or `//` therefore mean a network path.
//!
//! Values come from the HTML tokenizer with character references already
//! decoded.

/// What a URL attribute value is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dest {
    /// `#id`.
    Fragment,
    /// A relative reference.
    Relative,
    /// `http(s)` with a host (protocol-relative `//host` resolves against
    /// `https:`); the serialized URL.
    Web(String),
    /// `mailto:`.
    Mailto,
    /// A local absolute path (`file:///…`, `C:\…`).
    LocalFile,
    /// UNC, `file://host`, `smb:` and the other network-path forms.
    Network,
    /// `javascript:`, `vbscript:` and look-alikes.
    Script,
    /// A Windows protocol handler with a record of remote code execution.
    DangerousHandler,
    /// Any `data:` URL.
    Data,
    /// Another scheme (`tel:`, `obsidian:`, `ssh:`, unknown ones, …).
    OtherScheme,
    /// Control characters, empty, or an unparsable `http(s)`/`file` URL.
    Invalid,
}

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
/// tab and line breaks anywhere. `None` if other control characters remain.
pub(crate) fn normalize(raw: &str) -> Option<String> {
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

/// Splits off a scheme matching `^[A-Za-z][A-Za-z0-9+.-]*:`, lower-cased.
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

fn starts_with_two_separators(b: &[u8]) -> bool {
    b.len() >= 2 && matches!(b[0], b'/' | b'\\') && matches!(b[1], b'/' | b'\\')
}

fn web(s: &str) -> Dest {
    match url::Url::parse(s) {
        Ok(u) if u.host_str().is_some_and(|h| !h.is_empty()) => Dest::Web(u.to_string()),
        _ => Dest::Invalid,
    }
}

fn classify_file(s: &str, rest: &str) -> Dest {
    let lower = s.to_ascii_lowercase();
    if lower.contains("%5c") || lower.contains("%2f") {
        return Dest::Invalid;
    }
    let seps = rest.chars().take_while(|c| matches!(c, '/' | '\\')).count();
    if seps >= 4 || (seps >= 2 && rest.contains('\\')) {
        return Dest::Network;
    }
    match url::Url::parse(s) {
        Ok(u) => {
            let host = u.host_str().unwrap_or("");
            if (!host.is_empty() && !host.eq_ignore_ascii_case("localhost"))
                || u.path().starts_with("//")
            {
                Dest::Network
            } else {
                Dest::LocalFile
            }
        }
        Err(_) => Dest::Invalid,
    }
}

/// Classifies a URL attribute value or a pasted URL.
pub(crate) fn classify(raw: &str) -> Dest {
    let Some(s) = normalize(raw) else {
        return Dest::Invalid;
    };
    if s.is_empty() {
        return Dest::Invalid;
    }
    let b = s.as_bytes();
    if starts_with_two_separators(b) {
        if b[0] == b'\\' || b[1] == b'\\' || s[2..].starts_with(['/', '\\']) {
            return Dest::Network;
        }
        return web(&format!("https:{s}"));
    }
    if let Some((scheme, rest)) = split_scheme(&s)
        && (scheme.len() > 1 || rest.starts_with(['\\', '/']))
        && scheme.len() <= 32
    {
        if scheme.len() == 1 {
            return Dest::LocalFile;
        }
        return match scheme.as_str() {
            "http" | "https" => web(&s),
            "mailto" => Dest::Mailto,
            "data" => Dest::Data,
            "file" => classify_file(&s, rest),
            sch if SCRIPT_SCHEMES.contains(&sch) => Dest::Script,
            sch if DANGEROUS_HANDLERS.contains(&sch) => Dest::DangerousHandler,
            sch if SHARE_SCHEMES.contains(&sch) => Dest::Network,
            _ => Dest::OtherScheme,
        };
    }
    if s.starts_with('#') {
        return Dest::Fragment;
    }
    // A colon before any `/?#` that did not parse as a scheme is a relative
    // path for browsers; refuse obfuscations that legacy parsers accept.
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
            return Dest::Script;
        }
        if squeezed.starts_with("data:") {
            return Dest::Data;
        }
    }
    let decoded_head: String = {
        let l = s.to_ascii_lowercase();
        l.replace("%5c", "\\")
            .replace("%2f", "/")
            .chars()
            .take(3)
            .collect()
    };
    if starts_with_two_separators(decoded_head.as_bytes()) {
        return Dest::Network;
    }
    if s.contains('\\') {
        // A backslash is a path separator on Windows: never trusted in a
        // relative reference.
        return Dest::Invalid;
    }
    Dest::Relative
}

/// Resolves `raw` against `base` (an `http(s)` URL; any other base is
/// ignored, so nothing ever resolves against `file:`) and classifies the
/// result.
pub(crate) fn classify_with_base(raw: &str, base: Option<&url::Url>) -> (Dest, String) {
    let class = classify(raw);
    let normalized = normalize(raw).unwrap_or_default();
    match (&class, base) {
        (Dest::Relative | Dest::Fragment, Some(base)) => match base.join(&normalized) {
            Ok(joined) => {
                let joined = joined.to_string();
                match classify(&joined) {
                    Dest::Web(u) => (Dest::Web(u.clone()), u),
                    other => (other, joined),
                }
            }
            Err(_) => (Dest::Invalid, normalized),
        },
        (Dest::Web(u), _) => (class.clone(), u.clone()),
        _ => (class, normalized),
    }
}

/// Parses `base` and keeps it only if it is an `http(s)` URL with a host.
pub(crate) fn parse_base(base: Option<&str>) -> Option<url::Url> {
    let base = normalize(base?)?;
    match classify(&base) {
        Dest::Web(u) => url::Url::parse(&u).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_and_obfuscation() {
        for s in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            " javascript:x",
            "jav\tascript:x",
            "java\nscript:x",
            "vbscript:msgbox",
            "java script:x",
            "livescript:x",
        ] {
            assert_eq!(classify(s), Dest::Script, "{s:?}");
        }
        assert_eq!(classify("\u{1}javascript:x"), Dest::Script);
        assert_eq!(classify("java\u{1}script:x"), Dest::Invalid);
    }

    #[test]
    fn network_paths() {
        for s in [
            "\\\\host\\share",
            "//\\host",
            "\\/host",
            "///host/x",
            "file://host/share",
            "file:////host/share",
            "file:\\\\host\\x",
            "%5C%5Chost/x",
            "%2f%2fhost/x",
            "smb://host/x",
            "nfs://host/x",
        ] {
            assert_eq!(classify(s), Dest::Network, "{s:?}");
        }
        assert_eq!(classify("file:///home/a.png"), Dest::LocalFile);
        assert_eq!(classify("C:\\x\\y.png"), Dest::LocalFile);
        assert_eq!(classify("a\\b.png"), Dest::Invalid);
    }

    #[test]
    fn web_and_relative() {
        assert_eq!(
            classify("//example.com/x"),
            Dest::Web("https://example.com/x".into())
        );
        assert_eq!(
            classify("HTTP://Example.com/a b"),
            Dest::Web("http://example.com/a%20b".into())
        );
        assert_eq!(classify("http://"), Dest::Invalid);
        assert_eq!(classify("data:image/png;base64,AAAA"), Dest::Data);
        assert_eq!(classify("ms-msdt:/id"), Dest::DangerousHandler);
        assert_eq!(classify("tel:+1"), Dest::OtherScheme);
        assert_eq!(classify("#top"), Dest::Fragment);
        assert_eq!(classify("img/a.png"), Dest::Relative);
    }

    #[test]
    fn resolution() {
        let base = parse_base(Some("https://ex.com/docs/page.html"));
        assert!(base.is_some());
        let (d, u) = classify_with_base("../img/a.png", base.as_ref());
        assert!(matches!(d, Dest::Web(_)));
        assert_eq!(u, "https://ex.com/img/a.png");
        let (_, u) = classify_with_base("#s", base.as_ref());
        assert_eq!(u, "https://ex.com/docs/page.html#s");
        // A `file:` base is ignored.
        assert!(parse_base(Some("file://host/share/")).is_none());
        assert!(parse_base(Some("file:///home/")).is_none());
        let (d, _) = classify_with_base("x.png", None);
        assert_eq!(d, Dest::Relative);
    }
}
