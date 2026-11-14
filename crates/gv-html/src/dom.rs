// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A small arena DOM built by html5ever through our own [`TreeSink`].
//!
//! Nodes live in one `Vec` and refer to each other by [`NodeId`]. Nothing
//! is reference counted, so dropping the tree is a plain `Vec` drop and
//! handles never outlive their nodes.

use std::borrow::Cow;
use std::cell::RefCell;

use html5ever::interface::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{Attribute, LocalName, Namespace, ParseOpts, QualName, ns, parse_document};

/// Index of a node in a [`Dom`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NodeId(usize);

/// What a node is.
#[derive(Debug)]
pub(crate) enum NodeData {
    /// The document (always node 0) or a template's contents.
    Document,
    /// An element with its attributes.
    Element {
        name: QualName,
        attrs: Vec<Attribute>,
    },
    /// A text node.
    Text(StrTendril),
    /// A comment (kept: Word marks its list bullets with comments).
    Comment(StrTendril),
    /// Doctype or processing instruction.
    Other,
}

#[derive(Debug)]
pub(crate) struct Node {
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    pub(crate) data: NodeData,
}

/// A parsed document.
#[derive(Debug)]
pub(crate) struct Dom {
    nodes: Vec<Node>,
}

impl Dom {
    /// Parses a whole document (scripting off, so `noscript` holds markup).
    pub(crate) fn parse(html: &str) -> Dom {
        let mut opts = ParseOpts::default();
        opts.tree_builder.scripting_enabled = false;
        opts.tree_builder.drop_doctype = true;
        parse_document(Sink::default(), opts).one(html)
    }

    pub(crate) fn document(&self) -> NodeId {
        NodeId(0)
    }

    pub(crate) fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0]
    }

    pub(crate) fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id.0].children
    }

    /// The local name of an HTML element, or `None` for other nodes and for
    /// SVG and MathML elements.
    pub(crate) fn html_name(&self, id: NodeId) -> Option<&str> {
        match &self.node(id).data {
            NodeData::Element { name, .. } if name.ns == ns!(html) => Some(&name.local),
            _ => None,
        }
    }

    /// The local name of any element (whatever its namespace).
    pub(crate) fn any_name(&self, id: NodeId) -> Option<&str> {
        match &self.node(id).data {
            NodeData::Element { name, .. } => Some(&name.local),
            _ => None,
        }
    }

    /// An attribute value (attribute names are lower-cased by the parser).
    pub(crate) fn attr(&self, id: NodeId, attr: &str) -> Option<&str> {
        match &self.node(id).data {
            NodeData::Element { attrs, .. } => attrs
                .iter()
                .find(|a| a.name.ns == ns!() && &*a.name.local == attr)
                .map(|a| &*a.value),
            _ => None,
        }
    }

    /// Appends the text of `id` and its descendants to `out`, iteratively
    /// (no recursion, so any depth is fine). `skip` drops whole subtrees.
    pub(crate) fn text_content(&self, id: NodeId, out: &mut String, skip: &dyn Fn(NodeId) -> bool) {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if skip(n) {
                continue;
            }
            match &self.node(n).data {
                NodeData::Text(t) => out.push_str(t),
                _ => stack.extend(self.children(n).iter().rev()),
            }
        }
    }

    /// The first descendant element of `id` (pre-order) named `name` in any
    /// namespace.
    pub(crate) fn find_descendant(&self, id: NodeId, name: &str) -> Option<NodeId> {
        let mut stack: Vec<NodeId> = self.children(id).iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            if self.any_name(n) == Some(name) {
                return Some(n);
            }
            stack.extend(self.children(n).iter().rev());
        }
        None
    }
}

/// The element name handed back to html5ever (an owned copy: atoms are
/// cheap to clone, and no `RefCell` borrow escapes the sink).
#[derive(Debug)]
pub(crate) struct Name(QualName);

impl ElemName for Name {
    fn ns(&self) -> &Namespace {
        &self.0.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.0.local
    }
}

#[derive(Debug)]
struct Sink {
    nodes: RefCell<Vec<Node>>,
}

impl Default for Sink {
    fn default() -> Self {
        Sink {
            nodes: RefCell::new(vec![Node {
                parent: None,
                children: Vec::new(),
                data: NodeData::Document,
            }]),
        }
    }
}

impl Sink {
    fn new_node(&self, data: NodeData) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        nodes.push(Node {
            parent: None,
            children: Vec::new(),
            data,
        });
        NodeId(nodes.len() - 1)
    }

    fn detach(nodes: &mut [Node], child: NodeId) {
        if let Some(p) = nodes[child.0].parent.take() {
            nodes[p.0].children.retain(|&c| c != child);
        }
    }

    /// Inserts `child` into `parent` at `index` (or at the end), merging
    /// adjacent text.
    fn insert(&self, parent: NodeId, index: Option<usize>, child: NodeOrText<NodeId>) {
        let mut nodes = self.nodes.borrow_mut();
        let len = nodes[parent.0].children.len();
        let at = index.unwrap_or(len);
        match child {
            NodeOrText::AppendText(text) => {
                if at > 0 {
                    let prev = nodes[parent.0].children[at - 1];
                    if let NodeData::Text(t) = &mut nodes[prev.0].data {
                        t.push_tendril(&text);
                        return;
                    }
                }
                nodes.push(Node {
                    parent: Some(parent),
                    children: Vec::new(),
                    data: NodeData::Text(text),
                });
                let id = NodeId(nodes.len() - 1);
                nodes[parent.0].children.insert(at, id);
            }
            NodeOrText::AppendNode(node) => {
                Self::detach(&mut nodes, node);
                // Detaching may have shifted the index.
                let len = nodes[parent.0].children.len();
                let at = at.min(len);
                nodes[node.0].parent = Some(parent);
                nodes[parent.0].children.insert(at, node);
            }
        }
    }
}

impl TreeSink for Sink {
    type Handle = NodeId;
    type Output = Dom;
    type ElemName<'a> = Name;

    fn finish(self) -> Dom {
        Dom {
            nodes: self.nodes.into_inner(),
        }
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> NodeId {
        NodeId(0)
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> Name {
        match &self.nodes.borrow()[target.0].data {
            NodeData::Element { name, .. } => Name(name.clone()),
            _ => Name(QualName::new(None, ns!(), LocalName::from(""))),
        }
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, _: ElementFlags) -> NodeId {
        self.new_node(NodeData::Element { name, attrs })
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.new_node(NodeData::Comment(text))
    }

    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> NodeId {
        self.new_node(NodeData::Other)
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        self.insert(*parent, None, child);
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let has_parent = self.nodes.borrow()[element.0].parent.is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(&self, _: StrTendril, _: StrTendril, _: StrTendril) {}

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        // A fresh, detached fragment: template contents are dropped anyway.
        let _ = target;
        self.new_node(NodeData::Document)
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        if let NodeOrText::AppendNode(node) = &new_node {
            // Detach first so that the sibling's index below is final.
            Self::detach(&mut self.nodes.borrow_mut(), *node);
        }
        let (parent, index) = {
            let nodes = self.nodes.borrow();
            let Some(parent) = nodes[sibling.0].parent else {
                return;
            };
            let index = nodes[parent.0]
                .children
                .iter()
                .position(|c| c == sibling)
                .unwrap_or(0);
            (parent, index)
        };
        self.insert(parent, Some(index), new_node);
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        let mut nodes = self.nodes.borrow_mut();
        if let NodeData::Element { attrs: have, .. } = &mut nodes[target.0].data {
            for a in attrs {
                if !have.iter().any(|h| h.name == a.name) {
                    have.push(a);
                }
            }
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        Self::detach(&mut self.nodes.borrow_mut(), *target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        let mut nodes = self.nodes.borrow_mut();
        let children = std::mem::take(&mut nodes[node.0].children);
        for &c in &children {
            nodes[c.0].parent = Some(*new_parent);
        }
        nodes[new_parent.0].children.extend(children);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(dom: &Dom, id: NodeId, out: &mut String) {
        match &dom.node(id).data {
            NodeData::Element { name, .. } => {
                out.push('<');
                out.push_str(&name.local);
                out.push('>');
                for &c in dom.children(id) {
                    dump(dom, c, out);
                }
                out.push_str("</");
                out.push_str(&name.local);
                out.push('>');
            }
            NodeData::Text(t) => out.push_str(t),
            NodeData::Document => {
                for &c in dom.children(id) {
                    dump(dom, c, out);
                }
            }
            _ => {}
        }
    }

    fn tree(html: &str) -> String {
        let dom = Dom::parse(html);
        let mut s = String::new();
        dump(&dom, dom.document(), &mut s);
        s
    }

    #[test]
    fn builds_tree() {
        assert_eq!(
            tree("<p>a<b>b</b>"),
            "<html><head></head><body><p>a<b>b</b></p></body></html>"
        );
    }

    #[test]
    fn adoption_agency_and_foster_parenting() {
        // Misnested formatting is repaired by moving nodes around.
        assert_eq!(
            tree("<b>1<p>2</b>3</p>"),
            "<html><head></head><body><b>1</b><p><b>2</b>3</p></body></html>"
        );
        // Text inside a table but outside cells is moved before the table.
        assert_eq!(
            tree("<table>x<tr><td>y</td></tr></table>"),
            "<html><head></head><body>x<table><tbody><tr><td>y</td></tr></tbody></table></body></html>"
        );
    }

    #[test]
    fn attributes_and_text() {
        let dom = Dom::parse("<a href=\"x&amp;y\">t</a>");
        let a = dom.find_descendant(dom.document(), "a").expect("a");
        assert_eq!(dom.attr(a, "href"), Some("x&y"));
        let mut s = String::new();
        dom.text_content(a, &mut s, &|_| false);
        assert_eq!(s, "t");
    }
}
