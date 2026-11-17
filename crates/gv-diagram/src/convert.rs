// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! merman's SVG to the display list, through usvg (text to outlines with the
//! measuring font, strokes to fill outlines, transforms applied).

use usvg::tiny_skia_path::{self, PathSegment, PathStroker, Transform};

use crate::display::{
    DiagramItem, DiagramLink, DiagramPath, FillRule, LinkTarget, Paint, PathEl, Rect,
};
use crate::palette::{self, Role};

/// Why a conversion stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConvertError {
    /// usvg rejected the SVG.
    Svg(String),
    /// Over the item or path-element cap.
    TooLarge { what: &'static str, max: usize },
}

/// Caps on the display list.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ListCaps {
    pub max_items: usize,
    pub max_elements: usize,
}

/// Extra precision when flattening strokes, so outlines stay smooth when
/// the host zooms in.
const STROKE_RESOLUTION: f32 = 4.0;

struct Builder {
    items: Vec<DiagramItem>,
    elements: usize,
    caps: ListCaps,
    skipped: usize,
}

/// What a conversion made.
pub(crate) struct Converted {
    /// Size in CSS pixels.
    pub width: f32,
    pub height: f32,
    pub items: Vec<DiagramItem>,
    pub links: Vec<DiagramLink>,
    /// Unsupported nodes skipped.
    pub skipped: usize,
}

/// Parses `svg` with `options` (its font database must hold the fonts
/// merman measured with) and converts it.
pub(crate) fn convert(
    svg: &str,
    options: &usvg::Options<'_>,
    caps: ListCaps,
) -> Result<Converted, ConvertError> {
    let xml = usvg::roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: u32::try_from(caps.max_elements).unwrap_or(u32::MAX),
        ..usvg::roxmltree::ParsingOptions::default()
    };
    let doc = usvg::roxmltree::Document::parse_with_options(svg, xml)
        .map_err(|e| ConvertError::Svg(e.to_string()))?;
    let tree =
        usvg::Tree::from_xmltree(&doc, options).map_err(|e| ConvertError::Svg(e.to_string()))?;
    let size = tree.size();
    let mut b = Builder {
        items: Vec::new(),
        elements: 0,
        caps,
        skipped: 0,
    };
    b.group(tree.root(), Transform::identity(), 1.0, None)?;
    let links = links(&doc, &tree);
    Ok(Converted {
        width: size.width(),
        height: size.height(),
        items: b.items,
        links,
        skipped: b.skipped,
    })
}

/// Links found at most per diagram.
const MAX_LINKS: usize = 1_000;

/// The clickable parts of the SVG: `<a>` elements with a link (Mermaid's
/// `click`), and nodes of class `internal-link` (Obsidian's internal
/// links, named by their text). Each is placed by the first element with
/// an id at or below it, through usvg's tree (transforms applied).
fn links(doc: &usvg::roxmltree::Document<'_>, tree: &usvg::Tree) -> Vec<DiagramLink> {
    const XLINK: &str = "http://www.w3.org/1999/xlink";
    let mut out = Vec::new();
    for node in doc.descendants().filter(|n| n.is_element()) {
        if out.len() >= MAX_LINKS {
            break;
        }
        let target = if node.tag_name().name() == "a" {
            let href = node
                .attribute((XLINK, "href"))
                .or_else(|| node.attribute("href"))
                .map(str::trim)
                .filter(|h| !h.is_empty() && *h != "about:blank" && !h.starts_with('#'));
            match href {
                Some(h) => LinkTarget::Href(h.to_owned()),
                None => continue,
            }
        } else if node
            .attribute("class")
            .is_some_and(|c| c.split_ascii_whitespace().any(|c| c == "internal-link"))
        {
            let mut text = String::new();
            for t in node.descendants().filter(|n| n.is_text()) {
                let piece = t.text().unwrap_or("").trim();
                if !piece.is_empty() {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(piece);
                }
            }
            if text.is_empty() {
                continue;
            }
            LinkTarget::Note(text)
        } else {
            continue;
        };
        let rect = node
            .descendants()
            .filter_map(|n| n.attribute("id"))
            .find_map(|id| tree.node_by_id(id))
            .map(usvg::Node::abs_bounding_box)
            .filter(|r| r.width() > 0.0 && r.height() > 0.0);
        if let Some(r) = rect {
            out.push(DiagramLink {
                rect: Rect {
                    x: r.x(),
                    y: r.y(),
                    width: r.width(),
                    height: r.height(),
                },
                target,
            });
        }
    }
    out
}

impl Builder {
    /// Walks `g` under the parent transform `parent`. Transforms are
    /// accumulated during the walk (as resvg does): the flattened outlines
    /// of text carry no absolute transform of their own.
    fn group(
        &mut self,
        g: &usvg::Group,
        parent: Transform,
        opacity: f32,
        clip: Option<Rect>,
    ) -> Result<(), ConvertError> {
        let ts = parent.pre_concat(g.transform());
        let opacity = opacity * g.opacity().get();
        if opacity <= 0.0 {
            return Ok(());
        }
        let clip = match g.clip_path() {
            Some(cp) => intersect(clip, clip_rect(cp, ts)),
            None => clip,
        };
        if g.mask().is_some() || !g.filters().is_empty() {
            // Masks and filters (merman's base theme uses neither) are
            // dropped; the content is still drawn.
            self.skipped += 1;
        }
        for node in g.children() {
            match node {
                usvg::Node::Group(child) => self.group(child, ts, opacity, clip)?,
                usvg::Node::Path(p) => self.path(p, ts, opacity, clip)?,
                usvg::Node::Text(t) => self.group(t.flattened(), ts, opacity, clip)?,
                usvg::Node::Image(_) => self.skipped += 1,
            }
        }
        Ok(())
    }

    fn path(
        &mut self,
        p: &usvg::Path,
        ts: Transform,
        opacity: f32,
        clip: Option<Rect>,
    ) -> Result<(), ConvertError> {
        if !p.is_visible() {
            return Ok(());
        }
        let fill_first = p.paint_order() == usvg::PaintOrder::FillAndStroke;
        if fill_first {
            self.fill(p, ts, opacity, clip)?;
            self.stroke(p, ts, opacity, clip)?;
        } else {
            self.stroke(p, ts, opacity, clip)?;
            self.fill(p, ts, opacity, clip)?;
        }
        Ok(())
    }

    fn fill(
        &mut self,
        p: &usvg::Path,
        ts: Transform,
        opacity: f32,
        clip: Option<Rect>,
    ) -> Result<(), ConvertError> {
        let Some(fill) = p.fill() else {
            return Ok(());
        };
        let Some((paint, paint_opacity)) = paint_of(fill.paint()) else {
            self.skipped += 1;
            return Ok(());
        };
        let Some(outline) = p.data().clone().transform(ts) else {
            return Ok(());
        };
        let rule = match fill.rule() {
            usvg::FillRule::NonZero => FillRule::NonZero,
            usvg::FillRule::EvenOdd => FillRule::EvenOdd,
        };
        self.push(
            &outline,
            rule,
            paint,
            opacity * fill.opacity().get() * paint_opacity,
            clip,
        )
    }

    fn stroke(
        &mut self,
        p: &usvg::Path,
        ts: Transform,
        opacity: f32,
        clip: Option<Rect>,
    ) -> Result<(), ConvertError> {
        let Some(stroke) = p.stroke() else {
            return Ok(());
        };
        let Some((paint, paint_opacity)) = paint_of(stroke.paint()) else {
            self.skipped += 1;
            return Ok(());
        };
        let ts_stroke = stroke.to_tiny_skia();
        let res = PathStroker::compute_resolution_scale(&ts) * STROKE_RESOLUTION;
        let dashed;
        let source = match &ts_stroke.dash {
            Some(dash) => match p.data().dash(dash, res) {
                Some(d) => {
                    dashed = d;
                    &dashed
                }
                None => return Ok(()),
            },
            None => p.data(),
        };
        let Some(outline) = source.stroke(&ts_stroke, res).and_then(|o| o.transform(ts)) else {
            return Ok(());
        };
        self.push(
            &outline,
            FillRule::NonZero,
            paint,
            opacity * stroke.opacity().get() * paint_opacity,
            clip,
        )
    }

    fn push(
        &mut self,
        outline: &tiny_skia_path::Path,
        rule: FillRule,
        paint: Paint,
        opacity: f32,
        clip: Option<Rect>,
    ) -> Result<(), ConvertError> {
        if opacity <= 0.0 {
            return Ok(());
        }
        let mut els = Vec::with_capacity(outline.len() + 1);
        for seg in outline.segments() {
            els.push(match seg {
                PathSegment::MoveTo(p) => PathEl::MoveTo(p.x, p.y),
                PathSegment::LineTo(p) => PathEl::LineTo(p.x, p.y),
                PathSegment::QuadTo(c, p) => PathEl::QuadTo(c.x, c.y, p.x, p.y),
                PathSegment::CubicTo(c1, c2, p) => {
                    PathEl::CubicTo(c1.x, c1.y, c2.x, c2.y, p.x, p.y)
                }
                PathSegment::Close => PathEl::Close,
            });
        }
        self.elements += els.len();
        if self.elements > self.caps.max_elements {
            return Err(ConvertError::TooLarge {
                what: "path elements",
                max: self.caps.max_elements,
            });
        }
        if self.items.len() >= self.caps.max_items {
            return Err(ConvertError::TooLarge {
                what: "drawing items",
                max: self.caps.max_items,
            });
        }
        self.items.push(DiagramItem {
            path: DiagramPath { els },
            rule,
            paint,
            opacity: opacity.min(1.0),
            clip,
        });
        Ok(())
    }
}

/// The paint and its own opacity. Gradients become their first stop (the
/// base theme draws none); patterns are not supported.
fn paint_of(paint: &usvg::Paint) -> Option<(Paint, f32)> {
    let (color, opacity) = match paint {
        usvg::Paint::Color(c) => (*c, 1.0),
        usvg::Paint::LinearGradient(g) => {
            let s = g.stops().first()?;
            (s.color(), s.opacity().get())
        }
        usvg::Paint::RadialGradient(g) => {
            let s = g.stops().first()?;
            (s.color(), s.opacity().get())
        }
        usvg::Paint::Pattern(_) => return None,
    };
    let rgb = [color.red, color.green, color.blue];
    let paint = Role::from_marker(rgb)
        .or_else(|| Role::category_by_hue(rgb))
        .map_or(Paint::Rgb(rgb), Paint::Role);
    Some((paint, opacity))
}

/// The bounds of a clip path's shapes under `ts`: clips are kept as
/// rectangles (merman clips marker and label viewports, which are boxes).
fn clip_rect(cp: &usvg::ClipPath, ts: Transform) -> Option<Rect> {
    let ts = ts.pre_concat(cp.transform());
    let mut out: Option<tiny_skia_path::Rect> = None;
    let mut stack: Vec<(&usvg::Group, Transform)> = vec![(cp.root(), ts)];
    while let Some((g, ts)) = stack.pop() {
        let ts = ts.pre_concat(g.transform());
        for node in g.children() {
            match node {
                usvg::Node::Group(child) => stack.push((child, ts)),
                usvg::Node::Path(p) => {
                    let Some(r) = p.data().bounds().transform(ts) else {
                        continue;
                    };
                    out = Some(match out {
                        None => r,
                        Some(o) => tiny_skia_path::Rect::from_ltrb(
                            o.left().min(r.left()),
                            o.top().min(r.top()),
                            o.right().max(r.right()),
                            o.bottom().max(r.bottom()),
                        )
                        .unwrap_or(o),
                    });
                }
                _ => {}
            }
        }
    }
    out.map(|r| Rect {
        x: r.x(),
        y: r.y(),
        width: r.width(),
        height: r.height(),
    })
}

fn intersect(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (None, r) | (r, None) => r,
        (Some(a), Some(b)) => {
            let x0 = a.x.max(b.x);
            let y0 = a.y.max(b.y);
            let x1 = (a.x + a.width).min(b.x + b.width);
            let y1 = (a.y + a.height).min(b.y + b.height);
            Some(Rect {
                x: x0,
                y: y0,
                width: (x1 - x0).max(0.0),
                height: (y1 - y0).max(0.0),
            })
        }
    }
}

/// Rewrites merman's fixed presentation-attribute colours to role markers
/// ([`palette::hardcoded`]) and gives the root a `color` (for
/// `currentColor`) of the text role.
pub(crate) fn normalise_hardcoded(svg: &str) -> String {
    let mut out = String::with_capacity(svg.len() + 32);
    let mut rest = svg;
    if let Some(i) = rest.find("<svg") {
        out.push_str(&rest[..i + 4]);
        out.push_str(&format!(r#" color="{}""#, palette::marker_hex(Role::Text)));
        rest = &rest[i + 4..];
    }
    loop {
        let next = [" fill=\"", " stroke=\""]
            .iter()
            .filter_map(|k| rest.find(k).map(|i| (i, *k)))
            .min_by_key(|(i, _)| *i);
        let Some((i, key)) = next else {
            out.push_str(rest);
            break;
        };
        let start = i + key.len();
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find('"') else {
            out.push_str(tail);
            break;
        };
        let value = &tail[..end];
        match palette::hardcoded(value, key.contains("stroke")) {
            Some(role) => out.push_str(&palette::marker_hex(role)),
            None => out.push_str(value),
        }
        rest = &tail[end..];
    }
    // Grey strokes in merman's own style sheet (journey faces).
    for grey in ["stroke:#666;", "stroke:#999;"] {
        out = out.replace(
            grey,
            &format!("stroke:{};", palette::marker_hex(Role::Line)),
        );
    }
    out
}

/// Mermaid computes some stroke widths that can go negative (mindmap edges
/// at depth 6 and below). Browsers treat a negative width as invalid and use
/// the initial value, 1; usvg would drop the stroke. Rewrite them to 1.
pub(crate) fn clamp_negative_stroke_widths(svg: &str) -> std::borrow::Cow<'_, str> {
    const KEYS: [&str; 2] = ["stroke-width=\"-", "stroke-width:-"];
    if !KEYS.iter().any(|k| svg.contains(k)) {
        return std::borrow::Cow::Borrowed(svg);
    }
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    loop {
        let next = KEYS
            .iter()
            .filter_map(|k| rest.find(k).map(|i| (i, *k)))
            .min_by_key(|(i, _)| *i);
        let Some((i, key)) = next else {
            out.push_str(rest);
            break;
        };
        // Keep the key without its minus sign, then skip the number.
        out.push_str(&rest[..i + key.len() - 1]);
        out.push('1');
        let tail = &rest[i + key.len()..];
        let num_len = tail
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E'))
            .unwrap_or(tail.len());
        // A unit (px) after the number is kept as is.
        rest = &tail[num_len..];
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_stroke_widths_become_one() {
        let s = r#"<path stroke-width="-4"/><g style="stroke-width:-1.5px;fill:red"/>"#;
        assert_eq!(
            clamp_negative_stroke_widths(s),
            r#"<path stroke-width="1"/><g style="stroke-width:1px;fill:red"/>"#
        );
        let ok = r#"<path stroke-width="3"/>"#;
        assert!(matches!(
            clamp_negative_stroke_widths(ok),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn hardcoded_attributes_become_markers() {
        let s =
            r##"<svg x="1"><circle fill="white" stroke="#666"/><rect style="fill:white"/></svg>"##;
        let out = normalise_hardcoded(s);
        assert!(out.starts_with(r##"<svg color="#191f1b" x="1">"##), "{out}");
        assert!(out.contains(r##"fill="#fdfdf6""##), "{out}");
        assert!(out.contains(r##"stroke="#4a514e""##), "{out}");
        assert!(out.contains("fill:white"), "CSS is left alone: {out}");
    }

    #[test]
    fn clips_intersect() {
        let a = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let b = Rect {
            x: 5.0,
            y: 5.0,
            width: 10.0,
            height: 10.0,
        };
        let r = intersect(Some(a), Some(b)).expect("rect");
        assert_eq!((r.x, r.y, r.width, r.height), (5.0, 5.0, 5.0, 5.0));
    }
}
