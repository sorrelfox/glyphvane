// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Smart paste in the Source view (07 § Paste and drop).
//!
//! `editor.paste` with a single caret, in this order:
//!
//! 1. **A URL over a selection** becomes `[selection](url)`
//!    ([`gv_html::smart_paste_text`]).
//! 2. **HTML** on the clipboard is converted to Markdown for the tab's
//!    profile ([`gv_html::html_to_markdown`]); relative links resolve
//!    against the page it came from when the platform names it, and are
//!    left as they are otherwise (the document's folder is never a base).
//!    A clipboard whose text is a single URL pastes that URL instead: a
//!    browser's *Copy link* puts the link's HTML beside it.
//! 3. Otherwise the clipboard's **text**, as it is.
//!
//! With several carets the paste is the plain one (one piece per caret
//! when the clipboard splits into as many). `editor.paste_plain` always
//! pastes the text as it is. Every paste is one undo step.
//!
//! # Where the HTML comes from
//!
//! GPUI's clipboard item holds text, images and file paths only, on every
//! platform (`gpui-pre` 0.3.12: `ClipboardEntry::{String, Image,
//! ExternalPaths}`; its X11 backend knows the `text/html` target but does
//! not read it). The HTML flavor therefore comes from a reader the
//! application installs with [`set_html_clipboard`] (the platform
//! clipboard through `arboard`, per the plan). Without one, smart paste
//! does steps 1 and 3 only.

use std::rc::Rc;

use gpui::{App, Global};
use gv_html::{PasteOptions, Profile, clipboard_url, html_to_markdown, smart_paste_text};

/// The HTML flavor of the system clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardHtml {
    /// The HTML (a fragment, a whole document, or Windows' CF_HTML with
    /// its header).
    pub html: String,
    /// The page the HTML was copied from, when the platform says (macOS
    /// `public.url`, Windows CF_HTML `SourceURL`, `text/x-moz-url`).
    pub source_url: Option<String>,
}

/// Reads the clipboard's HTML, which GPUI's clipboard does not carry.
pub trait HtmlClipboard {
    /// The HTML on the clipboard now, if any.
    fn read_html(&self) -> Option<ClipboardHtml>;
}

/// The reader [`set_html_clipboard`] installed.
struct InstalledReader(Rc<dyn HtmlClipboard>);

impl Global for InstalledReader {}

/// Installs the reader of the clipboard's HTML (or removes it, with
/// `None`).
pub fn set_html_clipboard(cx: &mut App, reader: Option<Rc<dyn HtmlClipboard>>) {
    match reader {
        Some(reader) => cx.set_global(InstalledReader(reader)),
        None => {
            if cx.has_global::<InstalledReader>() {
                cx.remove_global::<InstalledReader>();
            }
        }
    }
}

/// The clipboard's HTML, from the installed reader.
pub(super) fn clipboard_html(cx: &App) -> Option<ClipboardHtml> {
    let reader = Rc::clone(&cx.try_global::<InstalledReader>()?.0);
    reader.read_html().filter(|h| !h.html.trim().is_empty())
}

/// What a smart paste inserts in place of the clipboard's text: a link
/// for a URL over `selection`, or the Markdown of `html`. `None` pastes
/// the text as it is.
pub(super) fn smart_text(
    text: Option<&str>,
    html: Option<&ClipboardHtml>,
    selection: &str,
    profile: Profile,
) -> Option<String> {
    if let Some(text) = text {
        if !selection.is_empty()
            && let Some(link) = smart_paste_text(text, selection, profile)
        {
            return Some(link);
        }
        if clipboard_url(text).is_some() {
            return None;
        }
    }
    let html = html?;
    let base = html
        .source_url
        .as_deref()
        .or_else(|| cf_html_source_url(&html.html));
    let opts = PasteOptions::new(profile).with_base_url(base);
    let markdown = html_to_markdown(&html.html, opts);
    (!markdown.trim().is_empty()).then_some(markdown)
}

/// The `SourceURL` of Windows' CF_HTML header, if `html` has one.
fn cf_html_source_url(html: &str) -> Option<&str> {
    if !html.starts_with("Version:") {
        return None;
    }
    html.lines()
        .take_while(|line| !line.trim_start().starts_with('<'))
        .find_map(|line| line.strip_prefix("SourceURL:"))
        .map(str::trim)
        .filter(|url| !url.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Profile = Profile::GfmFile;

    fn html(html: &str) -> ClipboardHtml {
        ClipboardHtml {
            html: html.to_owned(),
            source_url: None,
        }
    }

    #[test]
    fn a_url_over_a_selection_wins_over_html() {
        let h = html("<a href=\"https://x.org\">https://x.org</a>");
        assert_eq!(
            smart_text(Some("https://x.org"), Some(&h), "docs", P).as_deref(),
            Some("[docs](https://x.org)")
        );
        // Without a selection the URL is pasted as it is, not its HTML.
        assert_eq!(smart_text(Some("https://x.org"), Some(&h), "", P), None);
    }

    #[test]
    fn html_becomes_markdown() {
        let h = html("<p><b>bold</b> and <a href=\"https://x.org/a\">link</a></p>");
        assert_eq!(
            smart_text(Some("bold and link"), Some(&h), "", P).as_deref(),
            Some("**bold** and [link](https://x.org/a)")
        );
        // HTML with nothing to keep falls back to the text.
        assert_eq!(
            smart_text(Some("x"), Some(&html("<script>x</script>")), "", P),
            None
        );
        assert_eq!(smart_text(Some("plain"), None, "", P), None);
    }

    #[test]
    fn relative_links_resolve_against_the_source_page() {
        let mut h = html("<a href=\"/docs\">docs</a>");
        h.source_url = Some("https://example.com/a/b".to_owned());
        assert_eq!(
            smart_text(None, Some(&h), "", P).as_deref(),
            Some("[docs](https://example.com/docs)")
        );
        let cf = "Version:0.9\r\nStartHTML:0\r\nSourceURL:https://example.com/x\r\n\
                  <html><body><!--StartFragment--><a href=\"y\">y</a><!--EndFragment--></body></html>";
        assert_eq!(cf_html_source_url(cf), Some("https://example.com/x"));
        assert_eq!(cf_html_source_url("<p>x</p>"), None);
    }
}
