// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the render tree asks the outside world: the status of each link
//! (from the resolver) and the size of each image (from the image engine).
//!
//! Both are traits so this crate depends on neither the vault nor the
//! media engines. The UI implements them over its resolver and image cache
//! and calls [`RenderTree::refresh`](crate::RenderTree::refresh) when their
//! answers change (the index finished building, an image header was read),
//! or [`RenderTree::refresh_where`](crate::RenderTree::refresh_where) to
//! re-ask only the links and images whose answers can have changed.

use std::fmt;
use std::ops::Range;

use gv_model::LinkKind;
use gv_syntax::BlockId;

/// The resolution status of a link, as the resolver reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkStatus {
    /// Resolved exactly as the profile's tool would.
    Exact,
    /// Matched only by case folding or NFC normalization.
    CaseFold,
    /// Matched with leniency the profile's tool does not have.
    Loose,
    /// Several candidates (their number); the best one was chosen.
    Ambiguous(u32),
    /// The index is still building.
    Pending,
    /// No target.
    Unresolved,
    /// The target escapes the link root.
    OutsideRoot,
    /// A URL handed to the operating system.
    External,
    /// Disallowed by policy.
    Blocked,
}

impl fmt::Display for LinkStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact => f.write_str("exact"),
            Self::CaseFold => f.write_str("case-fold"),
            Self::Loose => f.write_str("loose"),
            Self::Ambiguous(n) => write!(f, "ambiguous({n})"),
            Self::Pending => f.write_str("pending"),
            Self::Unresolved => f.write_str("unresolved"),
            Self::OutsideRoot => f.write_str("outside-root"),
            Self::External => f.write_str("external"),
            Self::Blocked => f.write_str("blocked"),
        }
    }
}

/// A link or image as the render tree sees it while building a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTarget<'a> {
    /// How the destination was given.
    pub kind: LinkKind,
    /// An image rather than a link.
    pub image: bool,
    /// The destination as the resolver takes it: escapes and entities
    /// decoded; email autolinks with `mailto:` (as
    /// [`gv_model::Link::destination`]).
    pub destination: &'a str,
    /// The title, decoded.
    pub title: Option<&'a str>,
    /// The whole construct in the document (as [`gv_model::Link::range`]:
    /// `DocModel::link_at(range.start)` finds the model's record).
    pub range: Range<usize>,
    /// The block it is in.
    pub block: BlockId,
}

/// Gives the status of each link. Implemented by the UI over the resolver.
pub trait LinkStatusSource {
    /// The status of `link`.
    fn status(&self, link: &LinkTarget<'_>) -> LinkStatus;
}

impl<F: Fn(&LinkTarget<'_>) -> LinkStatus> LinkStatusSource for F {
    fn status(&self, link: &LinkTarget<'_>) -> LinkStatus {
        self(link)
    }
}

/// Every link is [`LinkStatus::Pending`]: what the preview shows before
/// the index exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllPending;

impl LinkStatusSource for AllPending {
    fn status(&self, _: &LinkTarget<'_>) -> LinkStatus {
        LinkStatus::Pending
    }
}

/// What is known about an image's size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageSize {
    /// Intrinsic size in px (from the file header or `|WxH`).
    Known { width: f32, height: f32 },
    /// Not known yet (loading).
    Pending,
    /// The image cannot be shown (missing, blocked, undecodable).
    Failed,
}

/// Gives the size of each image. Implemented by the UI over the image
/// engine's header probe.
pub trait ImageSizeSource {
    /// The size of `image` (`image.image` is always true).
    fn size(&self, image: &LinkTarget<'_>) -> ImageSize;
}

impl<F: Fn(&LinkTarget<'_>) -> ImageSize> ImageSizeSource for F {
    fn size(&self, image: &LinkTarget<'_>) -> ImageSize {
        self(image)
    }
}

/// Every image size is [`ImageSize::Pending`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NoImageSizes;

impl ImageSizeSource for NoImageSizes {
    fn size(&self, _: &LinkTarget<'_>) -> ImageSize {
        ImageSize::Pending
    }
}

/// The injected sources of a build.
#[derive(Clone, Copy)]
pub struct Sources<'a> {
    pub links: &'a dyn LinkStatusSource,
    pub images: &'a dyn ImageSizeSource,
}

impl fmt::Debug for Sources<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sources").finish_non_exhaustive()
    }
}

impl Default for Sources<'static> {
    fn default() -> Self {
        Self {
            links: &AllPending,
            images: &NoImageSizes,
        }
    }
}
