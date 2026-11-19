// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Media cards in the Reading view (FR-RENDER-07..09): audio, video and
//! PDF embeds (`![[x.mp3]]`, `![[x.mp4]]`, `![[x.pdf#page=3]]`) and the
//! `<audio>`, `<video>` and `<iframe>` HTML blocks, all live blocks of a
//! media kind ([`LiveKind::is_media`]).
//!
//! An audio file of the vault gets a player ([`crate::audio`]) under the
//! card's title line, created when the block is first drawn and kept with
//! it; the file is read only on the first play or seek, so a note with
//! many embeds decodes none of them up front. A file the player cannot
//! play falls back to the card.
//!
//! A PDF of the vault shows the page its `#page=N` names (the first
//! without one) under the card's title line, [`PDF_EMBED_HEIGHT`] px high
//! or as `#height=N` says, drawn by the PDF engine the application
//! installs (the helper process, as in the PDF viewer); the card shows
//! while the page is drawn, and stays, saying why, when it cannot be. A
//! click opens the PDF viewer at that page. Nothing else is played,
//! rasterized or loaded. A card says what the file or page is and offers
//! to open it, through the same link handling as a click on a link (the
//! window asks before opening a file with its default app, and hands web
//! addresses to the browser under the external hand-off rules).
//!
//! | Source | Title | Line | Action |
//! |---|---|---|---|
//! | A file in the vault | its name | "MP3 audio · 3.4 MB", "MP4 video · 12 MB", "PDF · page 3 · 1.2 MB" | *Open externally*, *Open in PDF viewer* |
//! | A web address (`<audio>`, `<video>`) | the file name in its path, else the host | "Audio from host, not loaded." | *Open in browser* |
//! | An `<iframe>` | its `title`, else the host | "Embedded page from host, not loaded." (YouTube and Vimeo players name the video) | *Open in browser* |
//!
//! A YouTube or Vimeo player address opens as the video's own page.
//! Network paths (UNC, `file://host`) are never touched, not even to read
//! a size: the card says they are blocked.
//!
//! Not designed: built with the live block's box, header and line
//! (flagged), the PDF page centred in the box. The card's poster comes
//! with its engine. A PDF embed shows one page: there is no page
//! navigation inside the embed (the PDF viewer has it).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui::{AppContext as _, Context, RenderImage};

use crate::audio::{AudioPlayerEvent, AudioPlayerView};
use crate::metrics::size::icon_button;
use gv_render_tree::{LiveBlock, LiveKind};
use gv_vault::url::UrlClass;
use gv_vault::{Status, TargetKind};

use super::embeds::Shown;
use super::links::{Resolver, resolver_kind};
use super::{Disposition, LinkRequest, Reader};

/// The height of a PDF embed's page area without `#height=N`, px.
pub(crate) const PDF_EMBED_HEIGHT: f32 = 480.0;

/// The tallest page area `#height=N` may ask for, px.
const PDF_EMBED_MAX_HEIGHT: f32 = 4000.0;

/// The widest page image drawn for an embed, device pixels.
const PDF_EMBED_WIDTH: u32 = 1600;

/// A PDF embed's page, drawn.
pub(crate) struct PdfPageShown {
    pub card: Arc<MediaCard>,
    /// The file, its length and the page (from 0) drawn, to keep it when
    /// nothing changed.
    pub path: PathBuf,
    pub len: u64,
    pub page: u32,
    /// The page area's height at 100 %, px.
    pub height: f32,
    pub image: Arc<RenderImage>,
    /// The image's size, pixels.
    pub size: (u32, u32),
}

impl std::fmt::Debug for PdfPageShown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfPageShown")
            .field("title", &self.card.title)
            .field("page", &self.page)
            .field("height", &self.height)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// The height of a PDF embed's page area for `anchor`, px at 100 %.
pub(crate) fn pdf_embed_height(anchor: PdfAnchor) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    anchor
        .height
        .map_or(PDF_EMBED_HEIGHT, |h| (h as f32).min(PDF_EMBED_MAX_HEIGHT))
}

/// The height of an audio card's player row, px (its 24 px buttons, its
/// 2 px padding above and below and its hairline focus ring).
pub(crate) const PLAYER_HEIGHT: f32 = icon_button::SQUARE + 6.0;

/// What a media card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MediaCard {
    /// The header's text.
    pub title: String,
    /// The line under the header.
    pub detail: String,
    /// The action on the header's right.
    pub action: &'static str,
    /// What a click opens: the destination and the kind of link it is
    /// followed as.
    pub open: Option<(Arc<str>, gv_model::LinkKind)>,
    /// A PDF embed whose page could not be drawn: the file, its length and
    /// the page (from 0), so that a refresh with nothing changed does not
    /// ask for it again.
    pub failed_page: Option<(PathBuf, u64, u32)>,
}

impl MediaCard {
    /// The link a click on the card follows.
    pub fn request(&self, disposition: Disposition) -> Option<LinkRequest> {
        let (destination, kind) = self.open.clone()?;
        Some(LinkRequest {
            destination,
            kind,
            disposition,
            range: None,
            from: None,
            at: None,
        })
    }
}

/// The `#page=N` and `#height=N` of a PDF embed (Obsidian's anchors,
/// also joined with `&`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PdfAnchor {
    pub page: Option<u32>,
    pub height: Option<u32>,
}

/// The PDF anchor in `anchor` (what follows `#`).
pub(crate) fn pdf_anchor(anchor: Option<&str>) -> PdfAnchor {
    let mut out = PdfAnchor::default();
    for part in anchor.unwrap_or("").split(['&', '#']) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let value = value.trim().parse::<u32>().ok().filter(|v| *v > 0);
        match key.trim().to_ascii_lowercase().as_str() {
            "page" => out.page = value,
            "height" => out.height = value,
            _ => {}
        }
    }
    out
}

/// `n` bytes for people: "512 bytes", "3.4 KB", "12 MB".
pub(crate) fn file_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1000 {
        return if n == 1 {
            "1 byte".to_owned()
        } else {
            format!("{n} bytes")
        };
    }
    #[allow(clippy::cast_precision_loss)]
    let mut v = n as f64 / 1000.0;
    let mut unit = 0;
    while v >= 999.95 && unit + 1 < UNITS.len() {
        v /= 1000.0;
        unit += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[unit])
    } else {
        format!("{v:.0} {}", UNITS[unit])
    }
}

/// The extension of `name` in capitals ("MP3"), if it has one.
fn format_name(name: &str) -> Option<String> {
    let ext = name.rsplit_once('.')?.1;
    (!ext.is_empty() && ext.len() <= 5).then(|| ext.to_ascii_uppercase())
}

/// The last path segment of `path` (`a/b/Talk.mp3` → `Talk.mp3`).
fn file_name(path: &str) -> &str {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

/// The card of a file of the vault named `name`, `size` bytes long.
pub(crate) fn local_card(
    kind: LiveKind,
    name: &str,
    size: Option<u64>,
    anchor: Option<&str>,
    open: (Arc<str>, gv_model::LinkKind),
) -> MediaCard {
    let format = format_name(name);
    let mut parts = Vec::new();
    match kind {
        LiveKind::Audio => parts.push(format.map_or("Audio".to_owned(), |f| format!("{f} audio"))),
        LiveKind::Video => parts.push(format.map_or("Video".to_owned(), |f| format!("{f} video"))),
        LiveKind::Pdf => {
            parts.push("PDF".to_owned());
            if let Some(page) = pdf_anchor(anchor).page {
                parts.push(format!("page {page}"));
            }
        }
        _ => parts.push("Embedded page".to_owned()),
    }
    if let Some(size) = size {
        parts.push(file_size(size));
    }
    MediaCard {
        title: name.to_owned(),
        detail: parts.join(" \u{b7} "),
        action: if kind == LiveKind::Pdf {
            "Open in PDF viewer"
        } else {
            "Open externally"
        },
        open: Some(match pdf_anchor(anchor).page {
            // The PDF viewer opens at the page.
            Some(page) if kind == LiveKind::Pdf => {
                (format!("{}#page={page}", open.0).into(), open.1)
            }
            _ => open,
        }),
        failed_page: None,
    }
}

/// The page of the video a YouTube or Vimeo player address shows, if it
/// is one: `youtube.com/embed/ID` (also `youtube-nocookie.com`) →
/// `https://www.youtube.com/watch?v=ID`, `player.vimeo.com/video/ID` →
/// `https://vimeo.com/ID`.
pub(crate) fn video_page(url: &str, host: &str) -> Option<(String, &'static str)> {
    let host = host.trim_start_matches("www.").to_ascii_lowercase();
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.split_once('/').map_or("", |(_, p)| p);
    let path = path.split(['?', '#']).next().unwrap_or("");
    let id_ok = |id: &str| {
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if matches!(
        host.as_str(),
        "youtube.com" | "youtube-nocookie.com" | "m.youtube.com"
    ) && let Some(id) = path.strip_prefix("embed/")
        && id_ok(id)
    {
        return Some((format!("https://www.youtube.com/watch?v={id}"), "YouTube"));
    }
    if host == "player.vimeo.com"
        && let Some(id) = path.strip_prefix("video/")
        && !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
    {
        return Some((format!("https://vimeo.com/{id}"), "Vimeo"));
    }
    None
}

/// The card of a web address `url` (host `host`), never loaded.
pub(crate) fn web_card(kind: LiveKind, url: &str, host: &str, title: Option<&str>) -> MediaCard {
    let page = video_page(url, host);
    let open = page.as_ref().map_or(url, |(p, _)| p.as_str());
    let name = file_name(url);
    let named = title.map(str::to_owned).or_else(|| {
        (kind != LiveKind::Frame && name.contains('.') && !name.is_empty()).then(|| name.to_owned())
    });
    let detail = match (kind, &page) {
        (LiveKind::Frame, Some((_, site))) => format!("{site} video, not loaded."),
        (LiveKind::Audio, _) => format!("Audio from {host}, not loaded."),
        (LiveKind::Video, _) => format!("Video from {host}, not loaded."),
        (LiveKind::Pdf, _) => format!("PDF from {host}, not loaded."),
        _ => format!("Embedded page from {host}, not loaded."),
    };
    MediaCard {
        title: named.unwrap_or_else(|| host.to_owned()),
        detail,
        action: "Open in browser",
        open: Some((open.into(), gv_model::LinkKind::Inline)),
        failed_page: None,
    }
}

/// What a media block's target is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MediaSource {
    /// A web address: shown as a card at once, never fetched.
    Web(MediaCard),
    /// A file of the vault, to read the size of off the UI thread.
    File {
        path: PathBuf,
        name: String,
        open: (Arc<str>, gv_model::LinkKind),
    },
    /// It cannot be shown; why.
    Notice(Arc<str>),
    /// The index is still building: asked again when it is complete.
    Pending,
}

/// Where the target of the media block `block` is, resolved by
/// `resolver` (`building`: the vault index is not complete yet).
pub(crate) fn media_source(block: &LiveBlock, resolver: &Resolver, building: bool) -> MediaSource {
    let Some(target) = block.target.as_deref() else {
        return MediaSource::Notice("Nothing to show.".into());
    };
    let title = block.html.as_ref().and_then(|h| h.title.as_deref());
    let html = block.html.is_some();
    if html {
        match gv_vault::url::classify(target) {
            UrlClass::Web { url, host, .. } => {
                return MediaSource::Web(web_card(block.kind, &url, &host, title));
            }
            UrlClass::Relative(_) => {}
            UrlClass::Network { .. } => {
                return MediaSource::Notice(
                    format!("\u{201c}{target}\u{201d} is on a network share and is blocked.")
                        .into(),
                );
            }
            _ => {
                return MediaSource::Notice(
                    format!("\u{201c}{target}\u{201d} cannot be shown here.").into(),
                );
            }
        }
    }
    let res = if html {
        // Inside a note embed: from the embedded note.
        resolver.resolve_from(
            resolver_kind(gv_model::LinkKind::Inline, true),
            target,
            block.host.as_deref(),
        )
    } else {
        resolver.resolve_embed(target, block.host.as_deref())
    };
    if res.status == Status::Pending && building {
        return MediaSource::Pending;
    }
    let found = res
        .target_path
        .as_ref()
        .filter(|_| res.status.is_resolved())
        .filter(|_| res.target_kind == Some(TargetKind::File));
    let Some(found) = found else {
        return MediaSource::Notice(super::embeds::target_notice(target, res.status));
    };
    let link_kind = if html {
        gv_model::LinkKind::Inline
    } else {
        gv_model::LinkKind::Wikilink
    };
    MediaSource::File {
        path: resolver.fs_path(found),
        name: file_name(found.as_str()).to_owned(),
        open: (target.into(), link_kind),
    }
}

impl Reader {
    /// Shows the card of media block `block`: at once for a web address,
    /// after reading the file's size for a file of the vault.
    pub(super) fn fetch_media(
        &mut self,
        block: &Arc<LiveBlock>,
        resolver: &Resolver,
        cx: &mut Context<'_, Self>,
    ) {
        let building = self.index.as_ref().is_some_and(|i| !i.is_complete());
        let key = block.key;
        let (path, name, open) = match media_source(block, resolver, building) {
            MediaSource::Web(card) => {
                self.land(key, Shown::Media(Arc::new(card)), cx);
                return;
            }
            MediaSource::Notice(why) => {
                self.land(key, Shown::Notice(why), cx);
                return;
            }
            MediaSource::Pending => {
                self.land(key, Shown::Pending, cx);
                return;
            }
            MediaSource::File { path, name, open } => (path, name, open),
        };
        let revision = self.state.borrow().revision;
        let kind = block.kind;
        let anchor = block.anchor.clone();
        // A player already shown for the same file stays, and keeps
        // playing, across a refresh.
        if kind == LiveKind::Audio {
            let mut st = self.state.borrow_mut();
            let same = matches!(
                st.cache.live.get(key),
                Some(Shown::Audio(_, player)) if player.read(cx).path() == path
            );
            if same {
                st.cache.live.unchanged(key);
                return;
            }
        }
        let path_for_player = path.clone();
        cx.spawn(async move |reader, cx| {
            let size = cx.background_spawn(async move { size_of(&path) }).await;
            reader
                .update(cx, |reader, cx| {
                    if reader.state.borrow().revision != revision {
                        reader.state.borrow_mut().cache.live.unchanged(key);
                        return;
                    }
                    let (card, len) = match size {
                        Ok(size) => (
                            Arc::new(local_card(kind, &name, Some(size), anchor.as_deref(), open)),
                            size,
                        ),
                        Err(why) => {
                            reader.land(key, Shown::Notice(why.into()), cx);
                            return;
                        }
                    };
                    if kind == LiveKind::Audio {
                        reader.show_player(key, card, path_for_player, cx);
                    } else if kind == LiveKind::Pdf {
                        let anchor = pdf_anchor(anchor.as_deref());
                        reader.show_pdf_page(key, card, path_for_player, len, anchor, revision, cx);
                    } else {
                        reader.land(key, Shown::Media(card), cx);
                    }
                })
                .ok();
        })
        .detach();
    }
}

impl Reader {
    /// Shows the page `anchor` names of the PDF at `path` (`len` bytes)
    /// under `card`: the card at once, the page once the engine has drawn
    /// it, or the card saying why it could not. A page already shown for
    /// the same file, page and height stays, and so does the card of a page
    /// that could not be drawn from the same file.
    #[allow(clippy::too_many_arguments)]
    fn show_pdf_page(
        &mut self,
        key: gv_render_tree::LiveKey,
        card: Arc<MediaCard>,
        path: PathBuf,
        len: u64,
        anchor: PdfAnchor,
        revision: u64,
        cx: &mut Context<'_, Self>,
    ) {
        let page = anchor.page.unwrap_or(1).saturating_sub(1);
        let height = pdf_embed_height(anchor);
        {
            let mut st = self.state.borrow_mut();
            let same = matches!(
                st.cache.live.get(key),
                Some(Shown::PdfPage(p)) if p.path == path
                    && p.len == len
                    && p.page == page
                    && (p.height - height).abs() < 0.5
            ) || matches!(
                st.cache.live.get(key),
                Some(Shown::Media(c)) if c.failed_page.as_ref().is_some_and(
                    |(p, l, n)| *p == path && *l == len && *n == page
                )
            );
            if same {
                st.cache.live.unchanged(key);
                return;
            }
        }
        let Some(engine) = crate::export::pdf_engine(cx) else {
            self.land(key, Shown::Media(card), cx);
            return;
        };
        self.land(key, Shown::Media(Arc::clone(&card)), cx);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let max_height = ((height * 2.0) as u32).clamp(64, gv_helper_max_side());
        let file = path.clone();
        cx.spawn(async move |reader, cx| {
            let drawn = cx
                .background_spawn(async move {
                    let bytes = read_pdf(&file, len)?;
                    let raster = engine
                        .page(
                            &bytes,
                            page,
                            PDF_EMBED_WIDTH,
                            max_height,
                            &AtomicBool::new(false),
                        )
                        .map_err(|e| e.to_string())?;
                    crate::export::dialog::raster_image(raster)
                        .ok_or_else(|| "the page image is empty".to_owned())
                })
                .await;
            reader
                .update(cx, |reader, cx| {
                    if reader.state.borrow().revision != revision {
                        return;
                    }
                    match drawn {
                        Ok((image, w, h)) => reader.land(
                            key,
                            Shown::PdfPage(Arc::new(PdfPageShown {
                                card,
                                path,
                                len,
                                page,
                                height,
                                image,
                                size: (w, h),
                            })),
                            cx,
                        ),
                        Err(why) => {
                            let mut card = (*card).clone();
                            card.detail = format!("{} \u{b7} {why}", card.detail);
                            card.failed_page = Some((path, len, page));
                            reader.land(key, Shown::Media(Arc::new(card)), cx);
                        }
                    }
                })
                .ok();
        })
        .detach();
    }

    /// Shows `card` with a player of `path` (lazy: nothing is read until
    /// the first play); a file it cannot play goes back to the card.
    fn show_player(
        &mut self,
        key: gv_render_tree::LiveKey,
        card: Arc<MediaCard>,
        path: PathBuf,
        cx: &mut Context<'_, Self>,
    ) {
        let player = cx.new(|cx| AudioPlayerView::lazy(path, cx));
        let fallback = Arc::clone(&card);
        let events = cx.subscribe(&player, move |reader, _, event: &AudioPlayerEvent, cx| {
            let AudioPlayerEvent::Failed(error) = event;
            let mut card = (*fallback).clone();
            card.detail = format!("{} \u{b7} {}", card.detail, error.message());
            reader.land(key, Shown::Media(Arc::new(card)), cx);
        });
        self.land(key, Shown::Audio(card, player), cx);
        self.state
            .borrow_mut()
            .cache
            .live
            .keep(key, Some(events), None);
    }
}

/// The largest side of a page drawn to fit a box (the helper's cap).
const fn gv_helper_max_side() -> u32 {
    4096
}

/// The largest PDF an embed draws a page of, in bytes.
const PDF_EMBED_MAX_BYTES: u64 = crate::file_viewer::PDF_MAX_BYTES;

/// The bytes of the PDF at `path`, `len` long (blocking: the background
/// executor).
fn read_pdf(path: &Path, len: u64) -> Result<Vec<u8>, String> {
    if len > PDF_EMBED_MAX_BYTES {
        return Err("too large to draw".to_owned());
    }
    std::fs::read(path).map_err(|e| format!("cannot read the file: {e}"))
}

/// The size of the file at `path` (blocking: the background executor).
pub(crate) fn size_of(path: &Path) -> Result<u64, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("Cannot read the file: {e}"))?;
    if !meta.is_file() {
        return Err("Not a file.".to_owned());
    }
    Ok(meta.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_anchors_give_page_and_height() {
        assert_eq!(
            pdf_anchor(Some("page=3")),
            PdfAnchor {
                page: Some(3),
                height: None
            }
        );
        assert_eq!(
            pdf_anchor(Some("page=2&height=400")),
            PdfAnchor {
                page: Some(2),
                height: Some(400)
            }
        );
        assert_eq!(pdf_anchor(Some("height=300")).height, Some(300));
        assert_eq!(pdf_anchor(Some("page=0")), PdfAnchor::default());
        assert_eq!(pdf_anchor(Some("Heading")), PdfAnchor::default());
        assert_eq!(pdf_anchor(None), PdfAnchor::default());
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(file_size(0), "0 bytes");
        assert_eq!(file_size(1), "1 byte");
        assert_eq!(file_size(999), "999 bytes");
        assert_eq!(file_size(3_400), "3.4 KB");
        assert_eq!(file_size(12_000_000), "12 MB");
        assert_eq!(file_size(999_999), "1.0 MB");
        assert_eq!(file_size(2_500_000_000), "2.5 GB");
    }

    #[test]
    fn local_cards_name_the_format_size_and_page() {
        let open =
            || -> (Arc<str>, gv_model::LinkKind) { ("x".into(), gv_model::LinkKind::Wikilink) };
        let a = local_card(LiveKind::Audio, "Talk.mp3", Some(3_400_000), None, open());
        assert_eq!(a.title, "Talk.mp3");
        assert_eq!(a.detail, "MP3 audio \u{b7} 3.4 MB");
        assert_eq!(a.action, "Open externally");
        let p = local_card(
            LiveKind::Pdf,
            "Paper.pdf",
            Some(1_200),
            Some("page=3"),
            open(),
        );
        assert_eq!(p.detail, "PDF \u{b7} page 3 \u{b7} 1.2 KB");
        assert_eq!(p.action, "Open in PDF viewer");
        let v = local_card(LiveKind::Video, "clip", None, None, open());
        assert_eq!(v.detail, "Video");
    }

    #[test]
    fn player_addresses_open_the_video_page() {
        assert_eq!(
            video_page(
                "https://www.youtube.com/embed/dQw4w9WgXcQ?start=3",
                "www.youtube.com"
            ),
            Some((
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_owned(),
                "YouTube"
            ))
        );
        assert_eq!(
            video_page(
                "https://www.youtube-nocookie.com/embed/abc_D-1",
                "www.youtube-nocookie.com"
            )
            .map(|p| p.0),
            Some("https://www.youtube.com/watch?v=abc_D-1".to_owned())
        );
        assert_eq!(
            video_page(
                "https://player.vimeo.com/video/76979871",
                "player.vimeo.com"
            ),
            Some(("https://vimeo.com/76979871".to_owned(), "Vimeo"))
        );
        assert_eq!(
            video_page("https://www.youtube.com/watch?v=x", "www.youtube.com"),
            None
        );
        assert_eq!(
            video_page("https://evil.example/embed/x", "evil.example"),
            None
        );
    }

    #[test]
    fn web_cards_are_never_loaded() {
        let f = web_card(
            LiveKind::Frame,
            "https://www.youtube.com/embed/abc",
            "www.youtube.com",
            Some("A talk"),
        );
        assert_eq!(f.title, "A talk");
        assert_eq!(f.detail, "YouTube video, not loaded.");
        assert_eq!(f.action, "Open in browser");
        assert_eq!(
            f.open,
            Some((
                "https://www.youtube.com/watch?v=abc".into(),
                gv_model::LinkKind::Inline
            ))
        );
        let m = web_card(
            LiveKind::Frame,
            "https://maps.example.org/x",
            "maps.example.org",
            None,
        );
        assert_eq!(m.title, "maps.example.org");
        assert_eq!(m.detail, "Embedded page from maps.example.org, not loaded.");
        let a = web_card(
            LiveKind::Audio,
            "https://cdn.example.com/a/Song.ogg?x=1",
            "cdn.example.com",
            None,
        );
        assert_eq!(a.title, "Song.ogg");
        assert_eq!(a.detail, "Audio from cdn.example.com, not loaded.");
    }
}
