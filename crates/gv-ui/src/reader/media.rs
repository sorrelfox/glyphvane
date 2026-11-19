// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Images in the Reading view.
//!
//! - **Sizes, while the document is built** (off the UI thread): the render
//!   tree asks [`ImageSizes`] for each image. A local image is resolved
//!   like a link (through the vault index, or below the document's folder
//!   without a vault) and its header probed ([`probe`], cached by path,
//!   time and size): the box gets the image's displayed size (EXIF
//!   orientation applied). A local file that does not decode gets a card's
//!   box; a missing file and anything else get the alt text. A remote
//!   image gets a card's box until it is fetched ([`super::remote`]: the
//!   remote-content policy decides), then its own size. What each destination
//!   resolved to is kept with the document ([`ImageTargets`]) so the UI
//!   thread never resolves.
//! - **Pixels, while drawing**: the view computes the cache key of the box
//!   at device pixels ([`MediaKey::for_path`]: one `stat`) and reads its own
//!   cache of GPUI images ([`Images`]). A pending image is a placeholder;
//!   the view starts one background job per key ([`load`]) on the
//!   low-priority pool, which decodes, fits, converts to BGRA and makes the
//!   frames (all of them for animations, unless motion is reduced). A job
//!   is cancelled when its image leaves the viewport. When it lands, the
//!   items drawn meanwhile are laid out again.
//! - A decode error is drawn as an image card over the box: the engine's
//!   message and, for formats another program can show, *Open externally*.
//! - `#gh-dark-mode-only` and `#gh-light-mode-only` images show only in a
//!   dark or a light theme (Semi-light is light).
//!
//! One media engine per allowed root ([`media_engine`]): external SVG
//! references stay inside the vault, or the document's folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use gpui::RenderImage;
use gv_media::{
    CancelFlag, DecodeRequest, DecodedImage, Limits, MediaEngine, MediaError, MediaKey, Probe,
    SvgContext,
};
use gv_render_tree::{ImageSize, ImageSizeSource, ItemId, LinkTarget};
use gv_vault::url::UrlClass;

use super::Reader;
use super::links::Resolver;

/// The box of an image that cannot be decoded: room for its card.
pub(crate) const CARD_WIDTH: f32 = 360.0;
pub(crate) const CARD_HEIGHT: f32 = 64.0;

/// Media engines kept, one per allowed root.
const ENGINES: usize = 4;

/// The media engine of remote images: their SVGs may load no file at all.
/// Making one may scan the system's fonts: only on a background thread.
pub(crate) fn remote_engine() -> Arc<MediaEngine> {
    static ENGINE: OnceLock<Arc<MediaEngine>> = OnceLock::new();
    Arc::clone(ENGINE.get_or_init(|| {
        Arc::new(MediaEngine::new(
            Limits::default(),
            SvgContext {
                fontdb: super::diagram::font_db(),
                allowed_root: None,
            },
        ))
    }))
}

/// The media engine whose SVGs may load files below `root`. Making one
/// may scan the system's fonts: only on a background thread.
pub(crate) fn media_engine(root: &Path) -> Arc<MediaEngine> {
    static ENGINES_BY_ROOT: Mutex<Vec<(PathBuf, Arc<MediaEngine>)>> = Mutex::new(Vec::new());
    let mut engines = ENGINES_BY_ROOT
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(i) = engines.iter().position(|(r, _)| r == root) {
        let e = engines.remove(i);
        let engine = Arc::clone(&e.1);
        engines.push(e);
        return engine;
    }
    let engine = Arc::new(MediaEngine::new(
        Limits::default(),
        SvgContext {
            fontdb: super::diagram::font_db(),
            allowed_root: Some(root.to_path_buf()),
        },
    ));
    if engines.len() >= ENGINES {
        engines.remove(0);
    }
    engines.push((root.to_path_buf(), Arc::clone(&engine)));
    engine
}

/// A file's identity for the probe cache: modification time and size.
type Stamp = (Option<SystemTime>, u64);

/// A probe and the stamp of the file it read.
type Probed = (Stamp, Result<Probe, MediaError>);

/// Probes kept at most (all are forgotten beyond).
const PROBES: usize = 8_192;

/// The header facts of `path` (blocking: reads the file), cached by path,
/// time and size.
pub(crate) fn probe(engine: &MediaEngine, path: &Path) -> Result<Probe, MediaError> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Probed>>> = OnceLock::new();
    let meta = std::fs::metadata(path).map_err(|e| MediaError::Io(e.to_string()))?;
    let stamp = (meta.modified().ok(), meta.len());
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some((s, r)) = cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(path)
        && *s == stamp
    {
        return r.clone();
    }
    let r = engine.probe_path(path);
    let mut c = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if c.len() > PROBES {
        c.clear();
    }
    c.insert(path.to_path_buf(), (stamp, r.clone()));
    r
}

/// What an image destination is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ImageTarget {
    /// A local file: its header facts, or why it cannot be shown.
    Local {
        path: PathBuf,
        probe: Result<Probe, MediaError>,
    },
    /// A remote image not fetched (yet): the URL. The remote-content
    /// policy decides what its card says ([`super::remote`]).
    Remote(String),
    /// A remote image in the cache: its URL, the cached file and its
    /// header facts.
    Fetched {
        url: String,
        path: PathBuf,
        probe: Result<Probe, MediaError>,
    },
    /// A `data:` image: its bytes (at most
    /// [`gv_media::MAX_DATA_IMAGE_BYTES`]), their cache key and header
    /// facts, or why it cannot be shown.
    Data(Result<DataImage, MediaError>),
    /// Missing, outside the root, blocked or not an image URL: the alt text.
    None,
}

/// The decoded payload of a `data:` image URL.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DataImage {
    pub bytes: Arc<[u8]>,
    /// The key of the bytes (hashed once), for any box size.
    pub key: MediaKey,
    pub probe: Result<Probe, MediaError>,
}

impl DataImage {
    /// Decodes and probes `url` (blocking: off the UI thread). SVG and
    /// other types are refused, the bytes are capped.
    pub fn from_url(url: &str) -> Result<DataImage, MediaError> {
        let bytes: Arc<[u8]> =
            gv_media::decode_data_url(url, gv_media::MAX_DATA_IMAGE_BYTES)?.into();
        let key = MediaKey::for_bytes(
            &bytes,
            DecodeRequest {
                fit: None,
                animate: false,
            },
        );
        // The remote engine: no file is ever read for it.
        let probe = remote_engine().probe_bytes(&bytes);
        Ok(DataImage { bytes, key, probe })
    }
}

/// Which scheme an image is for, by its GitHub fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchemeOnly {
    Dark,
    Light,
}

/// The scheme `destination` is shown in only (`#gh-dark-mode-only`,
/// `#gh-light-mode-only`), if any.
pub(crate) fn scheme_only(destination: &str) -> Option<SchemeOnly> {
    let (_, fragment) = destination.rsplit_once('#')?;
    match fragment {
        "gh-dark-mode-only" => Some(SchemeOnly::Dark),
        "gh-light-mode-only" => Some(SchemeOnly::Light),
        _ => None,
    }
}

/// What the images of one document resolved to, by destination: filled
/// while the document is built, read by the view.
#[derive(Debug)]
pub(crate) struct ImageTargets {
    /// The root image files must stay below (the vault, or the document's
    /// folder).
    root: PathBuf,
    map: Mutex<HashMap<String, ImageTarget>>,
}

impl ImageTargets {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            map: Mutex::new(HashMap::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// What `destination` resolved to, if it was asked for.
    pub fn get(&self, destination: &str) -> Option<ImageTarget> {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(destination)
            .cloned()
    }

    /// The remote URLs asked for, with the destinations that name each.
    pub fn remote_urls(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let mut out: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for (dest, target) in self
            .map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            if let ImageTarget::Remote(url) | ImageTarget::Fetched { url, .. } = target {
                out.entry(url.clone()).or_default().push(dest.clone());
            }
        }
        out
    }

    /// Whether the image at `url` is shown from the cache.
    pub fn is_fetched(&self, url: &str) -> bool {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .any(|t| matches!(t, ImageTarget::Fetched { url: u, .. } if u == url))
    }

    /// The destinations that resolved to local file `path`.
    pub fn local_destinations(&self, path: &Path) -> Vec<String> {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, t)| matches!(t, ImageTarget::Local { path: p, .. } if p == path))
            .map(|(d, _)| d.clone())
            .collect()
    }

    /// Sets what `destination` resolves to (a remote image fetched, or
    /// one that may no longer show).
    pub fn set(&self, destination: &str, target: ImageTarget) {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(destination.to_owned(), target);
    }

    /// Resolves `destination` (of the note `from` inside a note embed)
    /// with `resolver` and probes a local file (blocking; off the UI
    /// thread), once per destination. Kept by its
    /// [`image_key`](gv_render_tree::image_key).
    fn resolve(&self, destination: &str, from: Option<&str>, resolver: &Resolver) -> ImageTarget {
        let key = gv_render_tree::image_key(destination, from);
        if let Some(t) = self.get(&key) {
            return t;
        }
        let target = image_target(destination, from, resolver, &self.root);
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.into_owned(), target.clone());
        target
    }
}

/// Resolves an image destination of a document, or of the note `from`
/// inside a note embed (blocking).
pub(crate) fn image_target(
    destination: &str,
    from: Option<&str>,
    resolver: &Resolver,
    root: &Path,
) -> ImageTarget {
    let bare = match scheme_only(destination) {
        Some(_) => destination.rsplit_once('#').map_or(destination, |(d, _)| d),
        None => destination,
    };
    match gv_vault::url::classify(bare) {
        UrlClass::Web { url, .. } => {
            // Fetched before (this run): shown at its size at once; the
            // view checks the policy before anything is drawn.
            return match super::remote::fetched(&url) {
                Some((path, probe)) => ImageTarget::Fetched { url, path, probe },
                None => ImageTarget::Remote(url),
            };
        }
        UrlClass::DataImage { .. } => return ImageTarget::Data(DataImage::from_url(bare)),
        UrlClass::Relative(_) | UrlClass::Fragment(_) | UrlClass::LocalFile { .. } => {}
        _ => return ImageTarget::None,
    }
    let res = resolver.resolve_from(gv_vault::LinkKind::Image, bare, from);
    let path = match (&res.target_path, res.target_kind) {
        (Some(p), Some(gv_vault::TargetKind::File)) if res.status.is_resolved() => {
            p.to_fs_path(resolver.root())
        }
        _ => match res.url {
            Some(UrlClass::LocalFile { path, .. }) => PathBuf::from(path),
            _ => return ImageTarget::None,
        },
    };
    // Only below the root (a `file:` URL can name anything).
    let inside = std::path::absolute(&path)
        .ok()
        .zip(std::path::absolute(root).ok())
        .is_some_and(|(p, r)| p.starts_with(r));
    if !inside || !path.is_file() {
        return ImageTarget::None;
    }
    let probe = probe(&media_engine(root), &path);
    ImageTarget::Local { path, probe }
}

/// The render tree's image sizes for one document build: resolved and
/// probed off the UI thread, recorded in the document's [`ImageTargets`].
pub(crate) struct ImageSizes<'a> {
    pub resolver: Resolver,
    pub targets: &'a ImageTargets,
}

impl ImageSizeSource for ImageSizes<'_> {
    fn size(&self, image: &LinkTarget<'_>) -> ImageSize {
        match self
            .targets
            .resolve(image.destination, image.from, &self.resolver)
        {
            ImageTarget::Local { probe: Ok(p), .. }
            | ImageTarget::Fetched { probe: Ok(p), .. }
            | ImageTarget::Data(Ok(DataImage { probe: Ok(p), .. }))
                if p.width > 0 && p.height > 0 =>
            {
                #[allow(clippy::cast_precision_loss)]
                ImageSize::Known {
                    width: p.width as f32,
                    height: p.height as f32,
                }
            }
            // A file that is there but cannot be shown: its card.
            ImageTarget::Local { probe: Err(e), .. } if !matches!(e, MediaError::Io(_)) => {
                ImageSize::Known {
                    width: CARD_WIDTH,
                    height: CARD_HEIGHT,
                }
            }
            // A remote image not fetched or that cannot be shown, or a
            // `data:` image that cannot: its card.
            ImageTarget::Remote(_) | ImageTarget::Fetched { .. } | ImageTarget::Data(_) => {
                ImageSize::Known {
                    width: CARD_WIDTH,
                    height: CARD_HEIGHT,
                }
            }
            _ => ImageSize::Failed,
        }
    }
}

/// The GPUI image of a decoded image: frames converted to BGRA with their
/// delays (blocking; on the background thread).
pub(crate) fn render_image(decoded: &DecodedImage) -> Option<Arc<RenderImage>> {
    let mut frames = Vec::with_capacity(decoded.frames.len());
    for f in &decoded.frames {
        let mut f = f.clone();
        f.rgba_to_bgra();
        let buffer = image::RgbaImage::from_raw(f.width, f.height, f.rgba)?;
        frames.push(image::Frame::from_parts(
            buffer,
            0,
            0,
            image::Delay::from_numer_denom_ms(f.delay_ms, 1),
        ));
    }
    if frames.is_empty() {
        return None;
    }
    Some(Arc::new(RenderImage::new(frames)))
}

/// A decode to start.
#[derive(Debug, Clone)]
pub(crate) struct ImageRequest {
    pub key: MediaKey,
    pub path: PathBuf,
    pub root: PathBuf,
    pub request: DecodeRequest,
    pub cancel: CancelFlag,
    /// A fetched remote image: decoded without access to any file.
    pub remote: bool,
    /// A `data:` image: these bytes, not `path`, decoded without access
    /// to any file.
    pub bytes: Option<Arc<[u8]>>,
}

/// Decodes a request into a GPUI image (blocking: the background executor
/// only).
pub(crate) fn load(r: &ImageRequest) -> Result<Arc<RenderImage>, MediaError> {
    #[cfg(test)]
    tests::count_job();
    let decoded = if let Some(bytes) = &r.bytes {
        remote_engine().load_bytes(bytes, r.request, &r.cancel)?
    } else if r.remote {
        remote_engine().load_path(&r.path, r.request, &r.cancel)?
    } else {
        media_engine(&r.root).load_path(&r.path, r.request, &r.cancel)?
    };
    if r.cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    render_image(&decoded).ok_or_else(|| MediaError::Malformed("no frames".into()))
}

/// What an image box shows now.
#[derive(Debug, Clone)]
pub(crate) enum ImageShown {
    Pending,
    Image(Arc<RenderImage>),
    Failed(MediaError),
}

#[derive(Debug)]
enum Entry {
    Pending { cancel: CancelFlag, seen: bool },
    Ready { image: Arc<RenderImage>, used: u64 },
    Failed(MediaError),
}

/// Bytes of GPUI images kept (least recently drawn go first).
const IMAGE_BYTES: usize = 256 << 20;

/// Images in one Reading view: decoded GPUI images by key, the jobs, and
/// the items to lay out again when one lands.
#[derive(Debug, Default)]
pub(crate) struct Images {
    entries: HashMap<MediaKey, Entry>,
    waiting: HashMap<MediaKey, Vec<ItemId>>,
    requests: Vec<ImageRequest>,
    bytes: usize,
    tick: u64,
    /// Images evicted, for the window to drop from its atlas.
    evicted: Vec<Arc<RenderImage>>,
    /// The stamp each local image file was first drawn with.
    stamps: HashMap<PathBuf, Stamp>,
    /// Image files whose stamp changed since: their sizes are probed
    /// again ([`crate::reader::Reader::reprobe_images`]).
    changed: Vec<PathBuf>,
    /// When the image files were last checked for changes, and where the
    /// next check starts in `stamps`.
    polled: Option<std::time::Instant>,
    poll_from: usize,
}

/// How often the image files drawn are checked for changes on disk while
/// the view draws (each check `stat`s at most [`POLL_FILES`] of them).
const POLL_EVERY: std::time::Duration = if cfg!(test) {
    std::time::Duration::ZERO
} else {
    std::time::Duration::from_secs(2)
};
const POLL_FILES: usize = 64;

fn image_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count())
        .map(|i| image.as_bytes(i).map_or(0, <[u8]>::len))
        .sum()
}

impl Images {
    /// What the image at `path` (below `root`) shows in a box of
    /// `device` pixels, for item `item`: starts a job when it is not
    /// decoded yet.
    pub fn shown(
        &mut self,
        path: &Path,
        root: &Path,
        request: DecodeRequest,
        item: ItemId,
        remote: bool,
    ) -> ImageShown {
        let key = match MediaKey::for_path(path, request) {
            Ok(k) => k,
            Err(e) => return ImageShown::Failed(e),
        };
        if let Some(stamp) = key.file_stamp() {
            match self.stamps.get_mut(path) {
                Some(old) if *old != stamp => {
                    *old = stamp;
                    if !self.changed.iter().any(|p| p == path) {
                        self.changed.push(path.to_path_buf());
                    }
                }
                Some(_) => {}
                None => {
                    self.stamps.insert(path.to_path_buf(), stamp);
                }
            }
        }
        self.shown_key(key, item, |key, cancel| ImageRequest {
            key,
            path: path.to_path_buf(),
            root: root.to_path_buf(),
            request,
            cancel,
            remote,
            bytes: None,
        })
    }

    /// What `data:` image `data` shows in a box of `request`'s device
    /// pixels, for item `item`: starts a job when it is not decoded yet.
    pub fn shown_data(
        &mut self,
        data: &DataImage,
        request: DecodeRequest,
        item: ItemId,
    ) -> ImageShown {
        let key = data.key.with_request(request);
        self.shown_key(key, item, |key, cancel| ImageRequest {
            key,
            path: PathBuf::new(),
            root: PathBuf::new(),
            request,
            cancel,
            remote: true,
            bytes: Some(Arc::clone(&data.bytes)),
        })
    }

    fn shown_key(
        &mut self,
        key: MediaKey,
        item: ItemId,
        make: impl FnOnce(MediaKey, CancelFlag) -> ImageRequest,
    ) -> ImageShown {
        self.tick += 1;
        match self.entries.get_mut(&key) {
            Some(Entry::Ready { image, used }) => {
                *used = self.tick;
                return ImageShown::Image(Arc::clone(image));
            }
            Some(Entry::Failed(e)) => return ImageShown::Failed(e.clone()),
            Some(Entry::Pending { seen, .. }) => *seen = true,
            None => {
                let cancel = CancelFlag::new();
                self.entries.insert(
                    key.clone(),
                    Entry::Pending {
                        cancel: cancel.clone(),
                        seen: true,
                    },
                );
                self.requests.push(make(key.clone(), cancel));
            }
        }
        let waiting = self.waiting.entry(key).or_default();
        if !waiting.contains(&item) {
            waiting.push(item);
        }
        ImageShown::Pending
    }

    /// The image files that changed on disk since they were first drawn
    /// (checked while drawing, at most every [`POLL_EVERY`]).
    pub fn take_changed_files(&mut self) -> Vec<PathBuf> {
        let due = self.polled.is_none_or(|t| t.elapsed() >= POLL_EVERY);
        if due && !self.stamps.is_empty() {
            self.polled = Some(std::time::Instant::now());
            self.poll_files();
        }
        std::mem::take(&mut self.changed)
    }

    /// `stat`s the next [`POLL_FILES`] image files drawn; those whose
    /// time or size changed are taken as changed.
    fn poll_files(&mut self) {
        let n = self.stamps.len();
        let start = self.poll_from % n;
        let mut seen = Vec::new();
        for (path, stamp) in self.stamps.iter_mut().skip(start).take(POLL_FILES) {
            let now = std::fs::metadata(path)
                .map(|m| (m.modified().ok(), m.len()))
                .ok();
            if let Some(now) = now
                && now != *stamp
            {
                *stamp = now;
                seen.push(path.clone());
            }
        }
        self.poll_from = (start + POLL_FILES) % n.max(1);
        for p in seen {
            if !self.changed.contains(&p) {
                self.changed.push(p);
            }
        }
    }

    /// The jobs to start.
    pub fn take_requests(&mut self) -> Vec<ImageRequest> {
        std::mem::take(&mut self.requests)
    }

    /// A job ended: the items to lay out again.
    pub fn landed(
        &mut self,
        key: &MediaKey,
        result: Result<Arc<RenderImage>, MediaError>,
    ) -> Vec<ItemId> {
        if !matches!(self.entries.get(key), Some(Entry::Pending { .. })) {
            return Vec::new();
        }
        match result {
            Err(MediaError::Cancelled) => {
                self.entries.remove(key);
                return Vec::new();
            }
            Ok(image) => {
                self.tick += 1;
                self.bytes += image_bytes(&image);
                self.entries.insert(
                    key.clone(),
                    Entry::Ready {
                        image,
                        used: self.tick,
                    },
                );
                self.evict();
            }
            Err(e) => {
                self.entries.insert(key.clone(), Entry::Failed(e));
            }
        }
        self.waiting.remove(key).unwrap_or_default()
    }

    fn evict(&mut self) {
        if self.bytes <= IMAGE_BYTES {
            return;
        }
        let mut ready: Vec<(u64, MediaKey)> = self
            .entries
            .iter()
            .filter_map(|(k, e)| match e {
                Entry::Ready { used, .. } => Some((*used, k.clone())),
                _ => None,
            })
            .collect();
        ready.sort_unstable_by_key(|(t, _)| *t);
        for (_, k) in ready {
            if self.bytes <= IMAGE_BYTES / 4 * 3 {
                break;
            }
            if let Some(Entry::Ready { image, .. }) = self.entries.remove(&k) {
                self.bytes -= image_bytes(&image);
                self.evicted.push(image);
            }
        }
    }

    /// The images evicted since the last call (to drop from the atlas).
    pub fn take_evicted(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.evicted)
    }

    /// Starts a frame: no pending image is seen yet.
    pub fn begin_frame(&mut self) {
        for e in self.entries.values_mut() {
            if let Entry::Pending { seen, .. } = e {
                *seen = false;
            }
        }
    }

    /// Ends a frame: the jobs of images not drawn in it are cancelled
    /// (they are asked for again when drawn). Returns the items that
    /// waited for them, whose layouts must be made again when they show.
    pub fn end_frame(&mut self) -> Vec<ItemId> {
        let gone: Vec<MediaKey> = self
            .entries
            .iter()
            .filter_map(|(k, e)| match e {
                Entry::Pending { seen: false, .. } => Some(k.clone()),
                _ => None,
            })
            .collect();
        let mut items = Vec::new();
        for k in gone {
            if let Some(Entry::Pending { cancel, .. }) = self.entries.remove(&k) {
                cancel.cancel();
            }
            items.extend(self.waiting.remove(&k).unwrap_or_default());
        }
        items
    }

    /// Forgets everything (another document is shown), cancelling the jobs.
    pub fn forget(&mut self) {
        for e in self.entries.values() {
            if let Entry::Pending { cancel, .. } = e {
                cancel.cancel();
            }
        }
        let ready: Vec<_> = self
            .entries
            .drain()
            .filter_map(|(_, e)| match e {
                Entry::Ready { image, .. } => Some(image),
                _ => None,
            })
            .collect();
        self.evicted.extend(ready);
        self.waiting.clear();
        self.requests.clear();
        self.bytes = 0;
        self.stamps.clear();
        self.changed.clear();
    }

    /// Images decoded and kept.
    #[cfg(test)]
    pub fn ready(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e, Entry::Ready { .. }))
            .count()
    }

    /// Jobs running.
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e, Entry::Pending { .. }))
            .count()
    }
}

impl Reader {
    /// Image files that changed on disk: probed again off the UI thread;
    /// the images whose size changed get boxes of the new size.
    pub(crate) fn reprobe_images(&mut self, paths: Vec<PathBuf>, cx: &mut gpui::Context<'_, Self>) {
        let Some(targets) = self
            .state
            .borrow()
            .doc
            .as_ref()
            .map(|d| Arc::clone(&d.images))
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let job = Arc::clone(&targets);
            let probes = cx
                .background_executor()
                .spawn_with_priority(gpui::Priority::Low, async move {
                    let engine = media_engine(job.root());
                    paths
                        .into_iter()
                        .map(|p| {
                            let r = probe(&engine, &p);
                            (p, r)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |reader, cx| reader.reprobed(&targets, probes, cx))
                .ok();
        })
        .detach();
    }

    /// New probes of changed image files landed.
    fn reprobed(
        &mut self,
        targets: &Arc<ImageTargets>,
        probes: Vec<(PathBuf, Result<Probe, MediaError>)>,
        cx: &mut gpui::Context<'_, Self>,
    ) {
        let current = self
            .state
            .borrow()
            .doc
            .as_ref()
            .is_some_and(|d| Arc::ptr_eq(&d.images, targets));
        if !current {
            // The document was built again meanwhile, with new probes.
            return;
        }
        let mut dests = std::collections::HashSet::new();
        for (path, probe) in probes {
            for d in targets.local_destinations(&path) {
                targets.set(
                    &d,
                    ImageTarget::Local {
                        path: path.clone(),
                        probe: probe.clone(),
                    },
                );
                dests.insert(d);
            }
        }
        if dests.is_empty() {
            return;
        }
        // Sizes that changed lay their items out again; the items whose
        // image kept its size are drawn again for the new pixels.
        self.refresh_images(&dests);
        let mut st = self.state.borrow_mut();
        let ids: Vec<ItemId> = st.doc.as_ref().map_or_else(Vec::new, |doc| {
            (0..doc.tree.items().len())
                .filter(|&ix| {
                    doc.tree.item_inline(ix).is_some_and(|c| {
                        c.images.iter().any(|i| {
                            dests.contains(&*gv_render_tree::image_key(
                                &i.destination,
                                i.from.as_deref(),
                            ))
                        })
                    })
                })
                .map(|ix| doc.tree.items()[ix].id)
                .collect()
        });
        st.cache.forget(ids.iter().copied());
        st.stale.extend(ids);
        drop(st);
        cx.notify();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;

    use super::*;

    thread_local! {
        static JOBS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn count_job() {
        assert_eq!(
            crate::reader::code_highlight::tests::in_frame(),
            0,
            "an image job ran while a frame was drawn"
        );
        JOBS.with(|j| j.set(j.get() + 1));
    }

    /// Image jobs run on this thread so far.
    pub(crate) fn jobs_run() -> usize {
        JOBS.with(Cell::get)
    }

    #[test]
    fn github_fragments_pick_a_scheme() {
        assert_eq!(
            scheme_only("a.png#gh-dark-mode-only"),
            Some(SchemeOnly::Dark)
        );
        assert_eq!(
            scheme_only("img/a.png#gh-light-mode-only"),
            Some(SchemeOnly::Light)
        );
        assert_eq!(scheme_only("a.png#other"), None);
        assert_eq!(scheme_only("a.png"), None);
    }

    #[test]
    fn decoded_frames_become_bgra_with_delays() {
        let frame = |c: [u8; 4], delay_ms| gv_media::Frame {
            width: 1,
            height: 1,
            rgba: c.to_vec(),
            delay_ms,
        };
        let d = DecodedImage {
            format: gv_media::ImageFormat::Gif,
            source_width: 1,
            source_height: 1,
            frames: vec![frame([1, 2, 3, 255], 50), frame([4, 5, 6, 255], 120)],
            truncated: false,
        };
        let img = render_image(&d).expect("an image");
        assert_eq!(img.frame_count(), 2);
        assert_eq!(img.as_bytes(0), Some(&[3, 2, 1, 255][..]));
        // The shared clock walks the frames by their delays and loops.
        assert_eq!(crate::inline::animation_frame(&img, 0), 0);
        assert_eq!(crate::inline::animation_frame(&img, 49), 0);
        assert_eq!(crate::inline::animation_frame(&img, 50), 1);
        assert_eq!(crate::inline::animation_frame(&img, 169), 1);
        assert_eq!(crate::inline::animation_frame(&img, 170), 0);
    }
}
