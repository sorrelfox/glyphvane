// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! SVG through usvg and resvg: size and node caps, no scripts (usvg has
//! none), no network, files only inside the allowed root, embedded raster
//! images checked against our caps before resvg decodes them.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{self, ImageHrefResolver, ImageKind};

use crate::format::ImageFormat;
use crate::limits::Limits;
use crate::types::{CancelFlag, DecodeRequest, DecodedImage, Frame, Probe};
use crate::{MediaError, raster};

/// What SVG documents may use: the fonts for `<text>` and the folder that
/// external `<image>` references must stay inside.
#[derive(Debug, Clone)]
pub struct SvgContext {
    /// Fonts for SVG text (the shared font stack). Empty: text is not drawn.
    pub fontdb: Arc<usvg::fontdb::Database>,
    /// External files are loaded only from inside this folder (the vault
    /// root). `None`: no external files at all.
    pub allowed_root: Option<PathBuf>,
}

impl Default for SvgContext {
    fn default() -> Self {
        Self {
            fontdb: Arc::new(usvg::fontdb::Database::new()),
            allowed_root: None,
        }
    }
}

struct Resolve {
    ctx: SvgContext,
    root: Option<PathBuf>,
    limits: Limits,
}

/// Inflates SVGZ (gzip) with a size cap.
fn gunzip(bytes: &[u8], max: u64) -> Result<Vec<u8>, MediaError> {
    let bad = || MediaError::Malformed("broken SVGZ".into());
    // Gzip header (RFC 1952): magic, method 8, flags, then optional fields.
    if bytes.len() < 18 || bytes[2] != 8 {
        return Err(bad());
    }
    let flags = bytes[3];
    let mut i = 10usize;
    if flags & 0x04 != 0 {
        let xlen = usize::from(u16::from_le_bytes([
            *bytes.get(i).ok_or_else(bad)?,
            *bytes.get(i + 1).ok_or_else(bad)?,
        ]));
        i += 2 + xlen;
    }
    for flag in [0x08, 0x10] {
        if flags & flag != 0 {
            let end = bytes
                .get(i..)
                .ok_or_else(bad)?
                .iter()
                .position(|b| *b == 0)
                .ok_or_else(bad)?;
            i += end + 1;
        }
    }
    if flags & 0x02 != 0 {
        i += 2;
    }
    let body = bytes.get(i..).ok_or_else(bad)?;
    let limit = usize::try_from(max).unwrap_or(usize::MAX);
    let mut out = Vec::new();
    let mut d = flate::Inflate::new(body);
    d.read_to_end_limited(&mut out, limit)
        .map_err(|e| match e {
            flate::Error::TooLarge => MediaError::TooLarge {
                what: "SVG bytes",
                actual: max + 1,
                max,
            },
            flate::Error::Broken => bad(),
        })?;
    Ok(out)
}

/// A tiny raw-deflate reader on miniz_oxide with an output cap.
mod flate {
    pub(super) enum Error {
        TooLarge,
        Broken,
    }

    pub(super) struct Inflate<'a> {
        input: &'a [u8],
    }

    impl<'a> Inflate<'a> {
        pub(super) fn new(input: &'a [u8]) -> Self {
            Self { input }
        }

        pub(super) fn read_to_end_limited(
            &mut self,
            out: &mut Vec<u8>,
            limit: usize,
        ) -> Result<(), Error> {
            match miniz_oxide::inflate::decompress_to_vec_with_limit(self.input, limit) {
                Ok(v) => {
                    *out = v;
                    Ok(())
                }
                Err(e) if e.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput => {
                    Err(Error::TooLarge)
                }
                Err(_) => Err(Error::Broken),
            }
        }
    }
}

/// Parses an SVG document. `base` is the folder of the file, for relative
/// references.
pub(crate) fn parse(
    bytes: &[u8],
    base: Option<&Path>,
    ctx: &SvgContext,
    limits: &Limits,
    depth: u8,
) -> Result<usvg::Tree, MediaError> {
    if bytes.len() as u64 > limits.max_svg_bytes {
        return Err(MediaError::TooLarge {
            what: "SVG bytes",
            actual: bytes.len() as u64,
            max: limits.max_svg_bytes,
        });
    }
    let owned;
    let bytes = if bytes.starts_with(b"\x1f\x8b") {
        owned = gunzip(bytes, limits.max_svg_bytes)?;
        &owned[..]
    } else {
        bytes
    };
    let text =
        std::str::from_utf8(bytes).map_err(|_| MediaError::Malformed("SVG is not UTF-8".into()))?;
    let xml = usvg::roxmltree::ParsingOptions {
        allow_dtd: true,
        nodes_limit: limits.max_svg_nodes,
        ..usvg::roxmltree::ParsingOptions::default()
    };
    let doc = usvg::roxmltree::Document::parse_with_options(text, xml).map_err(|e| match e {
        usvg::roxmltree::Error::NodesLimitReached => MediaError::TooLarge {
            what: "SVG nodes",
            actual: u64::from(limits.max_svg_nodes) + 1,
            max: u64::from(limits.max_svg_nodes),
        },
        other => MediaError::Malformed(other.to_string()),
    })?;
    let root = ctx
        .allowed_root
        .as_ref()
        .and_then(|r| std::fs::canonicalize(r).ok());
    let resolve = Arc::new(Resolve {
        ctx: ctx.clone(),
        root,
        limits: *limits,
    });
    let base: Option<PathBuf> = base.map(Path::to_path_buf);
    let (r1, r2) = (Arc::clone(&resolve), Arc::clone(&resolve));
    let base2 = base.clone();
    let options = usvg::Options {
        fontdb: Arc::clone(&ctx.fontdb),
        image_href_resolver: ImageHrefResolver {
            resolve_data: Box::new(move |_mime, data, _| r1.kind(&data, base.as_deref(), depth)),
            resolve_string: Box::new(move |href, _| r2.file(href, base2.as_deref(), depth)),
        },
        ..usvg::Options::default()
    };
    usvg::Tree::from_xmltree(&doc, &options).map_err(|e| MediaError::Malformed(e.to_string()))
}

impl Resolve {
    /// An image from bytes, after our caps.
    fn kind(&self, data: &Arc<Vec<u8>>, base: Option<&Path>, depth: u8) -> Option<ImageKind> {
        let format = ImageFormat::sniff(data)?;
        match format {
            ImageFormat::Svg => {
                if depth + 1 > self.limits.max_svg_nesting {
                    return None;
                }
                parse(data, base, &self.ctx, &self.limits, depth + 1)
                    .ok()
                    .map(ImageKind::SVG)
            }
            ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP => {
                // resvg decodes these itself, at full size: check the
                // header against our caps first.
                let probe = raster::probe(data, format, &self.limits).ok()?;
                let bytes = u64::from(probe.width) * u64::from(probe.height) * 4;
                self.limits
                    .check_canvas(probe.width, probe.height, bytes)
                    .ok()?;
                let data = Arc::clone(data);
                Some(match format {
                    ImageFormat::Png => ImageKind::PNG(data),
                    ImageFormat::Jpeg => ImageKind::JPEG(data),
                    ImageFormat::Gif => ImageKind::GIF(data),
                    _ => ImageKind::WEBP(data),
                })
            }
            _ => None,
        }
    }

    /// An external file: only a plain path inside the allowed root. URLs of
    /// any scheme (network included) are never followed.
    fn file(&self, href: &str, base: Option<&Path>, depth: u8) -> Option<ImageKind> {
        let root = self.root.as_ref()?;
        if href.contains(':') || href.starts_with("//") || href.is_empty() {
            return None;
        }
        let href = href.split(['#', '?']).next()?;
        let path = Path::new(href);
        let full = if path.is_absolute() {
            path.to_path_buf()
        } else {
            base?.join(path)
        };
        let canon = std::fs::canonicalize(&full).ok()?;
        if !canon.starts_with(root) {
            log::debug!("gv-media: SVG reference outside the allowed root ignored");
            return None;
        }
        let file = std::fs::File::open(&canon).ok()?;
        let mut data = Vec::new();
        file.take(self.limits.max_file_bytes + 1)
            .read_to_end(&mut data)
            .ok()?;
        if data.len() as u64 > self.limits.max_file_bytes {
            return None;
        }
        self.kind(&Arc::new(data), canon.parent(), depth)
    }
}

/// The intrinsic size of an SVG, rounded up to whole pixels.
pub(crate) fn probe(
    bytes: &[u8],
    base: Option<&Path>,
    ctx: &SvgContext,
    limits: &Limits,
) -> Result<Probe, MediaError> {
    let tree = parse(bytes, base, ctx, limits, 0)?;
    let size = tree.size();
    Ok(Probe {
        format: ImageFormat::Svg,
        width: px(size.width()),
        height: px(size.height()),
    })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn px(v: f32) -> u32 {
    v.ceil().clamp(1.0, 1.0e9) as u32
}

/// Rasterizes an SVG. With `fit`, it is scaled (up or down: it is a vector)
/// to fit the box; otherwise drawn at its intrinsic size.
pub(crate) fn decode(
    bytes: &[u8],
    base: Option<&Path>,
    ctx: &SvgContext,
    req: DecodeRequest,
    limits: &Limits,
    cancel: &CancelFlag,
) -> Result<DecodedImage, MediaError> {
    let tree = parse(bytes, base, ctx, limits, 0)?;
    cancel.check()?;
    let size = tree.size();
    let (sw, sh) = (px(size.width()), px(size.height()));
    let scale = match req.fit {
        Some((fw, fh)) => (fw as f32 / size.width()).min(fh as f32 / size.height()),
        None => 1.0,
    };
    let (tw, th) = (px(size.width() * scale), px(size.height() * scale));
    limits.check_canvas(tw, th, u64::from(tw) * u64::from(th) * 4)?;
    let mut pixmap =
        Pixmap::new(tw, th).ok_or_else(|| MediaError::Malformed("empty SVG".into()))?;
    resvg::render(
        &tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    cancel.check()?;
    let mut rgba = Vec::with_capacity(pixmap.data().len());
    for p in pixmap.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Ok(DecodedImage {
        format: ImageFormat::Svg,
        source_width: sw,
        source_height: sh,
        frames: vec![Frame {
            width: tw,
            height: th,
            rgba,
            delay_ms: 0,
        }],
        truncated: false,
    })
}
