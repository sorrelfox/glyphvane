// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The cached, capped media engine.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Read as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use crate::format::ImageFormat;
use crate::limits::Limits;
use crate::svg::{self, SvgContext};
use crate::types::{CancelFlag, DecodeRequest, DecodedImage, Probe};
use crate::{MediaError, raster};

/// A decode result, shared.
pub type LoadResult = Result<Arc<DecodedImage>, MediaError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Source {
    /// A file, identified by path, modification time and size: an edit or
    /// replacement on disk gives a new key.
    File {
        path: PathBuf,
        mtime: Option<SystemTime>,
        len: u64,
    },
    /// Bytes in memory (clipboard, data URLs), by content hash.
    Bytes { hash: u64, len: usize },
}

/// The cache key of a request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaKey {
    source: Source,
    request: DecodeRequest,
}

impl MediaKey {
    /// The key of `path` (one `stat`, cheap enough for the UI thread).
    pub fn for_path(path: &Path, request: DecodeRequest) -> Result<Self, MediaError> {
        let meta = std::fs::metadata(path).map_err(|e| MediaError::Io(e.to_string()))?;
        Ok(Self {
            source: Source::File {
                path: path.to_path_buf(),
                mtime: meta.modified().ok(),
                len: meta.len(),
            },
            request,
        })
    }

    /// The key of in-memory `bytes`.
    pub fn for_bytes(bytes: &[u8], request: DecodeRequest) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        Self {
            source: Source::Bytes {
                hash: h.finish(),
                len: bytes.len(),
            },
            request,
        }
    }
}

/// Cache statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub hits: u64,
    pub misses: u64,
}

#[derive(Debug, Default)]
struct Cache {
    map: HashMap<MediaKey, (LoadResult, u64)>,
    tick: u64,
    stats: CacheStats,
}

fn cost(r: &LoadResult) -> usize {
    match r {
        Ok(img) => img.bytes() + 64,
        Err(_) => 64,
    }
}

impl Cache {
    fn get(&mut self, key: &MediaKey) -> Option<LoadResult> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(key)?;
        entry.1 = tick;
        Some(entry.0.clone())
    }

    fn insert(&mut self, key: MediaKey, value: LoadResult, budget: usize) {
        self.tick += 1;
        self.stats.bytes += cost(&value);
        if let Some((old, _)) = self.map.insert(key, (value, self.tick)) {
            self.stats.bytes -= cost(&old);
        }
        if self.stats.bytes > budget {
            let mut by_age: Vec<(u64, MediaKey)> =
                self.map.iter().map(|(k, (_, t))| (*t, k.clone())).collect();
            by_age.sort_unstable_by_key(|(t, _)| *t);
            for (_, k) in by_age {
                if self.stats.bytes <= budget / 4 * 3 {
                    break;
                }
                if let Some((old, _)) = self.map.remove(&k) {
                    self.stats.bytes -= cost(&old);
                }
            }
        }
        self.stats.entries = self.map.len();
    }
}

/// Image and SVG decoding with caps and an LRU cache by bytes. `Send +
/// Sync`: share one per process.
///
/// On the UI thread: [`MediaKey::for_path`] and [`cached`](Self::cached).
/// Everything that decodes ([`load_path`](Self::load_path),
/// [`load_bytes`](Self::load_bytes), the futures) blocks: run it on a
/// background executor. Nothing here touches the network.
#[derive(Debug)]
pub struct MediaEngine {
    limits: Limits,
    svg: SvgContext,
    cache: Mutex<Cache>,
}

impl MediaEngine {
    /// An engine with `limits`; SVG text uses `svg.fontdb` and external SVG
    /// references stay inside `svg.allowed_root`.
    pub fn new(limits: Limits, svg: SvgContext) -> Self {
        Self {
            limits,
            svg,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// The caps in force.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The cached result for `key`, without decoding (UI thread).
    pub fn cached(&self, key: &MediaKey) -> Option<LoadResult> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let hit = cache.get(key);
        if hit.is_some() {
            cache.stats.hits += 1;
        }
        hit
    }

    /// Header facts of a file: format and displayed size, without decoding
    /// pixels (SVG is parsed). Blocking (reads the file).
    pub fn probe_path(&self, path: &Path) -> Result<Probe, MediaError> {
        let bytes = self.read(path)?;
        self.probe_bytes_in(&bytes, path.parent())
    }

    /// Header facts of in-memory bytes.
    pub fn probe_bytes(&self, bytes: &[u8]) -> Result<Probe, MediaError> {
        self.probe_bytes_in(bytes, None)
    }

    fn probe_bytes_in(&self, bytes: &[u8], base: Option<&Path>) -> Result<Probe, MediaError> {
        guard(|| match sniff(bytes)? {
            ImageFormat::Svg => svg::probe(bytes, base, &self.svg, &self.limits),
            f => raster::probe(bytes, f, &self.limits),
        })
    }

    /// Decodes the file at `path`: from the cache when its path, time and
    /// size match. Blocking.
    pub fn load_path(&self, path: &Path, req: DecodeRequest, cancel: &CancelFlag) -> LoadResult {
        let key = MediaKey::for_path(path, req)?;
        self.cached_or(key, cancel, || {
            let bytes = self.read(path)?;
            cancel.check()?;
            self.decode(&bytes, path.parent(), req, cancel)
        })
    }

    /// Decodes in-memory bytes. SVGs decoded this way cannot reference
    /// files. Blocking.
    pub fn load_bytes(&self, bytes: &[u8], req: DecodeRequest, cancel: &CancelFlag) -> LoadResult {
        if bytes.len() as u64 > self.limits.max_file_bytes {
            return Err(MediaError::TooLarge {
                what: "bytes",
                actual: bytes.len() as u64,
                max: self.limits.max_file_bytes,
            });
        }
        let key = MediaKey::for_bytes(bytes, req);
        self.cached_or(key, cancel, || self.decode(bytes, None, req, cancel))
    }

    /// [`load_path`](Self::load_path) as a future for an executor's
    /// background pool (GPUI's `background_spawn`). It decodes when first
    /// polled, blocking that thread: never poll it on the UI thread.
    pub async fn load_path_future(
        self: Arc<Self>,
        path: PathBuf,
        req: DecodeRequest,
        cancel: CancelFlag,
    ) -> LoadResult {
        self.load_path(&path, req, &cancel)
    }

    /// [`load_bytes`](Self::load_bytes) as a future (see
    /// [`load_path_future`](Self::load_path_future)).
    pub async fn load_bytes_future(
        self: Arc<Self>,
        bytes: Arc<[u8]>,
        req: DecodeRequest,
        cancel: CancelFlag,
    ) -> LoadResult {
        self.load_bytes(&bytes, req, &cancel)
    }

    /// Cache statistics.
    pub fn cache_stats(&self) -> CacheStats {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stats
    }

    /// Empties the cache.
    pub fn clear_cache(&self) {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let stats = cache.stats;
        *cache = Cache::default();
        cache.stats.hits = stats.hits;
        cache.stats.misses = stats.misses;
    }

    fn cached_or(
        &self,
        key: MediaKey,
        cancel: &CancelFlag,
        work: impl FnOnce() -> Result<DecodedImage, MediaError>,
    ) -> LoadResult {
        {
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(hit) = cache.get(&key) {
                cache.stats.hits += 1;
                return hit;
            }
            cache.stats.misses += 1;
        }
        cancel.check()?;
        let result = guard(work).map(Arc::new);
        if !matches!(result, Err(MediaError::Cancelled | MediaError::Io(_))) {
            self.cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, result.clone(), self.limits.cache_bytes);
        }
        result
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, MediaError> {
        let file = std::fs::File::open(path).map_err(|e| MediaError::Io(e.to_string()))?;
        let max = self.limits.max_file_bytes;
        let mut bytes = Vec::new();
        file.take(max + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| MediaError::Io(e.to_string()))?;
        if bytes.len() as u64 > max {
            return Err(MediaError::TooLarge {
                what: "file bytes",
                actual: bytes.len() as u64,
                max,
            });
        }
        Ok(bytes)
    }

    fn decode(
        &self,
        bytes: &[u8],
        base: Option<&Path>,
        req: DecodeRequest,
        cancel: &CancelFlag,
    ) -> Result<DecodedImage, MediaError> {
        match sniff(bytes)? {
            ImageFormat::Svg => svg::decode(bytes, base, &self.svg, req, &self.limits, cancel),
            f => raster::decode(bytes, f, req, &self.limits, cancel),
        }
    }
}

/// The supported format of `bytes`, or why not.
fn sniff(bytes: &[u8]) -> Result<ImageFormat, MediaError> {
    match ImageFormat::sniff(bytes) {
        Some(f) if f.is_supported() => Ok(f),
        Some(f) => Err(MediaError::Unsupported(f)),
        None => Err(MediaError::Malformed("not an image".into())),
    }
}

/// Runs a decoder, turning a panic into [`MediaError::Panicked`].
fn guard<T>(f: impl FnOnce() -> Result<T, MediaError>) -> Result<T, MediaError> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        log::error!("gv-media: decoder panicked");
        Err(MediaError::Panicked)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        fn assert_send_static<T: Send + 'static>(_: &T) {}
        assert_send_sync::<MediaEngine>();
        assert_send_sync::<LoadResult>();
        assert_send_sync::<CancelFlag>();
        let e = Arc::new(MediaEngine::new(Limits::default(), SvgContext::default()));
        let fut = e.load_path_future(PathBuf::new(), DecodeRequest::default(), CancelFlag::new());
        assert_send_static(&fut);
    }
}
