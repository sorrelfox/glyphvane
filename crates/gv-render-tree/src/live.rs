// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live blocks: Obsidian constructs whose content the view fills in
//! itself, asynchronously, from outside the document.
//!
//! | Construct | Kind |
//! |---|---|
//! | `![[x.canvas]]` (a paragraph holding only it) | [`LiveKind::Canvas`] |
//! | `![[x.base]]`, `![[x.base#View]]` | [`LiveKind::Base`] |
//! | a ` ```base ` fence | [`LiveKind::Base`] |
//! | a ` ```query ` fence (a search embed) | [`LiveKind::Query`] |
//!
//! The render tree never reads the target nor runs anything: a live block
//! is one virtual item ([`ItemKind::Live`](crate::ItemKind::Live)) that
//! says what to show ([`LiveBlock`]), with a stable [`LiveKey`] the view
//! keeps the content under (across rebuilds, zoom changes and edits
//! elsewhere) and a reserved height ([`LiveKind::reserved_height`]) the
//! item is estimated at until the view has measured what landed. The view
//! keeps its scroll anchor while a block's height changes, as for
//! diagrams and images.
//!
//! Embeds are made where note embeds are allowed (the profile's `embeds`
//! switch); fences with [`RenderOptions::obsidian_fences`](crate::RenderOptions).
//! Inside a note embed (transclusion) a live block maps to the end of the
//! embed's line like the other embedded blocks, carries the embedding
//! note's key ([`LiveBlock::host`]) and counts towards the embed depth
//! limit ([`MAX_EMBED_DEPTH`](crate::MAX_EMBED_DEPTH)).
//!
//! Copy and find see the embed's own line (its link text) or the fence's
//! lines; exports fall back to the same (a link, or the code block).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use crate::node::{CodeBlock, InlineContent};

/// A canvas or base embed's reserved height, in px at zoom 1: the block's
/// header row and its content area.
pub const LIVE_EMBED_HEIGHT: f32 = 400.0;
/// A query block's reserved height, in px at zoom 1 (the header and a few
/// result rows; it grows or shrinks to its results).
pub const LIVE_QUERY_HEIGHT: f32 = 160.0;

/// What a live block shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiveKind {
    /// A `.canvas` file, drawn as shapes.
    Canvas,
    /// A base (a `.base` file or a ` ```base ` fence), interactive.
    Base,
    /// The results of a search query (a ` ```query ` fence), read-only.
    Query,
}

impl LiveKind {
    /// The height the block is estimated at before its content lands, in
    /// px at zoom 1.
    pub fn reserved_height(self) -> f32 {
        match self {
            Self::Canvas | Self::Base => LIVE_EMBED_HEIGHT,
            Self::Query => LIVE_QUERY_HEIGHT,
        }
    }

    /// A short name, for dumps.
    pub fn name(self) -> &'static str {
        match self {
            Self::Canvas => "canvas",
            Self::Base => "base",
            Self::Query => "query",
        }
    }

    /// The kind of an embed of `path` (what is between the brackets
    /// before `#` and `|`): `.canvas` and `.base` files, any case.
    pub fn of_embed(path: &str) -> Option<Self> {
        let path = path.trim();
        let ext = path.rsplit_once('.')?.1;
        if ext.eq_ignore_ascii_case("canvas") {
            Some(Self::Canvas)
        } else if ext.eq_ignore_ascii_case("base") {
            Some(Self::Base)
        } else {
            None
        }
    }

    /// The kind of a fence whose info string's first word is `language`.
    pub fn of_fence(language: &str) -> Option<Self> {
        match language {
            "base" => Some(Self::Base),
            "query" => Some(Self::Query),
            _ => None,
        }
    }
}

/// Identifies a live block's content: the same construct (kind, target or
/// fence text, embedding note) has the same key in every build, so the
/// view keeps what it fetched across rebuilds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LiveKey(pub u64);

/// A live block.
#[derive(Debug, Clone)]
pub struct LiveBlock {
    pub kind: LiveKind,
    /// An embed's target: what is between the brackets before `#` and
    /// `|`, trimmed (`Boards/Plan.canvas`). `None` for a fence.
    pub target: Option<Arc<str>>,
    /// An embed's anchor (after `#`): the view of a base to show.
    pub anchor: Option<Arc<str>>,
    /// A fence's lines (the base's YAML, the query).
    pub code: Option<Arc<CodeBlock>>,
    /// An embed's own line (its link): what copy and find see, and what
    /// exports show.
    pub label: Option<Arc<InlineContent>>,
    /// The key ([`EmbedFetch::Note`](crate::EmbedFetch)) of the note the
    /// block is in when it is inside a note embed; `None`: the document.
    pub host: Option<Arc<str>>,
    /// Number of note embeds around it.
    pub depth: u16,
    pub key: LiveKey,
}

impl LiveBlock {
    /// A live block; its key is computed from what it shows.
    pub(crate) fn new(
        kind: LiveKind,
        target: Option<Arc<str>>,
        anchor: Option<Arc<str>>,
        code: Option<Arc<CodeBlock>>,
        label: Option<Arc<InlineContent>>,
        host: Option<Arc<str>>,
        depth: u16,
    ) -> Self {
        let mut h = DefaultHasher::new();
        kind.hash(&mut h);
        target.hash(&mut h);
        anchor.hash(&mut h);
        code.as_ref().map(|c| &c.text).hash(&mut h);
        host.hash(&mut h);
        Self {
            kind,
            target,
            anchor,
            code,
            label,
            host,
            depth,
            key: LiveKey(h.finish()),
        }
    }

    /// The fence's text (the base's YAML, the query), if it is a fence.
    pub fn fence_text(&self) -> Option<&str> {
        self.code.as_deref().map(|c| c.text.as_str())
    }

    /// The text that stands for the block where it cannot be shown: the
    /// embed as written (`![[x.canvas]]`), or the fence's text.
    pub fn fallback_text(&self) -> String {
        match (&self.target, &self.code) {
            (Some(t), _) => match &self.anchor {
                Some(a) => format!("![[{t}#{a}]]"),
                None => format!("![[{t}]]"),
            },
            (None, Some(c)) => c.text.clone(),
            (None, None) => String::new(),
        }
    }
}

/// Splits what is between the brackets of `![[…]]` into its path and its
/// anchor (the alias after `|` is dropped).
pub(crate) fn split_embed(inner: &str) -> (&str, Option<&str>) {
    let before_alias = inner.split('|').next().unwrap_or("");
    match before_alias.split_once('#') {
        Some((path, anchor)) => (path.trim(), Some(anchor.trim()).filter(|a| !a.is_empty())),
        None => (before_alias.trim(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_come_from_the_extension_and_the_fence_language() {
        assert_eq!(LiveKind::of_embed("Plan.canvas"), Some(LiveKind::Canvas));
        assert_eq!(LiveKind::of_embed("a/B.CANVAS "), Some(LiveKind::Canvas));
        assert_eq!(LiveKind::of_embed("Books.base"), Some(LiveKind::Base));
        assert_eq!(LiveKind::of_embed("Note"), None);
        assert_eq!(LiveKind::of_embed("note.md"), None);
        assert_eq!(LiveKind::of_fence("base"), Some(LiveKind::Base));
        assert_eq!(LiveKind::of_fence("query"), Some(LiveKind::Query));
        assert_eq!(LiveKind::of_fence("rust"), None);
    }

    #[test]
    fn embeds_split_into_path_and_anchor() {
        assert_eq!(
            split_embed("Books.base#Table"),
            ("Books.base", Some("Table"))
        );
        assert_eq!(split_embed("Books.base|alias"), ("Books.base", None));
        assert_eq!(split_embed(" x.canvas # "), ("x.canvas", None));
    }

    #[test]
    fn keys_follow_the_content() {
        let a = LiveBlock::new(
            LiveKind::Base,
            Some("x.base".into()),
            None,
            None,
            None,
            None,
            0,
        );
        let b = LiveBlock::new(
            LiveKind::Base,
            Some("x.base".into()),
            None,
            None,
            None,
            None,
            0,
        );
        let c = LiveBlock::new(
            LiveKind::Base,
            Some("x.base".into()),
            Some("View".into()),
            None,
            None,
            None,
            0,
        );
        let d = LiveBlock::new(
            LiveKind::Base,
            Some("x.base".into()),
            None,
            None,
            None,
            Some("Other.md".into()),
            1,
        );
        assert_eq!(a.key, b.key);
        assert_ne!(a.key, c.key);
        assert_ne!(a.key, d.key);
        assert_eq!(c.fallback_text(), "![[x.base#View]]");
    }
}
