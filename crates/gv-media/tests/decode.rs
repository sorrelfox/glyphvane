// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Decoding: formats, EXIF orientation, colour, animation frames and their
//! budget, caps, SVG rules, the cache, and a small corpus of broken files.
//! Every input is generated here; nothing is downloaded.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use gv_media::{
    CancelFlag, DecodeRequest, DecodedImage, ImageFormat, Limits, MediaEngine, MediaError,
    MediaKey, SvgContext,
};
use image::codecs::gif::GifEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder, Rgba, RgbaImage};

const STILL: DecodeRequest = DecodeRequest {
    fit: None,
    animate: false,
};
const ANIM: DecodeRequest = DecodeRequest {
    fit: None,
    animate: true,
};

fn engine() -> MediaEngine {
    MediaEngine::new(Limits::default(), SvgContext::default())
}

fn load(bytes: &[u8], req: DecodeRequest) -> Result<Arc<DecodedImage>, MediaError> {
    engine().load_bytes(bytes, req, &CancelFlag::new())
}

// ---------------------------------------------------------------- inputs

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

/// A 3 × 2 grid of 8 px blocks: red top left, green top right, blue bottom
/// left, white elsewhere.
fn marked() -> RgbaImage {
    RgbaImage::from_fn(24, 16, |x, y| {
        Rgba(match (x / 8, y / 8) {
            (0, 0) => RED,
            (2, 0) => GREEN,
            (0, 1) => BLUE,
            _ => WHITE,
        })
    })
}

fn png_bytes(img: &RgbaImage, exif: Option<Vec<u8>>, icc: Option<Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = PngEncoder::new(&mut out);
    if let Some(e) = exif {
        enc.set_exif_metadata(e).expect("png exif");
    }
    if let Some(p) = icc {
        enc.set_icc_profile(p).expect("png icc");
    }
    enc.write_image(img, img.width(), img.height(), ExtendedColorType::Rgba8)
        .expect("png");
    out
}

fn jpeg_bytes(img: &RgbaImage, exif: Option<Vec<u8>>) -> Vec<u8> {
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).into_rgb8();
    let mut out = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut out, 100);
    if let Some(e) = exif {
        enc.set_exif_metadata(e).expect("jpeg exif");
    }
    enc.write_image(&rgb, rgb.width(), rgb.height(), ExtendedColorType::Rgb8)
        .expect("jpeg");
    out
}

fn webp_bytes(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .encode(img, img.width(), img.height(), ExtendedColorType::Rgba8)
        .expect("webp");
    out
}

fn solid(w: u32, h: u32, c: [u8; 4]) -> RgbaImage {
    RgbaImage::from_pixel(w, h, Rgba(c))
}

/// A frame colour per index.
fn frame_colour(i: usize) -> [u8; 4] {
    [RED, GREEN, BLUE, WHITE][i % 4]
}

fn gif_anim(frames: usize, w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut out);
        enc.set_repeat(image::codecs::gif::Repeat::Infinite)
            .expect("repeat");
        for i in 0..frames {
            let f = image::Frame::from_parts(
                solid(w, h, frame_colour(i)),
                0,
                0,
                image::Delay::from_numer_denom_ms(50, 1),
            );
            enc.encode_frame(f).expect("gif frame");
        }
    }
    out
}

fn apng(frames: usize, w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_animated(u32::try_from(frames).expect("frames"), 0)
            .expect("animated");
        enc.set_frame_delay(1, 10).expect("delay");
        let mut writer = enc.write_header().expect("header");
        for i in 0..frames {
            writer
                .write_image_data(solid(w, h, frame_colour(i)).as_raw())
                .expect("frame");
        }
        writer.finish().expect("finish");
    }
    out
}

/// An animated WebP built from lossless frames (the encoder in `image`
/// writes stills only): RIFF, VP8X, ANIM, then one ANMF per frame.
fn webp_anim(frames: usize, w: u32, h: u32) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, fourcc: &[u8], payload: &[u8]) {
        out.extend_from_slice(fourcc);
        out.extend_from_slice(&u32::try_from(payload.len()).expect("len").to_le_bytes());
        out.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            out.push(0);
        }
    }
    fn u24(v: u32) -> [u8; 3] {
        let b = v.to_le_bytes();
        [b[0], b[1], b[2]]
    }
    let mut body = Vec::new();
    let mut vp8x = vec![0x02 | 0x10, 0, 0, 0];
    vp8x.extend_from_slice(&u24(w - 1));
    vp8x.extend_from_slice(&u24(h - 1));
    chunk(&mut body, b"VP8X", &vp8x);
    chunk(&mut body, b"ANIM", &[0, 0, 0, 0, 0, 0]);
    for i in 0..frames {
        let still = webp_bytes(&solid(w, h, frame_colour(i)));
        // The image data chunk of the still: everything after "RIFF....WEBP".
        let data = &still[12..];
        let mut anmf = Vec::new();
        anmf.extend_from_slice(&u24(0));
        anmf.extend_from_slice(&u24(0));
        anmf.extend_from_slice(&u24(w - 1));
        anmf.extend_from_slice(&u24(h - 1));
        anmf.extend_from_slice(&u24(70));
        anmf.push(0x02);
        anmf.extend_from_slice(data);
        chunk(&mut body, b"ANMF", &anmf);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&u32::try_from(body.len() + 4).expect("len").to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&body);
    out
}

/// A TIFF block with one IFD entry: Orientation = `o`.
fn exif_orientation(o: u16) -> Vec<u8> {
    let mut e = b"MM\0\x2a\0\0\0\x08".to_vec();
    e.extend_from_slice(&1u16.to_be_bytes());
    e.extend_from_slice(&0x0112u16.to_be_bytes());
    e.extend_from_slice(&3u16.to_be_bytes());
    e.extend_from_slice(&1u32.to_be_bytes());
    e.extend_from_slice(&o.to_be_bytes());
    e.extend_from_slice(&[0, 0]);
    e.extend_from_slice(&0u32.to_be_bytes());
    e
}

/// A PNG whose header claims `w × h` at `depth` bits per RGBA channel,
/// followed by a token IDAT: only a header-first check can stop it.
fn png_header_only(w: u32, h: u32, depth: u8) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&u32::try_from(data.len()).expect("len").to_be_bytes());
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc.finalize().to_be_bytes());
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[depth, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(
        &mut out,
        b"IDAT",
        &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

fn px(img: &DecodedImage, x: u32, y: u32) -> [u8; 4] {
    let f = &img.frames[0];
    let i = ((y * f.width + x) * 4) as usize;
    [f.rgba[i], f.rgba[i + 1], f.rgba[i + 2], f.rgba[i + 3]]
}

fn close(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| x.abs_diff(*y) <= tol)
}

/// The colour of block `(bx, by)` (8 px blocks) of a decoded marked image.
fn block(img: &DecodedImage, bx: u32, by: u32) -> [u8; 4] {
    px(img, bx * 8 + 4, by * 8 + 4)
}

// ----------------------------------------------------------------- tests

#[test]
fn every_still_format_decodes() {
    let img = marked();
    let mut bmp = Vec::new();
    image::DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut Cursor::new(&mut bmp), image::ImageFormat::Bmp)
        .expect("bmp");
    let mut ico = Vec::new();
    image::DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut Cursor::new(&mut ico), image::ImageFormat::Ico)
        .expect("ico");
    let mut gif = Vec::new();
    image::DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut Cursor::new(&mut gif), image::ImageFormat::Gif)
        .expect("gif");
    let cases: Vec<(ImageFormat, Vec<u8>, u8)> = vec![
        (ImageFormat::Png, png_bytes(&img, None, None), 0),
        (ImageFormat::Jpeg, jpeg_bytes(&img, None), 24),
        (ImageFormat::Gif, gif, 0),
        (ImageFormat::WebP, webp_bytes(&img), 0),
        (ImageFormat::Bmp, bmp, 0),
        (ImageFormat::Ico, ico, 0),
    ];
    for (format, bytes, tol) in cases {
        let d = load(&bytes, STILL).unwrap_or_else(|e| panic!("{format:?}: {e}"));
        assert_eq!(d.format, format);
        assert_eq!((d.source_width, d.source_height), (24, 16), "{format:?}");
        assert_eq!(d.frames.len(), 1);
        assert!(
            close(block(&d, 0, 0), RED, tol),
            "{format:?} {:?}",
            block(&d, 0, 0)
        );
        assert!(close(block(&d, 2, 0), GREEN, tol), "{format:?}");
        assert!(close(block(&d, 0, 1), BLUE, tol), "{format:?}");
        let probe = engine().probe_bytes(&bytes).expect("probe");
        assert_eq!((probe.format, probe.width, probe.height), (format, 24, 16));
    }
}

/// Where red, green and blue blocks land for each EXIF orientation of the
/// marked 3 × 2 grid, as `(width, height, red, green, blue)` in blocks.
type Block = (u32, u32);

fn expected(o: u16) -> (u32, u32, Block, Block, Block) {
    match o {
        1 => (3, 2, (0, 0), (2, 0), (0, 1)),
        2 => (3, 2, (2, 0), (0, 0), (2, 1)),
        3 => (3, 2, (2, 1), (0, 1), (2, 0)),
        4 => (3, 2, (0, 1), (2, 1), (0, 0)),
        5 => (2, 3, (0, 0), (0, 2), (1, 0)),
        6 => (2, 3, (1, 0), (1, 2), (0, 0)),
        7 => (2, 3, (1, 2), (1, 0), (0, 2)),
        8 => (2, 3, (0, 2), (0, 0), (1, 2)),
        _ => unreachable!(),
    }
}

#[test]
fn exif_orientation_is_applied_for_jpeg_and_png() {
    for o in 1..=8u16 {
        let (bw, bh, red, green, blue) = expected(o);
        for (name, bytes, tol) in [
            ("jpeg", jpeg_bytes(&marked(), Some(exif_orientation(o))), 24),
            (
                "png",
                png_bytes(&marked(), Some(exif_orientation(o)), None),
                0,
            ),
        ] {
            let d = load(&bytes, STILL).unwrap_or_else(|e| panic!("{name} {o}: {e}"));
            assert_eq!(
                (d.source_width, d.source_height),
                (bw * 8, bh * 8),
                "{name} {o}"
            );
            assert!(close(block(&d, red.0, red.1), RED, tol), "{name} {o}: red");
            assert!(
                close(block(&d, green.0, green.1), GREEN, tol),
                "{name} {o}: green"
            );
            assert!(
                close(block(&d, blue.0, blue.1), BLUE, tol),
                "{name} {o}: blue"
            );
            let p = engine().probe_bytes(&bytes).expect("probe");
            assert_eq!((p.width, p.height), (bw * 8, bh * 8), "{name} {o}: probe");
        }
    }
}

#[test]
fn icc_profiles_are_converted_to_srgb() {
    let p3 = moxcms::ColorProfile::new_display_p3()
        .encode()
        .expect("encode P3");
    let src = [200u8, 100, 50, 255];
    let img = solid(4, 4, src);
    let tagged = load(&png_bytes(&img, None, Some(p3)), STILL).expect("p3 png");
    let plain = load(&png_bytes(&img, None, None), STILL).expect("plain png");
    assert_eq!(px(&plain, 1, 1), src);
    let c = px(&tagged, 1, 1);
    // Display P3 orange is more saturated than the same numbers in sRGB.
    assert!(c[0] > 205 && c[2] < 45, "{c:?}");
    assert_eq!(c[3], 255);
}

#[test]
fn animations_decode_every_frame() {
    for (name, bytes) in [
        ("gif", gif_anim(6, 10, 8)),
        ("apng", apng(6, 10, 8)),
        ("webp", webp_anim(6, 10, 8)),
    ] {
        let d = load(&bytes, ANIM).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(d.frames.len(), 6, "{name}");
        assert!(d.is_animated());
        assert!(!d.truncated);
        for (i, f) in d.frames.iter().enumerate() {
            assert_eq!((f.width, f.height), (10, 8), "{name}");
            assert!(f.delay_ms > 0, "{name} frame {i} has no delay");
            let c = [f.rgba[0], f.rgba[1], f.rgba[2], f.rgba[3]];
            assert!(close(c, frame_colour(i), 2), "{name} frame {i}: {c:?}");
        }
        // Reduced motion: the first frame only.
        let still = load(&bytes, STILL).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(still.frames.len(), 1, "{name}");
        assert!(close(px(&still, 0, 0), RED, 2), "{name}");
    }
}

#[test]
fn the_frame_budget_cuts_long_animations() {
    // 40 frames of 64 × 64 RGBA are 640 KB; allow 10 frames' worth.
    let limits = Limits {
        max_frame_bytes: 10 * 64 * 64 * 4,
        ..Limits::default()
    };
    let e = MediaEngine::new(limits, SvgContext::default());
    let d = e
        .load_bytes(&gif_anim(40, 64, 64), ANIM, &CancelFlag::new())
        .expect("gif");
    assert!(d.truncated);
    assert_eq!(d.frames.len(), 10);
    assert!(d.bytes() <= 10 * 64 * 64 * 4);

    let limits = Limits {
        max_frames: 3,
        ..Limits::default()
    };
    let e = MediaEngine::new(limits, SvgContext::default());
    let d = e
        .load_bytes(&apng(8, 4, 4), ANIM, &CancelFlag::new())
        .expect("apng");
    assert!(d.truncated);
    assert_eq!(d.frames.len(), 3);

    // Fitting shrinks what a frame holds, so more frames fit the budget.
    let limits = Limits {
        max_frame_bytes: 10 * 64 * 64 * 4,
        ..Limits::default()
    };
    let e = MediaEngine::new(limits, SvgContext::default());
    let d = e
        .load_bytes(
            &gif_anim(40, 64, 64),
            DecodeRequest {
                fit: Some((16, 16)),
                animate: true,
            },
            &CancelFlag::new(),
        )
        .expect("gif fitted");
    assert_eq!(d.frames.len(), 40);
    assert!(!d.truncated);
}

#[test]
fn images_are_fitted_without_upscaling() {
    let img = solid(400, 300, [10, 20, 30, 255]);
    let bytes = png_bytes(&img, None, None);
    let d = load(
        &bytes,
        DecodeRequest {
            fit: Some((100, 100)),
            animate: false,
        },
    )
    .expect("png");
    assert_eq!((d.frames[0].width, d.frames[0].height), (100, 75));
    assert_eq!((d.source_width, d.source_height), (400, 300));
    assert!(close(px(&d, 50, 37), [10, 20, 30, 255], 1));
    let d = load(
        &bytes,
        DecodeRequest {
            fit: Some((1000, 1000)),
            animate: false,
        },
    )
    .expect("png");
    assert_eq!((d.frames[0].width, d.frames[0].height), (400, 300));
}

#[test]
fn caps_refuse_from_the_header() {
    // 400 MP: refused by pixel count, before any allocation.
    let huge = png_header_only(20_000, 20_000, 8);
    match load(&huge, STILL) {
        Err(MediaError::TooLarge { what: "pixels", .. }) => {}
        other => panic!("{other:?}"),
    }
    // 81 MP of RGBA16 is 648 MB decoded: under the pixel cap, over the
    // byte cap (the case `image`'s own max_alloc misses for PNG).
    let deep = png_header_only(9_000, 9_000, 16);
    match load(&deep, STILL) {
        Err(MediaError::TooLarge {
            what: "decoded bytes",
            actual,
            ..
        }) => assert_eq!(actual, 9_000 * 9_000 * 8),
        other => panic!("{other:?}"),
    }
    // A side longer than the cap.
    let wide = png_header_only(70_000, 1, 8);
    assert!(matches!(
        load(&wide, STILL),
        Err(MediaError::TooLarge { .. })
    ));
    // Animation canvases are checked too.
    let limits = Limits {
        max_pixels: 50,
        ..Limits::default()
    };
    let e = MediaEngine::new(limits, SvgContext::default());
    assert!(matches!(
        e.load_bytes(&gif_anim(2, 10, 10), ANIM, &CancelFlag::new()),
        Err(MediaError::TooLarge { what: "pixels", .. })
    ));
    // File size.
    let limits = Limits {
        max_file_bytes: 100,
        ..Limits::default()
    };
    let e = MediaEngine::new(limits, SvgContext::default());
    assert!(matches!(
        e.load_bytes(&png_bytes(&marked(), None, None), STILL, &CancelFlag::new()),
        Err(MediaError::TooLarge { .. })
    ));
}

#[test]
fn unsupported_formats_are_named_for_the_card() {
    let avif = b"\0\0\0\x1cftypavif\0\0\0\0mif1miafMA1B\0\0\0\0".to_vec();
    let heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic\0\0\0\0".to_vec();
    let jxl = b"\xFF\x0A\xFA\x7F\x01\x90\x08".to_vec();
    for (bytes, f) in [
        (avif, ImageFormat::Avif),
        (heic, ImageFormat::Heic),
        (jxl, ImageFormat::JpegXl),
    ] {
        let err = load(&bytes, STILL).expect_err("unsupported");
        assert_eq!(err, MediaError::Unsupported(f));
        assert!(err.offers_open_externally());
        assert!(err.card_message().contains(f.name()));
    }
    assert!(matches!(
        load(b"just some text", STILL),
        Err(MediaError::Malformed(_))
    ));
}

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20" viewBox="0 0 40 20">
<rect width="20" height="20" fill="#ff0000"/><rect x="20" width="20" height="20" fill="#0000ff" fill-opacity="0.5"/></svg>"##;

#[test]
fn svg_rasterizes_and_scales_to_fit() {
    let d = load(SVG.as_bytes(), STILL).expect("svg");
    assert_eq!(d.format, ImageFormat::Svg);
    assert_eq!((d.frames[0].width, d.frames[0].height), (40, 20));
    assert!(close(px(&d, 5, 10), RED, 0));
    // Straight alpha out.
    assert!(
        close(px(&d, 30, 10), [0, 0, 255, 128], 1),
        "{:?}",
        px(&d, 30, 10)
    );
    // Vectors scale up to the box.
    let d = load(
        SVG.as_bytes(),
        DecodeRequest {
            fit: Some((400, 400)),
            animate: false,
        },
    )
    .expect("svg");
    assert_eq!((d.frames[0].width, d.frames[0].height), (400, 200));
    let p = engine().probe_bytes(SVG.as_bytes()).expect("probe");
    assert_eq!((p.width, p.height), (40, 20));
}

#[test]
fn svg_text_uses_the_given_fonts() {
    let mut db = resvg::usvg::fontdb::Database::new();
    db.load_font_data(
        ratex_katex_fonts::ttf_bytes("KaTeX_SansSerif-Regular.ttf")
            .expect("font")
            .into_owned(),
    );
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="30"><text x="2" y="24" font-family="KaTeX_SansSerif" font-size="24">Hi</text></svg>"#;
    let ink = |e: &MediaEngine| {
        let d = e
            .load_bytes(svg.as_bytes(), STILL, &CancelFlag::new())
            .expect("svg");
        d.frames[0]
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0)
            .count()
    };
    let with = MediaEngine::new(
        Limits::default(),
        SvgContext {
            fontdb: Arc::new(db),
            allowed_root: None,
        },
    );
    assert!(ink(&with) > 20);
    assert_eq!(ink(&engine()), 0, "no fonts, no text, no crash");
}

#[test]
fn svg_references_stay_inside_the_root_and_off_the_network() {
    let root = tempfile::tempdir().expect("tmp");
    let outside = tempfile::tempdir().expect("tmp");
    let red = png_bytes(&solid(10, 10, RED), None, None);
    std::fs::write(root.path().join("in.png"), &red).expect("write");
    std::fs::write(outside.path().join("out.png"), &red).expect("write");
    let doc = |href: &str| {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image width="10" height="10" xlink:href="{href}"/></svg>"#
        )
    };
    let e = MediaEngine::new(
        Limits::default(),
        SvgContext {
            fontdb: Arc::new(resvg::usvg::fontdb::Database::new()),
            allowed_root: Some(root.path().to_path_buf()),
        },
    );
    let drawn = |svg: String, name: &str| {
        let file = root.path().join(name);
        std::fs::write(&file, svg).expect("write svg");
        let d = e.load_path(&file, STILL, &CancelFlag::new()).expect("svg");
        px(&d, 5, 5)[3] > 0
    };
    assert!(
        drawn(doc("in.png"), "a.svg"),
        "a file inside the root loads"
    );
    let out = outside.path().join("out.png");
    assert!(
        !drawn(doc(&out.display().to_string()), "b.svg"),
        "outside the root"
    );
    assert!(!drawn(doc("../out.png"), "c.svg"), "escape by ..");
    assert!(
        !drawn(doc("https://example.invalid/x.png"), "d.svg"),
        "network"
    );
    assert!(
        !drawn(doc(&format!("file://{}", out.display())), "e.svg"),
        "file URL"
    );
    // No root at all: nothing external.
    let file = root.path().join("a.svg");
    let d = engine()
        .load_path(&file, STILL, &CancelFlag::new())
        .expect("svg");
    assert_eq!(px(&d, 5, 5)[3], 0);
    // Data URLs work without a root.
    let b64 = base64(&red);
    let d = load(
        doc(&format!("data:image/png;base64,{b64}")).as_bytes(),
        STILL,
    )
    .expect("svg");
    assert!(close(px(&d, 5, 5), RED, 0));
}

#[test]
fn svg_caps_hold() {
    // Too many nodes.
    let mut big =
        String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">"#);
    for _ in 0..2_000 {
        big.push_str("<g/>");
    }
    big.push_str("</svg>");
    let limits = Limits {
        max_svg_nodes: 1_000,
        ..Limits::default()
    };
    let e = MediaEngine::new(limits, SvgContext::default());
    assert!(matches!(
        e.load_bytes(big.as_bytes(), STILL, &CancelFlag::new()),
        Err(MediaError::TooLarge {
            what: "SVG nodes",
            ..
        })
    ));
    // Entity expansion ("billion laughs") ends quickly with an error.
    let laughs = r#"<?xml version="1.0"?><!DOCTYPE svg [
<!ENTITY a "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa">
<!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;"><!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;">
<!ENTITY d "&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;"><!ENTITY e "&d;&d;&d;&d;&d;&d;&d;&d;&d;&d;">
<!ENTITY f "&e;&e;&e;&e;&e;&e;&e;&e;&e;&e;"><!ENTITY g "&f;&f;&f;&f;&f;&f;&f;&f;&f;&f;">
<!ENTITY h "&g;&g;&g;&g;&g;&g;&g;&g;&g;&g;"><!ENTITY i "&h;&h;&h;&h;&h;&h;&h;&h;&h;&h;">
]><svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><text>&i;</text></svg>"#;
    let t = std::time::Instant::now();
    let _ = load(laughs.as_bytes(), STILL);
    assert!(t.elapsed().as_secs() < 5);
    // A raster far larger than the caps from a tiny SVG.
    let wide = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100000" height="100000"/>"#;
    assert!(matches!(
        load(wide.as_bytes(), STILL),
        Err(MediaError::TooLarge { .. })
    ));
    // SVGZ inflates under the cap; a bomb does not.
    let gz = gzip_store(SVG.as_bytes());
    assert!(load(&gz, STILL).is_ok());
    let limits = Limits {
        max_svg_bytes: 64,
        ..Limits::default()
    };
    let e = MediaEngine::new(limits, SvgContext::default());
    assert!(matches!(
        e.load_bytes(&gz, STILL, &CancelFlag::new()),
        Err(MediaError::TooLarge { .. })
    ));
    // Nested SVG images stop at the nesting cap instead of recursing.
    let inner = base64(SVG.as_bytes());
    let mut doc = SVG.to_owned();
    for _ in 0..6 {
        let b = base64(doc.as_bytes());
        doc = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="40" height="20"><image width="40" height="20" xlink:href="data:image/svg+xml;base64,{b}"/></svg>"#
        );
    }
    let _ = inner;
    assert!(load(doc.as_bytes(), STILL).is_ok());
}

#[test]
fn files_are_cached_by_path_time_and_size() {
    let dir = tempfile::tempdir().expect("tmp");
    let file = dir.path().join("x.png");
    std::fs::write(&file, png_bytes(&solid(8, 8, RED), None, None)).expect("write");
    let e = engine();
    let key = MediaKey::for_path(&file, STILL).expect("key");
    assert!(e.cached(&key).is_none());
    let a = e.load_path(&file, STILL, &CancelFlag::new()).expect("load");
    let b = e.load_path(&file, STILL, &CancelFlag::new()).expect("load");
    assert!(Arc::ptr_eq(&a, &b));
    assert!(e.cached(&key).is_some());
    assert_eq!(e.cache_stats().entries, 1);
    // A different file at the same path (another size) is a new key.
    std::fs::write(&file, png_bytes(&solid(9, 9, GREEN), None, None)).expect("rewrite");
    let c = e.load_path(&file, STILL, &CancelFlag::new()).expect("load");
    assert_eq!(c.source_width, 9);
    // Another request size is another entry.
    let _ = e
        .load_path(
            &file,
            DecodeRequest {
                fit: Some((4, 4)),
                animate: false,
            },
            &CancelFlag::new(),
        )
        .expect("load");
    assert_eq!(e.cache_stats().entries, 3);
    assert!(matches!(
        e.load_path(Path::new("/nonexistent/x.png"), STILL, &CancelFlag::new()),
        Err(MediaError::Io(_))
    ));
}

#[test]
fn cancelled_loads_are_not_cached() {
    let e = engine();
    let bytes = png_bytes(&marked(), None, None);
    let cancel = CancelFlag::new();
    cancel.cancel();
    assert_eq!(
        e.load_bytes(&bytes, STILL, &cancel).expect_err("cancelled"),
        MediaError::Cancelled
    );
    assert!(e.cached(&MediaKey::for_bytes(&bytes, STILL)).is_none());
    assert!(e.load_bytes(&bytes, STILL, &CancelFlag::new()).is_ok());
}

#[test]
fn futures_decode_off_the_calling_thread() {
    let e = Arc::new(engine());
    let bytes: Arc<[u8]> = png_bytes(&marked(), None, None).into();
    let fut = Arc::clone(&e).load_bytes_future(Arc::clone(&bytes), STILL, CancelFlag::new());
    let handle = std::thread::spawn(move || block_on(fut));
    let d = handle.join().expect("thread").expect("decode");
    assert_eq!(d.source_width, 24);
}

#[test]
fn bgra_swap() {
    let mut f = gv_media::Frame {
        width: 1,
        height: 1,
        rgba: vec![1, 2, 3, 4],
        delay_ms: 0,
    };
    f.rgba_to_bgra();
    assert_eq!(f.rgba, vec![3, 2, 1, 4]);
}

/// Truncations and byte flips of every valid test file: no panic, no hang,
/// and either an image or an error that is not a caught panic.
#[test]
fn broken_files_fail_cleanly() {
    let img = marked();
    let seeds: Vec<(&str, Vec<u8>)> = vec![
        ("png", png_bytes(&img, Some(exif_orientation(6)), None)),
        ("jpeg", jpeg_bytes(&img, Some(exif_orientation(3)))),
        ("webp", webp_bytes(&img)),
        ("gif", gif_anim(3, 12, 12)),
        ("apng", apng(3, 12, 12)),
        ("webp-anim", webp_anim(3, 12, 12)),
        ("svg", SVG.as_bytes().to_vec()),
    ];
    let e = engine();
    let mut cases = 0;
    for (name, seed) in &seeds {
        let mut variants: Vec<Vec<u8>> = Vec::new();
        for cut in [
            1,
            2,
            8,
            12,
            16,
            24,
            33,
            50,
            64,
            100,
            seed.len() / 2,
            seed.len() - 1,
        ] {
            if cut < seed.len() {
                variants.push(seed[..cut].to_vec());
            }
        }
        for k in 0..40usize {
            let mut b = seed.clone();
            let at = (k * 7919 + 13) % b.len();
            b[at] ^= [0xFF, 0x01, 0x80, 0x55][k % 4];
            if k % 3 == 0 {
                let at2 = (k * 104_729) % b.len();
                b[at2] = 0;
            }
            variants.push(b);
        }
        // Huge sizes written into the header bytes.
        let mut b = seed.clone();
        for v in b.iter_mut().skip(16).take(8) {
            *v = 0x7F;
        }
        variants.push(b);
        for v in variants {
            cases += 1;
            for req in [STILL, ANIM] {
                match e.load_bytes(&v, req, &CancelFlag::new()) {
                    Err(MediaError::Panicked) => panic!("{name}: decoder panicked"),
                    Err(MediaError::Cancelled) => panic!("{name}: cancelled"),
                    _ => {}
                }
            }
            let _ = e.probe_bytes(&v);
        }
    }
    assert!(cases > 300, "{cases}");
}

// ---------------------------------------------------------------- helpers

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

/// Gzip with stored (uncompressed) deflate blocks: enough for an SVGZ test.
fn gzip_store(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    let chunks: Vec<&[u8]> = data.chunks(65_535).collect();
    for (i, c) in chunks.iter().enumerate() {
        out.push(u8::from(i + 1 == chunks.len()));
        let len = u16::try_from(c.len()).expect("len");
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
    let mut crc = crc32fast::Hasher::new();
    crc.update(data);
    out.extend_from_slice(&crc.finalize().to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).expect("len").to_le_bytes());
    out
}

/// Polls a future that completes on its first poll.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("the decode future does its work on the first poll"),
    }
}
