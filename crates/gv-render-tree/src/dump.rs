// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Paragraphs of code chunks, and a readable dump of a render tree (for
//! tests and debugging).

use std::fmt::Write as _;
use std::sync::Arc;

use gv_inline::{ItemKind as RunKind, Paragraph, ParagraphBuilder, UnderlineStyle};

use crate::item::{Item, ItemKind};
use crate::node::{
    BlockNode, BoxRef, Frame, FrameKind, InlineContent, InlineRole, ListMarker, NodeKind,
};
use crate::style::{ColorToken, TextRole, Variant, decode_style_id};
use crate::tree::RenderTree;

impl RenderTree {
    /// The `gv-inline` paragraph item `ix` lays out: the block's paragraph
    /// for paragraphs and headings; for code, HTML and front matter chunks
    /// a plain paragraph of the chunk's lines (one hard break per line
    /// ending), built on each call (the UI caches it per item id and
    /// replaces it with highlighted runs). Source offsets are relative to
    /// the node's start. `None` for items without text.
    pub fn item_paragraph(&self, ix: usize) -> Option<Arc<Paragraph>> {
        let item = self.items().get(ix)?;
        let node = self.nodes().get(item.node)?;
        if let Some(c) = node.item_inline(&item.kind) {
            return Some(Arc::clone(&c.paragraph));
        }
        let code = node.lines()?;
        let lines = item.kind.lines()?;
        let role = match node.kind {
            NodeKind::Properties { .. } => TextRole::Properties,
            _ => TextRole::Code,
        };
        let styles = &self.options().styles;
        let style = styles.run_style(role, Variant::PLAIN);
        let mut b = ParagraphBuilder::new(styles.paragraph_style(role));
        let first = code.lines.get(lines.start as usize)?;
        b.source_anchor(first.source.start);
        let mut prev_end: Option<usize> = None;
        for i in lines.start as usize..lines.end as usize {
            let Some(l) = code.lines.get(i) else {
                break;
            };
            if let Some(end) = prev_end {
                b.hard_break(end..l.source.start);
            }
            let text = &code.text[l.text.clone()];
            if !text.is_empty() {
                let indent = usize::from(l.indent).min(text.len());
                let anchors: &[(usize, usize)] = if indent > 0 {
                    &[(indent, l.source.start)]
                } else {
                    &[]
                };
                let anchors = anchors.to_vec();
                b.text_mapped(text, style, l.source.clone(), &anchors);
            }
            prev_end = Some(l.source.end);
        }
        Some(Arc::new(b.build()))
    }

    /// A readable dump of the nodes and items: one block per item with its
    /// context, gap and estimates at 600 and 200 px, then the block's
    /// content (runs with style, colour and decorations; links; code
    /// lines).
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for (ix, item) in self.items().iter().enumerate() {
            let node = &self.nodes()[item.node];
            dump_item(ix, item, &mut out);
            if item.kind.chunk().0 == 0 {
                dump_node(node, &mut out);
            }
            if let (NodeKind::Table(t), ItemKind::TableRow { row, .. }) = (&node.kind, &item.kind)
                && let Some(r) = t.rows.get(*row as usize)
            {
                let _ = writeln!(out, "  row {row}{}", if r.header { " header" } else { "" });
                dump_inline(&r.content, &mut out);
            }
            if let (Some(code), Some(lines)) = (node.lines(), item.kind.lines()) {
                for i in lines.start as usize..lines.end as usize {
                    let _ = writeln!(out, "    line {i}: {:?}", code.line(i));
                }
            }
        }
        out
    }
}

fn dump_item(ix: usize, item: &Item, out: &mut String) {
    let kind = match &item.kind {
        ItemKind::Heading { level } => format!("heading h{level}"),
        ItemKind::Code {
            chunk,
            chunks,
            lines,
        }
        | ItemKind::Html {
            chunk,
            chunks,
            lines,
        }
        | ItemKind::Properties {
            chunk,
            chunks,
            lines,
        } => {
            format!(
                "{} {}/{} lines {}..{}",
                item.kind.name(),
                chunk,
                chunks,
                lines.start,
                lines.end
            )
        }
        k => k.name().to_owned(),
    };
    let _ = writeln!(
        out,
        "item {ix} #{}.{} {kind} {:?} gap={} opens={} closes={} h600={} h200={}",
        item.id.block.0,
        item.id.chunk,
        item.source,
        num(item.gap_before),
        item.opens,
        item.closes,
        num(item.estimate.height(600.0)),
        num(item.estimate.height(200.0)),
    );
    if !item.context.is_empty() {
        let ctx: Vec<String> = item.context.iter().map(frame).collect();
        let _ = writeln!(out, "  ctx {}", ctx.join(" > "));
    }
}

fn frame(f: &Frame) -> String {
    match &f.kind {
        FrameKind::Quote { depth: 0 } => format!("quote#{}", f.id.0),
        FrameKind::Quote { depth } => format!("quote#{}(depth={depth})", f.id.0),
        FrameKind::List {
            ordered,
            tight,
            start,
            depth,
            ..
        } => format!(
            "{}#{}({}{}{})",
            if *ordered { "ol" } else { "ul" },
            f.id.0,
            if *tight { "tight" } else { "loose" },
            if *ordered {
                format!(" start={start}")
            } else {
                String::new()
            },
            if *depth > 0 {
                format!(" depth={depth}")
            } else {
                String::new()
            }
        ),
        FrameKind::Item { marker } => format!(
            "item#{}({})",
            f.id.0,
            match marker {
                ListMarker::Task { checked, .. } => format!("task {checked}"),
                m => m.text(),
            }
        ),
    }
}

fn dump_node(node: &BlockNode, out: &mut String) {
    match &node.kind {
        NodeKind::Paragraph(c) => dump_inline(c, out),
        NodeKind::Heading {
            level,
            setext,
            anchor,
            content,
        } => {
            let _ = writeln!(
                out,
                "  h{level}{} anchor={:?}",
                if *setext { " setext" } else { "" },
                anchor.as_deref().unwrap_or("")
            );
            dump_inline(content, out);
        }
        NodeKind::Code(code) => {
            let _ = writeln!(
                out,
                "  {} info={:?} lang={:?} lines={}",
                if code.fenced { "fenced" } else { "indented" },
                code.info.as_deref().unwrap_or(""),
                code.language.as_deref().unwrap_or(""),
                code.lines.len()
            );
        }
        NodeKind::Html {
            kind,
            raw_html_pending,
            code,
        } => {
            let _ = writeln!(
                out,
                "  html kind={kind} raw_html_pending={raw_html_pending} lines={}",
                code.lines.len()
            );
        }
        NodeKind::Properties { kind, lines } => {
            let _ = writeln!(out, "  front matter {kind:?} lines={}", lines.lines.len());
        }
        NodeKind::Math(code) => {
            let _ = writeln!(out, "  math lines={} tex={:?}", code.lines.len(), code.text);
        }
        NodeKind::Table(t) => {
            let _ = writeln!(out, "  table aligns={:?} rows={}", t.aligns, t.rows.len());
        }
        NodeKind::ThematicBreak | NodeKind::EmptyItem | NodeKind::EmptyQuote => {}
    }
}

fn dump_inline(c: &InlineContent, out: &mut String) {
    let p = &c.paragraph;
    if c.raw_html_pending {
        let _ = writeln!(out, "  raw_html_pending");
    }
    for (ix, (it, span)) in p.items().iter().zip(c.spans.iter()).enumerate() {
        let role = match span.role {
            InlineRole::Text => "text".to_owned(),
            InlineRole::Code => "code".to_owned(),
            InlineRole::Break => "break".to_owned(),
            InlineRole::RawHtml => "raw-html".to_owned(),
            InlineRole::Image(i) => format!("image[{i}]"),
            InlineRole::ImageAlt(i) => format!("alt[{i}]"),
            InlineRole::LinkSuffix => "suffix".to_owned(),
            InlineRole::Math(i) => format!("math[{i}]"),
            InlineRole::CellGap(i) => format!("gap[{i}]"),
        };
        let link = span.link.map(|l| format!(" link[{l}]")).unwrap_or_default();
        let what = match it.kind() {
            RunKind::Text(s) => {
                let (r, v) = decode_style_id(s.font).unwrap_or((TextRole::Body, Variant::PLAIN));
                let mut style = r.name().to_owned();
                if v.contains(Variant::ITALIC) {
                    style.push_str("+i");
                }
                if v.contains(Variant::STRONG) {
                    style.push_str("+b");
                }
                if v.contains(Variant::MONO) {
                    style.push_str("+mono");
                }
                let color = ColorToken::from_role(s.color).map_or("?", ColorToken::name);
                let mut deco = String::new();
                if let Some(u) = s.underline {
                    let st = match u.style {
                        UnderlineStyle::Solid => "solid",
                        UnderlineStyle::Dotted => "dotted",
                        UnderlineStyle::Dashed => "dashed",
                        UnderlineStyle::Wavy => "wavy",
                    };
                    let uc = ColorToken::from_role(u.color).map_or("?", ColorToken::name);
                    let _ = write!(deco, " underline={st}/{uc}");
                }
                if s.strikethrough.is_some() {
                    deco.push_str(" strike");
                }
                if s.background.is_some() {
                    deco.push_str(" chip");
                }
                format!("{:?} {style} {color}{deco}", p.item_text(ix))
            }
            RunKind::Box(b) => format!(
                "box {:?} id={:?} {}x{}",
                b.kind,
                BoxRef::from_id(b.id),
                num(b.width),
                num(b.height())
            ),
            RunKind::HardBreak => "hard-break".to_owned(),
        };
        let _ = writeln!(out, "    {role}{link} {:?} {what}", it.source());
    }
    for (i, l) in c.links.iter().enumerate() {
        let _ = writeln!(
            out,
            "  link[{i}] {:?} {:?} {} {:?}",
            l.kind, &*l.destination, l.status, l.source
        );
    }
    for (i, m) in c.math.iter().enumerate() {
        let _ = writeln!(
            out,
            "  math[{i}] {:?}{} {:?}",
            &*m.tex,
            if m.display { " display" } else { "" },
            m.source
        );
    }
    for (i, im) in c.images.iter().enumerate() {
        let _ = writeln!(
            out,
            "  image[{i}] {:?} alt={:?} {:?} {:?}",
            &*im.destination, &*im.alt, im.size, im.source
        );
    }
}

/// A float for dumps: integers without decimals, else one decimal.
fn num(v: f32) -> String {
    if (v - v.round()).abs() < 0.005 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.2}")
    }
}
