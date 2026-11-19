// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The virtual list engine of the Reading view, free of GPUI types: a
//! height index over all items, a scroll anchor `(item id, px offset in the
//! item)`, and a frame pass that lays out only the items in the viewport
//! plus some overdraw.
//!
//! - Heights above the anchor may change at any time (a link status
//!   changes, a code block is wrapped): visible content is positioned from
//!   the anchor, so it does not move; only the scrollbar does.
//! - On a width change the anchor becomes a **text anchor** (the source
//!   offset of the line at the top, and the px below that line's top)
//!   before re-wrapping, and back afterwards. The text anchor is kept across
//!   consecutive width changes until the user scrolls, so a live resize does
//!   not creep.
//! - Scroll requests (wheel, scrollbar, jumps) are recorded and resolved by
//!   the next frame. Deltas larger than the viewport go through the height
//!   index, so a big jump lays out only the new viewport.
//! - Measured heights are written to the height index in one batched update
//!   at the end of each frame.
//! - When the document is updated in place (the Split view's preview
//!   follows the editor), the replaced items' heights are spliced into the
//!   index as estimates ([`Engine::splice`]) and the anchor stays on its
//!   item. Scroll sync asks for a place inside an item and, optionally, for
//!   a block to be shown whole ([`Engine::sync_to`]).
//!
//! Document coordinates start at the top of the document area: the first
//! item's box starts `lead` px down (the column's top padding).

use std::ops::Range;

use gv_render_tree::ItemId;

use super::heights::{Heights, Slot};

/// Where a jump target arrives, as a fraction of the viewport from its top.
const ARRIVAL: f32 = 0.25;

/// A position inside an item as text: the source offset at the start of
/// the line there and the px below that line's top. Survives re-wrapping.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TextAnchor {
    pub source: usize,
    pub delta: f32,
}

/// The items of a document, as the engine sees them.
pub(crate) trait Items {
    fn len(&self) -> usize;
    fn id(&self, ix: usize) -> ItemId;
    fn index_of(&self, id: ItemId) -> Option<usize>;
    /// Estimated height of item `ix` at `width`, gap included, without
    /// shaping.
    fn estimate(&self, ix: usize, width: f32) -> f32;
    /// Lays item `ix` out at `width` (cached) and returns its height, gap
    /// included.
    fn measure(&mut self, ix: usize, width: f32) -> f32;
    /// The text position `offset` px below item `ix`'s top, in the layout
    /// it was last painted with.
    fn text_anchor(&mut self, ix: usize, offset: f32) -> Option<TextAnchor>;
    /// Where `anchor` is in item `ix` laid out at `width`: px below its
    /// top.
    fn offset_of(&mut self, ix: usize, anchor: &TextAnchor, width: f32) -> Option<f32>;
}

/// The scroll position: `offset` px from the item's top to the viewport
/// top. Normalised by every frame to `0 <= offset < height`, except at the
/// document's ends (`-lead <= offset` on the first item).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Anchor {
    pub id: ItemId,
    pub offset: f32,
}

/// An item placed in the viewport; `y` from the viewport top to the top of
/// the item's box (its gap included).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placed {
    pub ix: usize,
    pub id: ItemId,
    pub y: f32,
    pub height: f32,
}

/// A scroll request for the next frame.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Target {
    /// A document y.
    Y(f64),
    /// `offset` px below item `ix`'s top at `above` of the viewport's
    /// height below the viewport top.
    Item { ix: usize, offset: f32, above: f32 },
}

/// The scrollbar thumb, in px of the track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Thumb {
    pub top: f32,
    pub height: f32,
}

#[derive(Debug, Default)]
pub(crate) struct Engine {
    heights: Heights,
    anchor: Option<Anchor>,
    /// Index of the anchor item at the last frame.
    anchor_ix: usize,
    width: f32,
    viewport: f32,
    lead: f32,
    overdraw: f32,
    target: Option<Target>,
    text_anchor: Option<(ItemId, TextAnchor)>,
    pending: Vec<(usize, Slot)>,
    placed: Vec<Placed>,
    /// The total height while the scrollbar is dragged, so the thumb stays
    /// under the pointer while measured heights replace estimates.
    frozen_total: Option<f64>,
    /// Width changes handled.
    pub rewraps: u64,
}

impl Engine {
    /// An engine for a document whose first item starts `lead` px down,
    /// measuring `overdraw` px beyond each edge of the viewport.
    pub fn new(lead: f32, overdraw: f32) -> Self {
        Self {
            lead,
            overdraw,
            ..Self::default()
        }
    }

    /// Starts over with the items of a new document, scrolled to the top.
    pub fn reset(&mut self, items: &impl Items, width: f32) {
        self.width = width;
        self.heights = estimates(items, width);
        self.pending.clear();
        self.placed.clear();
        self.text_anchor = None;
        self.frozen_total = None;
        self.anchor_ix = 0;
        self.anchor = (items.len() > 0).then(|| Anchor {
            id: items.id(0),
            offset: -self.lead,
        });
    }

    /// Items `changed` have new content: their heights become estimates
    /// until they are laid out again.
    pub fn invalidate(&mut self, items: &impl Items, changed: impl IntoIterator<Item = usize>) {
        let width = self.width;
        self.pending.extend(changed.into_iter().map(|ix| {
            (
                ix,
                Slot {
                    height: items.estimate(ix, width),
                    measured: false,
                },
            )
        }));
        self.heights.write(&mut self.pending);
    }

    pub fn placed(&self) -> &[Placed] {
        &self.placed
    }

    pub fn anchor(&self) -> Option<Anchor> {
        self.anchor
    }

    pub fn viewport(&self) -> f32 {
        self.viewport
    }

    /// The px above the first item.
    pub fn lead(&self) -> f32 {
        self.lead
    }

    /// The height of item `ix` in the height index (measured, or an
    /// estimate), gap included.
    pub fn height_of(&self, ix: usize) -> Option<f32> {
        self.heights.get(ix).map(|s| s.height)
    }

    /// The document was updated in place: items `removed` of the old list
    /// were replaced by items `inserted` of the new one (`items`), and the
    /// content of items `changed` is new. Their heights become estimates;
    /// the others keep theirs. The anchor stays on its item, or goes to
    /// the item that took its place.
    pub fn splice(
        &mut self,
        items: &impl Items,
        removed: Range<usize>,
        inserted: Range<usize>,
        changed: impl IntoIterator<Item = usize>,
    ) {
        self.heights.write(&mut self.pending);
        self.placed.clear();
        let width = self.width;
        let n = items.len();
        let old = self.heights.len();
        let removed = removed.start.min(old)..removed.end.min(old).max(removed.start.min(old));
        let added = inserted.len();
        self.heights.splice(
            removed.clone(),
            inserted.clone().map(|ix| Slot {
                height: items.estimate(ix, width),
                measured: false,
            }),
        );
        if self.heights.len() != n {
            // Not a splice of these items: start over where the anchor was.
            self.heights = estimates(items, width);
        }
        let changed: Vec<usize> = changed
            .into_iter()
            .filter(|ix| *ix < n && !inserted.contains(ix))
            .collect();
        self.invalidate(items, changed);
        if self
            .text_anchor
            .is_some_and(|(id, _)| items.index_of(id).is_none())
        {
            self.text_anchor = None;
        }
        let Some(anchor) = self.anchor else {
            return;
        };
        if n == 0 {
            self.anchor = None;
            return;
        }
        let ix = match items.index_of(anchor.id) {
            Some(ix) => ix,
            None => {
                let was = self.anchor_ix;
                let ix = if was < removed.start {
                    was
                } else if was >= removed.end {
                    (was + added).saturating_sub(removed.len())
                } else {
                    inserted.start + (was - removed.start).min(added.saturating_sub(1))
                };
                let ix = ix.min(n - 1);
                self.anchor = Some(Anchor {
                    id: items.id(ix),
                    offset: if ix == 0 {
                        anchor.offset
                    } else {
                        anchor.offset.max(0.0)
                    },
                });
                ix
            }
        };
        self.anchor_ix = ix;
    }

    /// Scrolls so that `offset` px below item `ix`'s top is at the viewport
    /// top; then, when `reveal` is given, by as little as shows items
    /// `reveal` whole, `margin` px clear of the edges (their top, when they
    /// are taller than the viewport). The items in between are measured as
    /// far as two viewports; a block further away goes to the top.
    pub fn sync_to(
        &mut self,
        items: &mut impl Items,
        ix: usize,
        offset: f32,
        reveal: Option<Range<usize>>,
        margin: f32,
    ) {
        self.text_anchor = None;
        let n = items.len();
        if n == 0 {
            return;
        }
        let ix = ix.min(n - 1);
        let mut top = offset;
        if let Some(r) = reveal.filter(|r| r.start < r.end && r.end <= n) {
            let viewport = self.viewport;
            let reach = 2.0 * viewport.max(1.0);
            let last = r.end - 1;
            let start = self.relative_top(items, ix, r.start, reach);
            let end = self
                .relative_top(items, ix, last, reach)
                .map(|y| y + self.measure(items, last));
            let (Some(start), Some(end)) = (start, end) else {
                self.target = Some(Target::Item {
                    ix: r.start,
                    offset: -margin,
                    above: 0.0,
                });
                return;
            };
            if end - top > viewport - margin {
                top = end - viewport + margin;
            }
            if start - margin < top {
                top = start - margin;
            }
        }
        self.target = Some(Target::Item {
            ix,
            offset: top,
            above: 0.0,
        });
    }

    /// The top of item `to` below the top of item `from`, measuring the
    /// items between; `None` when it is more than `reach` px away.
    fn relative_top(
        &mut self,
        items: &mut impl Items,
        from: usize,
        to: usize,
        reach: f32,
    ) -> Option<f32> {
        let mut y = 0.0;
        if to >= from {
            for j in from..to {
                y += self.measure(items, j);
                if y > reach {
                    return None;
                }
            }
        } else {
            for j in (to..from).rev() {
                y -= self.measure(items, j);
                if -y > reach {
                    return None;
                }
            }
        }
        Some(y)
    }

    fn measure(&mut self, items: &mut impl Items, ix: usize) -> f32 {
        let height = items.measure(ix, self.width);
        let slot = Slot {
            height,
            measured: true,
        };
        if self.heights.get(ix) != Some(slot) {
            self.pending.push((ix, slot));
        }
        height
    }

    /// One frame: re-wrap if the width changed, apply the scroll request,
    /// place the items that intersect `[0, viewport)` and measure the
    /// overdraw.
    pub fn frame(&mut self, items: &mut impl Items, width: f32, viewport: f32) -> &[Placed] {
        let n = items.len();
        self.placed.clear();
        if n == 0 || self.heights.len() != n {
            if self.heights.len() != n {
                self.reset(items, width);
            }
            if n == 0 {
                return &self.placed;
            }
        }
        if (width - self.width).abs() > 0.01 {
            self.set_width(items, width);
        }
        self.viewport = viewport;
        self.resolve_target(items);
        let Some(anchor) = self.anchor else {
            return &self.placed;
        };
        let mut ix = items.index_of(anchor.id).unwrap_or(0).min(n - 1);
        let mut off = anchor.offset;
        // Normalise: negative offsets walk up, offsets past the item down.
        while off < 0.0 {
            if ix == 0 {
                off = off.max(-self.lead);
                break;
            }
            ix -= 1;
            off += self.measure(items, ix);
        }
        loop {
            let h = self.measure(items, ix);
            if off < h || ix + 1 >= n {
                break;
            }
            off -= h;
            ix += 1;
        }
        // Place; if the end of the document leaves a gap, pull the anchor
        // up once and place again.
        let mut placed = std::mem::take(&mut self.placed);
        for attempt in 0..2 {
            placed.clear();
            let mut y = -off;
            let mut j = ix;
            while j < n && y < viewport + self.overdraw {
                let h = self.measure(items, j);
                if y < viewport && y + h > 0.0 {
                    placed.push(Placed {
                        ix: j,
                        id: items.id(j),
                        y,
                        height: h,
                    });
                }
                y += h;
                j += 1;
            }
            let gap = viewport - y;
            if attempt == 0 && j >= n && gap > 0.0 && (ix > 0 || off > -self.lead) {
                off -= gap;
                while off < 0.0 && ix > 0 {
                    ix -= 1;
                    off += self.measure(items, ix);
                }
                off = off.max(-self.lead);
                continue;
            }
            break;
        }
        self.placed = placed;
        // Leading overdraw, so that scrolling up finds measured items.
        let mut above = off.max(0.0);
        let mut k = ix;
        while above < self.overdraw && k > 0 {
            k -= 1;
            above += self.measure(items, k);
        }
        self.anchor = Some(Anchor {
            id: items.id(ix),
            offset: off,
        });
        self.anchor_ix = ix;
        self.heights.write(&mut self.pending);
        &self.placed
    }

    fn resolve_target(&mut self, items: &impl Items) {
        let Some(target) = self.target.take() else {
            return;
        };
        let n = items.len();
        self.anchor = Some(match target {
            Target::Item { ix, offset, above } => {
                let ix = ix.min(n - 1);
                Anchor {
                    id: items.id(ix),
                    offset: offset - above * self.viewport,
                }
            }
            Target::Y(y) => {
                let lead = f64::from(self.lead);
                // Not past the end of the height index: Scroll to Bottom
                // asks for a y far beyond it, whose offset into the last
                // item would not fit an f32 (an infinite offset, then a NaN
                // one, would leave the last item at the top). From the end,
                // the frame pulls the last item down to the bottom.
                let y = y.clamp(0.0, lead + self.heights.total());
                if y < lead {
                    Anchor {
                        id: items.id(0),
                        offset: (y - lead) as f32,
                    }
                } else {
                    let (ix, off) = self.heights.at_y(y - lead);
                    Anchor {
                        id: items.id(ix.min(n - 1)),
                        offset: off as f32,
                    }
                }
            }
        });
    }

    /// Re-wraps at a new width: keeps the anchor on the same text line and
    /// replaces every height by the estimate at the new width (the visible
    /// items are measured again in the same frame).
    fn set_width(&mut self, items: &mut impl Items, width: f32) {
        self.heights.write(&mut self.pending);
        self.rewraps += 1;
        let n = items.len();
        let Some(anchor) = self.anchor else {
            self.width = width;
            self.heights = estimates(items, width);
            return;
        };
        let ix = items.index_of(anchor.id).unwrap_or(0).min(n - 1);
        let text = match self.text_anchor {
            Some((id, a)) if id == anchor.id => Some(a),
            _ if anchor.offset >= 0.0 => items.text_anchor(ix, anchor.offset),
            _ => None,
        };
        self.text_anchor = text.map(|a| (anchor.id, a));
        let old = self.heights.get(ix).map_or(0.0, |s| s.height);
        self.width = width;
        self.heights = estimates(items, width);
        let new = self.measure(items, ix);
        let offset = match text.and_then(|a| items.offset_of(ix, &a, width)) {
            Some(o) => o,
            None if anchor.offset < 0.0 => anchor.offset,
            None if old > 0.0 => anchor.offset / old * new,
            None => 0.0,
        };
        self.anchor = Some(Anchor {
            id: anchor.id,
            offset,
        });
        self.heights.write(&mut self.pending);
    }

    // ----- scrolling --------------------------------------------------------

    /// Scrolls by `dy` px (positive: down).
    pub fn scroll_by(&mut self, dy: f32) {
        self.text_anchor = None;
        match &mut self.target {
            Some(Target::Y(y)) => *y += f64::from(dy),
            Some(Target::Item { offset, .. }) => *offset += dy,
            None if self.viewport > 0.0 && dy.abs() > self.viewport => {
                self.target = Some(Target::Y(self.scroll_top() + f64::from(dy)));
            }
            None => {
                if let Some(a) = &mut self.anchor {
                    a.offset += dy;
                }
            }
        }
    }

    /// Scrolls so that document y `y` is at the viewport top.
    pub fn scroll_to_y(&mut self, y: f64) {
        self.text_anchor = None;
        self.target = Some(Target::Y(y));
    }

    /// Scrolls so that `offset` px below item `ix`'s top is at the viewport
    /// top (a negative offset shows what is above it).
    pub fn jump_to(&mut self, ix: usize, offset: f32) {
        self.text_anchor = None;
        self.target = Some(Target::Item {
            ix,
            offset,
            above: 0.0,
        });
    }

    /// Scrolls so that `offset` px below item `ix`'s top is a quarter of
    /// the viewport down, where a followed link's target arrives.
    pub fn jump_near_top(&mut self, ix: usize, offset: f32) {
        self.text_anchor = None;
        self.target = Some(Target::Item {
            ix,
            offset,
            above: ARRIVAL,
        });
    }

    /// Scrolls so that `offset` px below item `ix`'s top is in the middle
    /// of the viewport (a find match that was off screen).
    pub fn jump_centred(&mut self, ix: usize, offset: f32) {
        self.text_anchor = None;
        self.target = Some(Target::Item {
            ix,
            offset,
            above: 0.5,
        });
    }

    /// Document y of the viewport top, from the height index (as of the
    /// last frame).
    pub fn scroll_top(&self) -> f64 {
        let Some(a) = self.anchor else {
            return 0.0;
        };
        f64::from(self.lead) + self.heights.top_of(self.anchor_ix) + f64::from(a.offset)
    }

    /// Height of the whole document, from measured and estimated heights.
    pub fn total(&self) -> f64 {
        self.frozen_total
            .unwrap_or_else(|| f64::from(self.lead) + self.heights.total())
    }

    /// The scrollbar thumb in a track `track` px long and at least
    /// `min` px long; `None` when everything fits.
    pub fn thumb(&self, track: f32, min: f32) -> Option<Thumb> {
        let total = self.total();
        let viewport = f64::from(self.viewport);
        if self.anchor.is_none() || total <= viewport + 0.5 || track <= 0.0 {
            return None;
        }
        let size = (viewport / total).min(1.0);
        let top = (self.scroll_top() / total).clamp(0.0, 1.0 - size);
        let height = (size as f32 * track).max(min).min(track);
        // The thumb's travel is the track minus its own length.
        let travel = track - height;
        let frac = if size < 1.0 { top / (1.0 - size) } else { 0.0 };
        Some(Thumb {
            top: frac as f32 * travel,
            height,
        })
    }

    /// The scrollbar is grabbed: the total stops changing until
    /// [`Self::end_drag`].
    pub fn begin_drag(&mut self) {
        self.frozen_total = Some(self.total());
    }

    pub fn end_drag(&mut self) {
        self.frozen_total = None;
    }

    /// Scrolls so that the thumb's top is at `top` px of a `track` px track
    /// with a `thumb` px thumb.
    pub fn drag_thumb_to(&mut self, top: f32, track: f32, thumb: f32) {
        let total = self.total();
        let viewport = f64::from(self.viewport);
        let travel = f64::from((track - thumb).max(1.0));
        let frac = (f64::from(top) / travel).clamp(0.0, 1.0);
        if frac >= 1.0 {
            // The thumb at the bottom shows the end, however far off the
            // estimates are.
            self.scroll_to_end();
        } else {
            self.scroll_to_y(frac * (total - viewport).max(0.0));
        }
    }

    /// Scrolls to the end of the document.
    pub fn scroll_to_end(&mut self) {
        self.scroll_to_y(f64::MAX / 4.0);
    }
}

fn estimates(items: &impl Items, width: f32) -> Heights {
    Heights::new((0..items.len()).map(|ix| Slot {
        height: items.estimate(ix, width),
        measured: false,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use gv_syntax::BlockId;

    use super::*;

    /// Items of text that wrap at 5 px per character, 10 px per line; each
    /// item's source starts at `1000 * ix`. Estimates are 10 % low.
    struct Fake {
        chars: Vec<u32>,
        /// The block id of each item.
        ids: Vec<u64>,
        gap: f32,
        /// Width the item was last measured at (its "painted" layout).
        laid_out: HashMap<usize, f32>,
        measures: usize,
    }

    const LINE: f32 = 10.0;
    const ADVANCE: f32 = 5.0;

    impl Fake {
        fn new(chars: Vec<u32>) -> Self {
            Self {
                ids: (0..chars.len() as u64).collect(),
                chars,
                gap: 4.0,
                laid_out: HashMap::new(),
                measures: 0,
            }
        }

        fn per_line(width: f32) -> u32 {
            ((width / ADVANCE).floor() as u32).max(1)
        }

        fn lines(&self, ix: usize, width: f32) -> u32 {
            self.chars[ix].div_ceil(Self::per_line(width)).max(1)
        }
    }

    impl Items for Fake {
        fn len(&self) -> usize {
            self.chars.len()
        }

        fn id(&self, ix: usize) -> ItemId {
            ItemId {
                block: BlockId(self.ids[ix]),
                chunk: 0,
            }
        }

        fn index_of(&self, id: ItemId) -> Option<usize> {
            let ix = id.block.0 as usize;
            if self.ids.get(ix) == Some(&id.block.0) {
                return Some(ix);
            }
            self.ids.iter().position(|&b| b == id.block.0)
        }

        fn estimate(&self, ix: usize, width: f32) -> f32 {
            0.9 * (self.gap + self.lines(ix, width) as f32 * LINE)
        }

        fn measure(&mut self, ix: usize, width: f32) -> f32 {
            self.measures += 1;
            self.laid_out.insert(ix, width);
            self.gap + self.lines(ix, width) as f32 * LINE
        }

        fn text_anchor(&mut self, ix: usize, offset: f32) -> Option<TextAnchor> {
            let width = *self.laid_out.get(&ix)?;
            let local = (offset - self.gap).max(0.0);
            let line = (local / LINE).floor() as u32;
            Some(TextAnchor {
                source: 1000 * ix + (line * Self::per_line(width)) as usize,
                delta: offset - self.gap - line as f32 * LINE,
            })
        }

        fn offset_of(&mut self, ix: usize, a: &TextAnchor, width: f32) -> Option<f32> {
            let chars = a.source.checked_sub(1000 * ix)? as u32;
            let line = chars / Self::per_line(width);
            Some(self.gap + line as f32 * LINE + a.delta)
        }
    }

    fn engine(items: &mut Fake, width: f32, viewport: f32) -> Engine {
        let mut e = Engine::new(20.0, 50.0);
        e.reset(items, width);
        e.frame(items, width, viewport);
        e
    }

    /// The source offset of the text line at the viewport top, and its y.
    fn top_line(e: &Engine, items: &mut Fake) -> (usize, f32) {
        let a = e.anchor().expect("anchor");
        let ix = items.index_of(a.id).expect("item");
        let t = items.text_anchor(ix, a.offset).expect("text");
        (t.source, -t.delta)
    }

    #[test]
    fn starts_at_the_top_below_the_lead() {
        let mut items = Fake::new(vec![100; 50]);
        let e = engine(&mut items, 200.0, 100.0);
        let first = e.placed()[0];
        assert_eq!((first.ix, first.y), (0, 20.0));
        assert_eq!(e.scroll_top(), 0.0);
        // 100 chars at 40 per line: 3 lines + gap.
        assert_eq!(first.height, 34.0);
        let last = e.placed().last().expect("placed");
        assert!(last.y < 100.0 && last.y + last.height >= 100.0);
    }

    #[test]
    fn scrolling_is_clamped_at_both_ends() {
        let mut items = Fake::new(vec![100; 20]);
        let mut e = engine(&mut items, 200.0, 100.0);
        e.scroll_by(-500.0);
        e.frame(&mut items, 200.0, 100.0);
        assert_eq!(e.placed()[0].y, 20.0);
        e.scroll_by(1e6);
        e.frame(&mut items, 200.0, 100.0);
        let last = e.placed().last().expect("placed");
        assert_eq!(last.ix, 19);
        assert!((last.y + last.height - 100.0).abs() < 1e-3, "{last:?}");
        // A document shorter than the viewport stays at the top.
        let mut short = Fake::new(vec![10; 2]);
        let mut e = engine(&mut short, 200.0, 500.0);
        e.scroll_by(300.0);
        e.frame(&mut short, 200.0, 500.0);
        assert_eq!(e.placed()[0].y, 20.0);
        assert!(e.thumb(500.0, 24.0).is_none());
    }

    #[test]
    fn scroll_to_end_shows_the_end_at_the_bottom() {
        let mut items = Fake::new(vec![100; 300]);
        let mut e = engine(&mut items, 200.0, 100.0);
        // From the top, where the items near the end are only estimated,
        // and from the end itself.
        for _ in 0..2 {
            e.scroll_to_end();
            e.frame(&mut items, 200.0, 100.0);
            let last = e.placed().last().expect("placed");
            assert_eq!(last.ix, 299);
            assert!((last.y + last.height - 100.0).abs() < 1e-3, "{last:?}");
        }
        // The thumb dragged to the bottom of its track.
        e.scroll_to_y(0.0);
        e.frame(&mut items, 200.0, 100.0);
        e.drag_thumb_to(1000.0, 100.0, 24.0);
        e.frame(&mut items, 200.0, 100.0);
        let last = e.placed().last().expect("placed");
        assert_eq!(last.ix, 299);
        assert!((last.y + last.height - 100.0).abs() < 1e-3, "{last:?}");
    }

    #[test]
    fn heights_above_the_anchor_do_not_move_the_content() {
        let mut items = Fake::new(vec![100; 200]);
        let mut e = engine(&mut items, 200.0, 100.0);
        e.jump_to(100, 7.0);
        e.frame(&mut items, 200.0, 100.0);
        let before = e.placed()[0];
        let top_before = e.scroll_top();
        // Item 50 grows (as when a code block is wrapped).
        items.chars[50] = 4000;
        e.invalidate(&items, [50]);
        e.frame(&mut items, 200.0, 100.0);
        assert_eq!(e.placed()[0], before);
        assert!(e.scroll_top() > top_before);
    }

    #[test]
    fn a_jump_lays_out_only_the_viewport() {
        let mut items = Fake::new(vec![100; 10_000]);
        let mut e = engine(&mut items, 200.0, 100.0);
        items.measures = 0;
        e.jump_to(8_000, 0.0);
        e.frame(&mut items, 200.0, 100.0);
        assert_eq!(e.placed()[0].ix, 8_000);
        assert!(items.measures < 40, "{}", items.measures);
        // Big wheel deltas go through the height index as well.
        items.measures = 0;
        e.scroll_by(-50_000.0);
        e.frame(&mut items, 200.0, 100.0);
        assert!(items.measures < 40, "{}", items.measures);
        assert!(e.placed()[0].ix < 8_000);
    }

    #[test]
    fn width_changes_keep_the_top_line() {
        let mut items = Fake::new(vec![300; 100]);
        let mut e = engine(&mut items, 400.0, 100.0);
        // 3 lines and 4 px into item 40.
        e.jump_to(40, 4.0 + 3.0 * LINE + 4.0);
        e.frame(&mut items, 400.0, 100.0);
        let (source, y) = top_line(&e, &mut items);
        for width in [300.0, 520.0, 333.0, 701.0, 455.0, 400.0] {
            e.frame(&mut items, width, 100.0);
            let a = e.anchor().expect("anchor");
            let ix = items.index_of(a.id).expect("item");
            let t = items.text_anchor(ix, a.offset).expect("text");
            // The line holding the old top line's start is at the top.
            let line_start = t.source;
            let per = Fake::per_line(width) as usize;
            assert!(line_start <= source && source < line_start + per, "{width}");
            assert!((-t.delta - y).abs() < 1e-3, "{width}: {} vs {y}", -t.delta);
        }
        // The round trip restores the exact anchor.
        assert_eq!(top_line(&e, &mut items), (source, y));
        // Scrolling drops the sticky text anchor.
        e.scroll_by(1.0);
        e.frame(&mut items, 400.0, 100.0);
        assert_eq!(top_line(&e, &mut items).1, y - 1.0);
    }

    #[test]
    fn a_splice_keeps_the_anchor_on_its_item() {
        let mut items = Fake::new(vec![100; 100]);
        let mut e = engine(&mut items, 200.0, 100.0);
        e.jump_to(50, 5.0);
        e.frame(&mut items, 200.0, 100.0);
        let before = e.placed()[0];
        // Items 10..12 become three items: one more above the anchor.
        items.chars.insert(10, 300);
        items.ids.splice(10..12, [1000, 1001, 1002]);
        e.splice(&items, 10..12, 10..13, []);
        assert_eq!(e.heights.len(), 101);
        assert_eq!(e.anchor().map(|a| a.id), Some(items.id(51)));
        e.frame(&mut items, 200.0, 100.0);
        assert_eq!(e.placed()[0].ix, 51);
        assert_eq!(e.placed()[0].y, before.y);
    }

    #[test]
    fn sync_to_reveals_a_block_below() {
        let mut items = Fake::new(vec![100; 100]);
        let mut e = engine(&mut items, 200.0, 100.0);
        // Items are 34 px: item 10 at the top would leave item 14 below
        // the viewport.
        e.sync_to(&mut items, 10, 0.0, Some(14..15), 2.0);
        e.frame(&mut items, 200.0, 100.0);
        let below = e.placed().iter().find(|p| p.ix == 14).expect("item 14");
        assert!((below.y + below.height - 98.0).abs() < 1e-3, "{below:?}");
        // A block above the top is shown at the top.
        e.sync_to(&mut items, 10, 0.0, Some(8..9), 2.0);
        e.frame(&mut items, 200.0, 100.0);
        let first = e.placed().iter().find(|p| p.ix == 8).expect("item 8");
        assert!((first.y - 2.0).abs() < 1e-3, "{first:?}");
        // One that is far away goes to the top.
        e.sync_to(&mut items, 10, 0.0, Some(90..91), 2.0);
        e.frame(&mut items, 200.0, 100.0);
        let far = e.placed().iter().find(|p| p.ix == 90).expect("item 90");
        assert!((far.y - 2.0).abs() < 1e-3, "{far:?}");
    }

    #[test]
    fn the_thumb_tracks_the_position_and_drags() {
        let mut items = Fake::new(vec![100; 1_000]);
        let mut e = engine(&mut items, 200.0, 100.0);
        let t0 = e.thumb(100.0, 24.0).expect("thumb");
        assert_eq!(t0.top, 0.0);
        assert_eq!(t0.height, 24.0);
        e.begin_drag();
        let total = e.total();
        e.drag_thumb_to(38.0, 100.0, t0.height);
        e.frame(&mut items, 200.0, 100.0);
        // Measuring changed heights, but the frozen total did not move.
        assert_eq!(e.total(), total);
        let t = e.thumb(100.0, 24.0).expect("thumb");
        assert!((t.top - 38.0).abs() < 0.5, "{t:?}");
        e.drag_thumb_to(1e4, 100.0, t0.height);
        e.frame(&mut items, 200.0, 100.0);
        assert_eq!(e.placed().last().map(|p| p.ix), Some(999));
        e.end_drag();
        let t = e.thumb(100.0, 24.0).expect("thumb");
        assert!((t.top + t.height - 100.0).abs() < 0.5, "{t:?}");
    }
}
