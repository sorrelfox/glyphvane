// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The document model: the parsed JSON plus typed, validated views of the
//! nodes and edges.
//!
//! The JSON value is kept as parsed (object keys in file order, unknown keys
//! included), so serialising it again loses nothing. The typed [`Node`] and
//! [`Edge`] lists hold only the items that can be drawn; every problem found
//! on the way is reported as an [`Issue`] against its item, and one bad item
//! never fails the whole file.

use std::collections::HashMap;
use std::fmt;

use serde_json::{Map, Value};

use crate::color::CanvasColor;
use crate::geom::{Rect, Side};

/// Coordinates beyond this magnitude are rejected: they are almost surely
/// broken data and would make the spatial index and transforms useless.
pub const MAX_COORDINATE: f64 = 1.0e9;

/// The whole file could not be read as a canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Not valid JSON. `line` and `column` are 1-based.
    Json {
        message: String,
        line: usize,
        column: usize,
    },
    /// Valid JSON, but the top level is not an object.
    NotAnObject,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json {
                message,
                line,
                column,
            } => write!(f, "invalid JSON at line {line}, column {column}: {message}"),
            Self::NotAnObject => f.write_str("the top level of a canvas must be an object"),
        }
    }
}

impl std::error::Error for ParseError {}

/// How serious an [`Issue`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The item is drawn, with a default in place of the bad value.
    Warning,
    /// The item cannot be drawn and is left out of the typed lists (it stays
    /// in the JSON).
    Error,
}

/// Which part of the file an [`Issue`] is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueItem {
    /// The top-level object.
    Root,
    /// `nodes[index]`, with its `id` when it has a readable one.
    Node { index: usize, id: Option<String> },
    /// `edges[index]`, with its `id` when it has a readable one.
    Edge { index: usize, id: Option<String> },
}

/// What is wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueKind {
    /// The array entry is not a JSON object.
    NotAnObject,
    /// A required key is absent.
    MissingKey(&'static str),
    /// A key holds the wrong JSON type.
    WrongType {
        key: &'static str,
        expected: &'static str,
    },
    /// A key has a value outside its allowed set.
    InvalidValue { key: &'static str, value: String },
    /// A number is not finite or beyond [`MAX_COORDINATE`].
    OutOfRange(&'static str),
    /// `width` or `height` is zero or negative.
    NonPositiveSize,
    /// The node `type` is not one of the four JSON Canvas types. The node is
    /// still drawn as a plain card.
    UnknownNodeType(String),
    /// Another node earlier in the file has the same `id`. Edges resolve to
    /// the first one.
    DuplicateId(String),
    /// An edge points at a node id that does not exist (or was rejected).
    DanglingEdge { key: &'static str, target: String },
}

impl fmt::Display for IssueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnObject => f.write_str("entry is not an object"),
            Self::MissingKey(k) => write!(f, "missing required key `{k}`"),
            Self::WrongType { key, expected } => write!(f, "`{key}` must be {expected}"),
            Self::InvalidValue { key, value } => write!(f, "`{key}` has an invalid value {value}"),
            Self::OutOfRange(k) => write!(f, "`{k}` is out of range"),
            Self::NonPositiveSize => f.write_str("`width` and `height` must be positive"),
            Self::UnknownNodeType(t) => write!(f, "unknown node type `{t}`"),
            Self::DuplicateId(id) => write!(f, "duplicate id `{id}`"),
            Self::DanglingEdge { key, target } => {
                write!(f, "`{key}` refers to missing node `{target}`")
            }
        }
    }
}

/// One problem with one item of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    pub item: IssueItem,
    pub kind: IssueKind,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        match &self.item {
            IssueItem::Root => write!(f, "{sev}: {}", self.kind),
            IssueItem::Node { index, id } | IssueItem::Edge { index, id } => {
                let what = if matches!(self.item, IssueItem::Node { .. }) {
                    "node"
                } else {
                    "edge"
                };
                match id {
                    Some(id) => write!(f, "{sev}: {what} {index} (`{id}`): {}", self.kind),
                    None => write!(f, "{sev}: {what} {index}: {}", self.kind),
                }
            }
        }
    }
}

/// `subpath` of a file node: a heading path or a block id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subpath {
    /// `#Heading` or `#Parent#Child` (Obsidian's nested heading form).
    Heading(Vec<String>),
    /// `#^block-id`.
    Block(String),
}

impl Subpath {
    /// Parses a subpath. The leading `#` is optional here; the validator
    /// warns when it is missing.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if let Some(block) = s.strip_prefix('^') {
            return (!block.is_empty()).then(|| Self::Block(block.to_owned()));
        }
        let parts: Vec<String> = s
            .split('#')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect();
        (!parts.is_empty()).then_some(Self::Heading(parts))
    }
}

/// `backgroundStyle` of a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BackgroundStyle {
    /// Fill the group, cropping as needed.
    Cover,
    /// Fit inside the group, keeping the aspect ratio.
    Ratio,
    /// Tile the image.
    Repeat,
}

/// What a node shows.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    /// Markdown text.
    Text { text: String },
    /// A vault file, optionally narrowed to a heading or block. `subpath_raw`
    /// is the string as written.
    File {
        file: String,
        subpath: Option<Subpath>,
        subpath_raw: Option<String>,
    },
    /// An external URL. Never fetched by default.
    Link { url: String },
    /// A visual group.
    Group {
        label: Option<String>,
        background: Option<String>,
        background_style: Option<BackgroundStyle>,
    },
    /// A type this version does not know; drawn as an empty card.
    Unknown { type_name: String },
}

impl NodeKind {
    pub fn is_group(&self) -> bool {
        matches!(self, Self::Group { .. })
    }
}

/// A drawable node.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// Position in the file's `nodes` array (z-order in the file).
    pub index: usize,
    pub id: String,
    pub rect: Rect,
    pub color: Option<CanvasColor>,
    pub kind: NodeKind,
}

/// An edge end marker (`fromEnd` / `toEnd`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum End {
    None,
    Arrow,
}

/// A drawable edge.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    /// Position in the file's `edges` array.
    pub index: usize,
    pub id: String,
    pub from_node: String,
    pub to_node: String,
    /// Positions of the two nodes in [`Canvas::nodes`].
    pub from: usize,
    pub to: usize,
    /// `None` when the file leaves the side out: the view picks one.
    pub from_side: Option<Side>,
    pub to_side: Option<Side>,
    /// Defaults to [`End::None`].
    pub from_end: End,
    /// Defaults to [`End::Arrow`].
    pub to_end: End,
    pub color: Option<CanvasColor>,
    pub label: Option<String>,
}

/// Keys the format defines for every node.
pub const NODE_GENERIC_KEYS: &[&str] = &["id", "type", "x", "y", "width", "height", "color"];
/// Keys the format defines for edges.
pub const EDGE_KEYS: &[&str] = &[
    "id", "fromNode", "fromSide", "fromEnd", "toNode", "toSide", "toEnd", "color", "label",
];

/// The type-specific keys of a node type.
pub fn node_type_keys(type_name: &str) -> &'static [&'static str] {
    match type_name {
        "text" => &["text"],
        "file" => &["file", "subpath"],
        "link" => &["url"],
        "group" => &["label", "background", "backgroundStyle"],
        _ => &[],
    }
}

/// A parsed `.canvas` file.
#[derive(Clone, Debug)]
pub struct Canvas {
    root: Map<String, Value>,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    issues: Vec<Issue>,
}

impl Canvas {
    /// Parses and validates a `.canvas` file. Fails only when the text is not
    /// a JSON object; everything else is reported through [`Canvas::issues`].
    pub fn parse(src: &str) -> Result<Self, ParseError> {
        // A leading byte-order mark is not JSON, but editors write one.
        let src = src.strip_prefix('\u{feff}').unwrap_or(src);
        let value: Value = serde_json::from_str(src).map_err(|e| ParseError::Json {
            message: e.to_string(),
            line: e.line(),
            column: e.column(),
        })?;
        match value {
            Value::Object(root) => Ok(Self::from_root(root)),
            _ => Err(ParseError::NotAnObject),
        }
    }

    /// Builds the model from an already parsed top-level object.
    pub fn from_root(root: Map<String, Value>) -> Self {
        let mut v = Validator::default();
        let nodes = v.nodes(&root);
        let edges = v.edges(&root, &nodes);
        Self {
            root,
            nodes,
            edges,
            issues: v.issues,
        }
    }

    /// An empty canvas (`{}`).
    pub fn empty() -> Self {
        Self::from_root(Map::new())
    }

    /// Drawable nodes, in file order.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Drawable edges, in file order.
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Every problem found, in file order (root, then nodes, then edges).
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }

    /// The JSON as parsed, unknown keys included.
    pub fn json(&self) -> &Map<String, Value> {
        &self.root
    }

    /// The JSON object of `nodes[index]` as written, if it is an object.
    pub fn node_json(&self, index: usize) -> Option<&Map<String, Value>> {
        self.root.get("nodes")?.as_array()?.get(index)?.as_object()
    }

    /// The JSON object of `edges[index]` as written, if it is an object.
    pub fn edge_json(&self, index: usize) -> Option<&Map<String, Value>> {
        self.root.get("edges")?.as_array()?.get(index)?.as_object()
    }

    /// Keys of `node` the format does not define (Obsidian extensions and
    /// others), in file order.
    pub fn unknown_node_keys<'a>(&'a self, node: &Node) -> Vec<(&'a str, &'a Value)> {
        let type_keys = match &node.kind {
            NodeKind::Text { .. } => node_type_keys("text"),
            NodeKind::File { .. } => node_type_keys("file"),
            NodeKind::Link { .. } => node_type_keys("link"),
            NodeKind::Group { .. } => node_type_keys("group"),
            NodeKind::Unknown { .. } => &[],
        };
        self.node_json(node.index)
            .map(|m| {
                m.iter()
                    .filter(|(k, _)| {
                        !NODE_GENERIC_KEYS.contains(&k.as_str()) && !type_keys.contains(&k.as_str())
                    })
                    .map(|(k, v)| (k.as_str(), v))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Keys of `edge` the format does not define, in file order.
    pub fn unknown_edge_keys<'a>(&'a self, edge: &Edge) -> Vec<(&'a str, &'a Value)> {
        self.edge_json(edge.index)
            .map(|m| {
                m.iter()
                    .filter(|(k, _)| !EDGE_KEYS.contains(&k.as_str()))
                    .map(|(k, v)| (k.as_str(), v))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Top-level keys other than `nodes` and `edges`, in file order.
    pub fn unknown_root_keys(&self) -> Vec<(&str, &Value)> {
        self.root
            .iter()
            .filter(|(k, _)| k.as_str() != "nodes" && k.as_str() != "edges")
            .map(|(k, v)| (k.as_str(), v))
            .collect()
    }

    /// The position in [`Canvas::nodes`] of the node with this id.
    pub fn node_position(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    /// Serialises the JSON the way Obsidian writes canvas files
    /// (`JSON.stringify(data, null, "\t")`): tab indentation, keys in their
    /// original order, no trailing newline. A file Obsidian wrote comes back
    /// byte for byte. Glyphvane 1.0 never writes whole canvas files (renames
    /// use [`crate::patch`]); this exists for tests and later editing.
    pub fn to_json_string(&self) -> String {
        use serde_json::ser::{PrettyFormatter, Serializer};
        let mut out = Vec::new();
        let mut ser = Serializer::with_formatter(&mut out, PrettyFormatter::with_indent(b"\t"));
        // Writing a `Map` of `Value`s into a `Vec` cannot fail.
        if serde::Serialize::serialize(&self.root, &mut ser).is_err() {
            return String::new();
        }
        String::from_utf8(out).unwrap_or_default()
    }

    /// Compact serialisation (no white space), keys in original order.
    pub fn to_json_compact(&self) -> String {
        Value::Object(self.root.clone()).to_string()
    }
}

#[derive(Default)]
struct Validator {
    issues: Vec<Issue>,
}

fn obj_id(o: &Map<String, Value>) -> Option<String> {
    o.get("id").and_then(Value::as_str).map(str::to_owned)
}

impl Validator {
    fn push(&mut self, severity: Severity, item: &IssueItem, kind: IssueKind) {
        self.issues.push(Issue {
            severity,
            item: item.clone(),
            kind,
        });
    }

    fn array<'a>(&mut self, root: &'a Map<String, Value>, key: &'static str) -> &'a [Value] {
        match root.get(key) {
            None | Some(Value::Null) => &[],
            Some(Value::Array(a)) => a,
            Some(_) => {
                self.push(
                    Severity::Error,
                    &IssueItem::Root,
                    IssueKind::WrongType {
                        key,
                        expected: "an array",
                    },
                );
                &[]
            }
        }
    }

    /// A required string. Reports and returns `None` when absent or not a
    /// string.
    fn req_str(
        &mut self,
        o: &Map<String, Value>,
        key: &'static str,
        item: &IssueItem,
        severity: Severity,
    ) -> Option<String> {
        match o.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            None => {
                self.push(severity, item, IssueKind::MissingKey(key));
                None
            }
            Some(_) => {
                self.push(
                    severity,
                    item,
                    IssueKind::WrongType {
                        key,
                        expected: "a string",
                    },
                );
                None
            }
        }
    }

    /// An optional string. A wrong type is a warning and reads as absent.
    fn opt_str(
        &mut self,
        o: &Map<String, Value>,
        key: &'static str,
        item: &IssueItem,
    ) -> Option<String> {
        match o.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => {
                self.push(
                    Severity::Warning,
                    item,
                    IssueKind::WrongType {
                        key,
                        expected: "a string",
                    },
                );
                None
            }
        }
    }

    fn number(
        &mut self,
        o: &Map<String, Value>,
        key: &'static str,
        item: &IssueItem,
    ) -> Option<f64> {
        match o.get(key) {
            None => {
                self.push(Severity::Error, item, IssueKind::MissingKey(key));
                None
            }
            Some(Value::Number(n)) => match n.as_f64() {
                Some(f) if f.is_finite() && f.abs() <= MAX_COORDINATE => Some(f),
                _ => {
                    self.push(Severity::Error, item, IssueKind::OutOfRange(key));
                    None
                }
            },
            Some(_) => {
                self.push(
                    Severity::Error,
                    item,
                    IssueKind::WrongType {
                        key,
                        expected: "a number",
                    },
                );
                None
            }
        }
    }

    fn color(&mut self, o: &Map<String, Value>, item: &IssueItem) -> Option<CanvasColor> {
        let s = self.opt_str(o, "color", item)?;
        let c = CanvasColor::parse(&s);
        if c.is_none() {
            self.push(
                Severity::Warning,
                item,
                IssueKind::InvalidValue {
                    key: "color",
                    value: format!("{s:?}"),
                },
            );
        }
        c
    }

    fn nodes(&mut self, root: &Map<String, Value>) -> Vec<Node> {
        let arr = self.array(root, "nodes");
        let mut out = Vec::with_capacity(arr.len());
        let mut seen: HashMap<String, ()> = HashMap::with_capacity(arr.len());
        for (index, v) in arr.iter().enumerate() {
            let Some(o) = v.as_object() else {
                self.push(
                    Severity::Error,
                    &IssueItem::Node { index, id: None },
                    IssueKind::NotAnObject,
                );
                continue;
            };
            let item = IssueItem::Node {
                index,
                id: obj_id(o),
            };
            if let Some(node) = self.node(index, o, &item) {
                if seen.insert(node.id.clone(), ()).is_some() {
                    self.push(
                        Severity::Warning,
                        &item,
                        IssueKind::DuplicateId(node.id.clone()),
                    );
                }
                out.push(node);
            }
        }
        out
    }

    fn node(&mut self, index: usize, o: &Map<String, Value>, item: &IssueItem) -> Option<Node> {
        // Check everything before giving up, so one pass reports all issues.
        let id = self.req_str(o, "id", item, Severity::Error);
        let type_name = self.req_str(o, "type", item, Severity::Error);
        let x = self.number(o, "x", item);
        let y = self.number(o, "y", item);
        let w = self.number(o, "width", item);
        let h = self.number(o, "height", item);
        let color = self.color(o, item);
        let kind = match type_name.as_deref() {
            Some(t) => self.node_kind(t, o, item),
            None => None,
        };
        let (x, y, w, h) = (x?, y?, w?, h?);
        if w <= 0.0 || h <= 0.0 {
            self.push(Severity::Error, item, IssueKind::NonPositiveSize);
            return None;
        }
        Some(Node {
            index,
            id: id?,
            rect: Rect::new(x, y, w, h),
            color,
            kind: kind?,
        })
    }

    fn node_kind(&mut self, t: &str, o: &Map<String, Value>, item: &IssueItem) -> Option<NodeKind> {
        Some(match t {
            "text" => {
                // An empty card is still worth drawing.
                let text = self
                    .req_str(o, "text", item, Severity::Warning)
                    .unwrap_or_default();
                NodeKind::Text { text }
            }
            "file" => {
                let file = self.req_str(o, "file", item, Severity::Error);
                let subpath_raw = self.opt_str(o, "subpath", item);
                let subpath = subpath_raw.as_deref().and_then(|s| {
                    let parsed = Subpath::parse(s);
                    if parsed.is_none() || !s.starts_with('#') {
                        self.push(
                            Severity::Warning,
                            item,
                            IssueKind::InvalidValue {
                                key: "subpath",
                                value: format!("{s:?}"),
                            },
                        );
                    }
                    parsed
                });
                NodeKind::File {
                    file: file?,
                    subpath,
                    subpath_raw,
                }
            }
            "link" => NodeKind::Link {
                url: self.req_str(o, "url", item, Severity::Error)?,
            },
            "group" => {
                let label = self.opt_str(o, "label", item);
                let background = self.opt_str(o, "background", item);
                let background_style =
                    self.opt_str(o, "backgroundStyle", item)
                        .and_then(|s| match s.as_str() {
                            "cover" => Some(BackgroundStyle::Cover),
                            "ratio" => Some(BackgroundStyle::Ratio),
                            "repeat" => Some(BackgroundStyle::Repeat),
                            _ => {
                                self.push(
                                    Severity::Warning,
                                    item,
                                    IssueKind::InvalidValue {
                                        key: "backgroundStyle",
                                        value: format!("{s:?}"),
                                    },
                                );
                                None
                            }
                        });
                NodeKind::Group {
                    label,
                    background,
                    background_style,
                }
            }
            other => {
                self.push(
                    Severity::Warning,
                    item,
                    IssueKind::UnknownNodeType(other.to_owned()),
                );
                NodeKind::Unknown {
                    type_name: other.to_owned(),
                }
            }
        })
    }

    fn edges(&mut self, root: &Map<String, Value>, nodes: &[Node]) -> Vec<Edge> {
        let mut by_id: HashMap<&str, usize> = HashMap::with_capacity(nodes.len());
        for (pos, n) in nodes.iter().enumerate() {
            by_id.entry(n.id.as_str()).or_insert(pos);
        }
        let arr = self.array(root, "edges");
        let mut out = Vec::with_capacity(arr.len());
        for (index, v) in arr.iter().enumerate() {
            let Some(o) = v.as_object() else {
                self.push(
                    Severity::Error,
                    &IssueItem::Edge { index, id: None },
                    IssueKind::NotAnObject,
                );
                continue;
            };
            let item = IssueItem::Edge {
                index,
                id: obj_id(o),
            };
            if let Some(e) = self.edge(index, o, &item, &by_id) {
                out.push(e);
            }
        }
        out
    }

    fn side(
        &mut self,
        o: &Map<String, Value>,
        key: &'static str,
        item: &IssueItem,
    ) -> Option<Side> {
        let s = self.opt_str(o, key, item)?;
        let side = Side::parse(&s);
        if side.is_none() {
            self.push(
                Severity::Warning,
                item,
                IssueKind::InvalidValue {
                    key,
                    value: format!("{s:?}"),
                },
            );
        }
        side
    }

    fn end(
        &mut self,
        o: &Map<String, Value>,
        key: &'static str,
        item: &IssueItem,
        default: End,
    ) -> End {
        let Some(s) = self.opt_str(o, key, item) else {
            return default;
        };
        match s.as_str() {
            "none" => End::None,
            "arrow" => End::Arrow,
            _ => {
                self.push(
                    Severity::Warning,
                    item,
                    IssueKind::InvalidValue {
                        key,
                        value: format!("{s:?}"),
                    },
                );
                default
            }
        }
    }

    fn edge(
        &mut self,
        index: usize,
        o: &Map<String, Value>,
        item: &IssueItem,
        by_id: &HashMap<&str, usize>,
    ) -> Option<Edge> {
        let id = self.req_str(o, "id", item, Severity::Error);
        let from_node = self.req_str(o, "fromNode", item, Severity::Error);
        let to_node = self.req_str(o, "toNode", item, Severity::Error);
        let from_side = self.side(o, "fromSide", item);
        let to_side = self.side(o, "toSide", item);
        let from_end = self.end(o, "fromEnd", item, End::None);
        let to_end = self.end(o, "toEnd", item, End::Arrow);
        let color = self.color(o, item);
        let label = self.opt_str(o, "label", item);
        let mut resolve = |key: &'static str, target: &Option<String>| {
            let t = target.as_deref()?;
            let pos = by_id.get(t).copied();
            if pos.is_none() {
                self.push(
                    Severity::Error,
                    item,
                    IssueKind::DanglingEdge {
                        key,
                        target: t.to_owned(),
                    },
                );
            }
            pos
        };
        let from = resolve("fromNode", &from_node);
        let to = resolve("toNode", &to_node);
        Some(Edge {
            index,
            id: id?,
            from_node: from_node?,
            to_node: to_node?,
            from: from?,
            to: to?,
            from_side,
            to_side,
            from_end,
            to_end,
            color,
            label,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subpaths() {
        assert_eq!(
            Subpath::parse("#Intro"),
            Some(Subpath::Heading(vec!["Intro".into()]))
        );
        assert_eq!(
            Subpath::parse("#A#B"),
            Some(Subpath::Heading(vec!["A".into(), "B".into()]))
        );
        assert_eq!(Subpath::parse("#^abc"), Some(Subpath::Block("abc".into())));
        assert_eq!(Subpath::parse("#^"), None);
        assert_eq!(Subpath::parse("#"), None);
    }

    #[test]
    fn whole_file_errors() {
        assert!(matches!(
            Canvas::parse("{"),
            Err(ParseError::Json { line: 1, .. })
        ));
        assert_eq!(Canvas::parse("[]").err(), Some(ParseError::NotAnObject));
        let empty = Canvas::parse("{}").expect("empty object");
        assert!(empty.nodes().is_empty() && empty.issues().is_empty());
        let bom = Canvas::parse("\u{feff}{\"nodes\":[]}").expect("BOM");
        assert!(bom.issues().is_empty());
    }

    #[test]
    fn wrong_top_level_types() {
        let c = Canvas::parse(r#"{"nodes": 3, "edges": {}}"#).expect("object");
        assert_eq!(c.issues().len(), 2);
        assert!(c.issues().iter().all(|i| i.item == IssueItem::Root));
    }
}
