// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Smart paste of plain text: a URL pasted over a selection makes a link.

use crate::paste::destination_for_paste;
use crate::profile::Profile;
use crate::url::{Dest, classify};

/// Longest clipboard text treated as a URL.
const MAX_URL_LEN: usize = 8 * 1024;

/// Schemes besides `http(s)`, `mailto` and `file` that make a link.
const LINK_SCHEMES: &[&str] = &[
    "tel", "sms", "xmpp", "irc", "ircs", "ftp", "sftp", "ssh", "news", "geo", "magnet", "obsidian",
];

/// The clipboard text as a URL: a single token that classifies as a
/// linkable URL (`http(s)` with a host, `mailto:` with an address, a local
/// `file:///` URL, or one of a few known schemes). Script URLs, `data:`
/// URLs, network paths (UNC, `file://host`) and dangerous OS handlers are
/// never URLs here.
#[must_use]
pub fn clipboard_url(text: &str) -> Option<&str> {
    let t = text.trim();
    if t.is_empty()
        || t.len() > MAX_URL_LEN
        || t.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let ok = match classify(t) {
        // Not protocol-relative `//host`: that is only a URL on a page.
        Dest::Web(_) => t.split_once("://").is_some_and(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        }),
        Dest::Mailto => t.contains('@'),
        Dest::LocalFile => t
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("file://")),
        Dest::OtherScheme => t.split_once(':').is_some_and(|(scheme, rest)| {
            LINK_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) && !rest.is_empty()
        }),
        _ => false,
    };
    ok.then_some(t)
}

/// Smart paste of plain text over `selection_text`.
///
/// A URL pasted over a non-empty selection gives `[selection](url)` (the
/// destination in `<…>` form when it needs it; white space around the
/// selection stays outside the brackets). Otherwise `None`: the caller
/// pastes the text as it is. The selection is Markdown source and is kept
/// as is, except that unbalanced brackets are escaped so the link text
/// stays one piece.
///
/// `None` also when the selection spans paragraphs or would cross block
/// markers, and when the selection is itself a URL (pasting replaces it).
/// The profile is accepted for profile-specific link styles; all built-in
/// profiles use the same inline link syntax.
#[must_use]
pub fn smart_paste_text(
    clipboard_text: &str,
    selection_text: &str,
    profile: Profile,
) -> Option<String> {
    let _ = profile;
    let url = clipboard_url(clipboard_text)?;
    let core = selection_text.trim();
    if core.is_empty() || clipboard_url(core).is_some() {
        return None;
    }
    for line in core.lines().skip(1) {
        let l = line.trim_start();
        let digits = l.bytes().take_while(u8::is_ascii_digit).count();
        let block_start = l.is_empty()
            || l.starts_with(['-', '*', '+', '>', '#', '|', '`', '~'])
            || (digits > 0 && l[digits..].starts_with(['.', ')']));
        if block_start {
            return None;
        }
    }
    let lead = &selection_text[..selection_text.len() - selection_text.trim_start().len()];
    let trail = &selection_text[selection_text.trim_end().len()..];
    Some(format!(
        "{lead}[{}]({}){trail}",
        balance_brackets(core),
        destination_for_paste(url)
    ))
}

/// The URL alone as an autolink `<url>` (for the "wrap pasted URLs"
/// setting), or `None` if the text is not a URL or cannot be one.
#[must_use]
pub fn url_autolink(clipboard_text: &str) -> Option<String> {
    let url = clipboard_url(clipboard_text)?;
    if url.contains(['<', '>']) {
        return None;
    }
    Some(format!("<{url}>"))
}

/// Escapes `[` and `]` that have no partner, outside code spans and
/// existing escapes.
fn balance_brackets(s: &str) -> String {
    let b = s.as_bytes();
    let mut unmatched: Vec<usize> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'`' => {
                let n = b[i..].iter().take_while(|&&c| c == b'`').count();
                let fence = &s[i..i + n];
                // Skip to the matching run, if any.
                let mut j = i + n;
                let mut found = None;
                while let Some(k) = s[j..].find(fence) {
                    let at = j + k;
                    let run = b[at..].iter().take_while(|&&c| c == b'`').count();
                    if run == n {
                        found = Some(at + n);
                        break;
                    }
                    j = at + run;
                }
                if let Some(end) = found {
                    i = end;
                    continue;
                }
                i += n;
                continue;
            }
            b'[' => open.push(i),
            b']' if open.pop().is_none() => unmatched.push(i),
            _ => {}
        }
        i += 1;
    }
    unmatched.extend(open);
    unmatched.sort_unstable();
    let mut out = String::with_capacity(s.len() + unmatched.len());
    let mut last = 0;
    for at in unmatched {
        out.push_str(&s[last..at]);
        out.push('\\');
        last = at;
    }
    out.push_str(&s[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Profile = Profile::GfmFile;

    #[test]
    fn url_over_selection() {
        assert_eq!(
            smart_paste_text("https://example.com/x", "docs", P).as_deref(),
            Some("[docs](https://example.com/x)")
        );
        assert_eq!(
            smart_paste_text("  https://example.com/a_(b)\n", " the docs ", P).as_deref(),
            Some(" [the docs](https://example.com/a_(b)) ")
        );
        assert_eq!(
            smart_paste_text("https://en.wikipedia.org/wiki/Foo)", "x", P).as_deref(),
            Some("[x](<https://en.wikipedia.org/wiki/Foo)>)")
        );
        assert_eq!(
            smart_paste_text("mailto:a@b.org", "mail", P).as_deref(),
            Some("[mail](mailto:a@b.org)")
        );
        assert_eq!(
            smart_paste_text("https://x.org", "a [b", P).as_deref(),
            Some("[a \\[b](https://x.org)")
        );
        assert_eq!(
            smart_paste_text("https://x.org", "`a]` and [c]", P).as_deref(),
            Some("[`a]` and [c]](https://x.org)")
        );
    }

    #[test]
    fn not_a_link() {
        // No selection, no URL, a URL selection, block-spanning selections.
        assert_eq!(smart_paste_text("https://x.org", "", P), None);
        assert_eq!(smart_paste_text("https://x.org", "  \n", P), None);
        assert_eq!(smart_paste_text("not a url", "x", P), None);
        assert_eq!(smart_paste_text("note:x", "x", P), None);
        assert_eq!(smart_paste_text("https://x.org", "https://y.org", P), None);
        assert_eq!(smart_paste_text("https://x.org", "a\n\nb", P), None);
        assert_eq!(smart_paste_text("https://x.org", "a\n- b", P), None);
        assert_eq!(
            smart_paste_text("https://x.org", "soft\nwrapped", P).as_deref(),
            Some("[soft\nwrapped](https://x.org)")
        );
    }

    #[test]
    fn unsafe_urls_are_not_links() {
        for u in [
            "javascript:alert(1)",
            "JAVASCRIPT:alert(1)",
            "vbscript:x",
            "data:text/html,<script>",
            "\\\\host\\share",
            "file://host/share",
            "file:////host/share",
            "smb://host/share",
            "ms-msdt:/id",
        ] {
            assert_eq!(smart_paste_text(u, "x", P), None, "{u:?}");
        }
        // Protocol-relative URLs are only URLs relative to a page.
        assert_eq!(smart_paste_text("//host/x", "x", P), None);
    }

    #[test]
    fn autolink() {
        assert_eq!(
            url_autolink("https://x.org/a").as_deref(),
            Some("<https://x.org/a>")
        );
        assert_eq!(url_autolink("hello"), None);
    }
}
