// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Tables too wide for the text column, in headless windows: a table whose
//! widest words alone do not fit grows into the space beside the column (to
//! its natural width, at most the wide block setting's width), stays
//! centred on the column and inside the view's padding, scrolls sideways
//! when even that is too narrow, follows the view's width while the column
//! stays at its maximum, and its cells are hit, selected and followed
//! where they are drawn. Diagrams are in `media_tests`.
//!
//! A table or unwrapped code block that scrolls sideways shows a thumb at
//! the bottom of its visible part, which drags and jumps, and fades over
//! the sides whose text is out of sight.

use gpui::{ScrollDelta, ScrollEvent};
use gv_render_tree::ItemKind;

use super::*;
use crate::metrics::size::SCROLLBAR_MIN_THUMB;
use crate::reader::state::{HBar, Hover};
use crate::settings::Target;

/// A widest word of the second column: longer than a third of the column.
const KEY: &str = "markup.goldmark.parser.autoHeadingIDType.legacy";

/// A table whose widest words (the code spans) are wider than the text
/// column together, but whose cells unwrapped fit a wide view.
fn wide_table() -> String {
    format!(
        "# Slugs\n\n\
         | Option | Key | Kind | Default | Notes |\n\
         |---|---|---|---|---|\n\
         | Typographer_extensions | `{KEY}` | `string` | \
         `github_ascii_only_with_dashes` | \
         the slug algorithm for heading anchors and links to them, see [the slug notes](slugs.md) |\n\
         | Attributes | `markup.goldmark.parser.attribute.block` | `bool` | `false` | \
         block attributes |\n\
         | Unsafe | `markup.goldmark.renderer.unsafe` | `bool` | `false` | raw HTML |\n\
         \nafter the table\n"
    )
}

/// A table whose long cells wrap to fit the column: its words are short.
fn wrapping_table() -> String {
    let long = "words that go on and on ".repeat(10);
    format!("| Construct | Rendering |\n|---|---|\n| Short | {long}end |\n| Two | x |\n\nafter\n")
}

struct Wide {
    _dir: tempfile::TempDir,
    vcx: VisualTestContext,
    reader: Entity<Reader>,
    st: Rc<RefCell<State>>,
}

/// `md` in a window `width` wide, read with the GFM profile.
fn open_wide(cx: &mut TestAppContext, md: &str, width: f32) -> Wide {
    let dir = temp(&[("t.md", md)]);
    let file = dir.path().join("t.md");
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    set_profile(&mut vcx, &file, gv_profile::ProfileId::GfmFile);
    let st = state(&reader, &mut vcx);
    let mut w = Wide {
        _dir: dir,
        vcx,
        reader,
        st,
    };
    w.resize(width);
    w
}

impl Wide {
    fn resize(&mut self, width: f32) {
        self.vcx.simulate_resize(size(px(width), px(HEIGHT)));
        self.settle();
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            draw(&mut self.vcx);
        }
    }

    fn set(&mut self, key: &str, value: toml::Value) {
        self.vcx.update(|_, cx| {
            AppSettings::set_value(cx, Target::AllVaults, key, &value).expect("a valid setting");
        });
        self.settle();
    }

    /// The table as the last frame drew it.
    fn table(&self) -> TableShown {
        TableShown::of(&self.st.borrow())
    }
}

/// What the last frame drew of the document's first table.
#[derive(Debug)]
struct TableShown {
    /// Window x of the text column and its width.
    column: (f32, f32),
    /// Window x of the area's edges.
    area: (f32, f32),
    /// Window x of the table's frame and its width.
    frame: (f32, f32),
    /// How far its text scrolls sideways.
    overflow: f32,
    /// Its width with every cell unwrapped, and with every column at its
    /// widest word (borders and padding included).
    natural: f32,
    min: f32,
    /// The rows drawn: item index, height, lines of text (one per cell
    /// when nothing wraps).
    rows: Vec<(usize, f32, usize)>,
    /// Its columns.
    cells: usize,
}

impl TableShown {
    fn of(s: &State) -> Self {
        let doc = s.doc.as_ref().expect("loaded");
        let m = doc.tree.options().metrics;
        let rows: Vec<_> = s
            .geometry
            .items
            .iter()
            .filter(|g| matches!(doc.tree.items()[g.ix].kind, ItemKind::TableRow { .. }))
            .collect();
        let first = rows.first().expect("table rows are drawn");
        let block = doc.tree.items()[first.ix].id.block;
        let widths = s.cache.table(block).expect("measured");
        let n = widths.columns.len() as f32;
        let extra = 2.0 * m.table_padding_x * n + 2.0 * m.table_border;
        let clip = first.code.expect("a table row's clip").clip;
        let bw = m.table_border;
        TableShown {
            column: s.geometry.column,
            area: s.geometry.area,
            frame: (
                f32::from(clip.left()) - bw,
                f32::from(clip.size.width) + 2.0 * bw,
            ),
            overflow: first.code.expect("clip").overflow,
            cells: widths.columns.len(),
            natural: widths.columns.iter().sum::<f32>() + extra,
            min: widths.min.iter().sum::<f32>() + extra,
            rows: rows
                .iter()
                .map(|g| {
                    let lines = g
                        .text
                        .as_ref()
                        .and_then(|b| b.borrow().layout().map(|l| l.lines().len()))
                        .unwrap_or(0);
                    (g.ix, g.bottom - g.top, lines)
                })
                .collect(),
        }
    }

    /// Whether no cell wraps.
    fn unwrapped(&self) -> bool {
        self.rows.iter().all(|r| r.2 <= self.cells)
    }

    fn centre_offset(&self) -> f32 {
        let (x0, column) = self.column;
        (self.frame.0 + self.frame.1 / 2.0) - (x0 + column / 2.0)
    }

    fn inside_area(&self) -> bool {
        self.frame.0 >= self.area.0 - 0.5 && self.frame.0 + self.frame.1 <= self.area.1 + 0.5
    }
}

/// The window position of source offset `offset`'s caret in the table row
/// that holds it (its top-left, nudged into the line).
fn row_caret(s: &State, offset: usize) -> Point<Pixels> {
    let doc = s.doc.as_ref().expect("loaded");
    for g in &s.geometry.items {
        let item = &doc.tree.items()[g.ix];
        if !(item.source.start <= offset && offset <= item.source.end) {
            continue;
        }
        let node = doc.tree.node_of(g.ix).expect("node");
        let block = g.text.as_ref().expect("text").borrow();
        let caret = block
            .caret_bounds(offset - node.source.start, Bias::Left)
            .expect("painted");
        return caret.origin + point(px(0.5), px(2.0));
    }
    panic!("offset {offset} is not on screen");
}

/// The middle of the first link's text in the table.
fn cell_link_point(s: &State) -> Point<Pixels> {
    let doc = s.doc.as_ref().expect("loaded");
    for g in &s.geometry.items {
        let Some(content) = doc.tree.item_inline(g.ix) else {
            continue;
        };
        let Some(piece) = content
            .spans
            .iter()
            .position(|s| s.link.is_some() && s.role == gv_render_tree::InlineRole::Text)
        else {
            continue;
        };
        let block = g.text.as_ref().expect("text").borrow();
        let layout = block.layout().expect("laid out");
        let origin = block.bounds().expect("painted").origin;
        let r = layout.item_rects(piece)[0];
        return origin + point(px(r.x + r.width / 2.0), px(r.y + r.height / 2.0));
    }
    panic!("no link on screen");
}

#[gpui::test]
fn a_table_whose_words_do_not_fit_grows_beside_the_column(cx: &mut TestAppContext) {
    let w = open_wide(cx, &wide_table(), 1700.0);
    let t = w.table();
    let (_, column) = t.column;
    assert_eq!(column, document::COLUMN_MAX, "{t:?}");
    assert!(t.min > column, "the widest words alone do not fit: {t:?}");
    assert!(
        t.natural < t.area.1 - t.area.0,
        "the natural width fits the view: {t:?}"
    );
    // At its natural width: every cell on one line, nothing to scroll,
    // centred on the column.
    assert!((t.frame.1 - t.natural).abs() < 0.5, "{t:?}");
    assert!(t.overflow < 0.5, "{t:?}");
    assert!(t.unwrapped(), "{t:?}");
    assert!(t.centre_offset().abs() <= 1.0, "{t:?}");
    assert!(t.inside_area(), "{t:?}");
    // The height index has the rows' heights.
    let s = w.st.borrow();
    for (ix, h, _) in &t.rows {
        assert_eq!(s.engine.height_of(*ix), Some(*h), "row {ix}");
    }
}

#[gpui::test]
fn a_table_that_fits_by_wrapping_stays_in_the_column(cx: &mut TestAppContext) {
    let mut w = open_wide(cx, &wrapping_table(), 1700.0);
    let t = w.table();
    let (x0, column) = t.column;
    assert!(t.natural > column, "it would be wider unwrapped: {t:?}");
    assert!(t.min <= column, "{t:?}");
    assert!(t.frame.0 >= x0 - 0.5, "{t:?}");
    assert!(t.frame.0 + t.frame.1 <= x0 + column + 0.5, "{t:?}");
    assert!(!t.unwrapped(), "its long cell wraps: {t:?}");
    // Nothing depends on the view's width: a wider view changes nothing.
    w.resize(2200.0);
    let wider = w.table();
    assert_eq!(wider.frame.1, t.frame.1);
    assert_eq!(wider.frame.0 - wider.column.0, t.frame.0 - t.column.0);
}

#[gpui::test]
fn with_the_setting_off_a_grown_table_stops_at_its_width(cx: &mut TestAppContext) {
    let mut w = open_wide(cx, &wide_table(), 1700.0);
    let natural = w.table().natural;
    w.set("appearance.fill_wide_blocks", toml::Value::Boolean(false));
    w.set(
        "appearance.wide_block_width_chars",
        toml::Value::Integer(110),
    );
    let limit = (110.0 * document::COLUMN_MAX / 72.0).round();
    let t = w.table();
    assert!(t.min < limit && limit < natural, "{t:?} at {limit}");
    // At the set width, its long cells wrapping; nothing to scroll.
    assert!((t.frame.1 - limit).abs() < 0.5, "{t:?} at {limit}");
    assert!(t.overflow < 0.5, "{t:?}");
    assert!(!t.unwrapped(), "{t:?}");
    assert!(t.centre_offset().abs() <= 1.0, "{t:?}");
    // Narrower than the column: the column's width, scrolling sideways.
    w.set(
        "appearance.wide_block_width_chars",
        toml::Value::Integer(40),
    );
    let t = w.table();
    assert!((t.frame.1 - t.column.1).abs() < 0.5, "{t:?}");
    assert!(t.overflow > 0.0, "{t:?}");
    // On again: all the space it needs.
    w.set("appearance.fill_wide_blocks", toml::Value::Boolean(true));
    let t = w.table();
    assert!((t.frame.1 - natural).abs() < 0.5, "{t:?}");
}

#[gpui::test]
fn in_a_narrow_view_a_grown_table_fills_it_and_scrolls(cx: &mut TestAppContext) {
    let mut w = open_wide(cx, &wide_table(), 900.0);
    let t = w.table();
    let area = t.area.1 - t.area.0;
    assert!(t.min > area, "too wide even for the view: {t:?}");
    assert!((t.frame.1 - area).abs() < 0.5, "{t:?}");
    assert!(t.inside_area(), "{t:?}");
    assert!(t.overflow > 0.0, "{t:?}");
    // Shift+wheel over it scrolls it sideways.
    let (at, before) = {
        let s = w.st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        let g = s
            .geometry
            .items
            .iter()
            .find(|g| matches!(doc.tree.items()[g.ix].kind, ItemKind::TableRow { .. }))
            .expect("a row");
        let block = g.text.as_ref().expect("text").borrow();
        (
            g.code.expect("clip").clip.center(),
            block.bounds().expect("painted").left(),
        )
    };
    w.vcx.simulate_mouse_move(at, None, Modifiers::none());
    w.vcx.simulate_event(ScrollEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(0.0), px(-60.0))),
        modifiers: Modifiers::shift(),
        ..Default::default()
    });
    w.settle();
    let s = w.st.borrow();
    let g = s
        .geometry
        .items
        .iter()
        .find(|g| g.code.is_some_and(|c| c.clip.contains(&at)))
        .expect("the row under the pointer");
    let after = g
        .text
        .as_ref()
        .expect("text")
        .borrow()
        .bounds()
        .expect("painted")
        .left();
    assert!(after < before, "{before:?} -> {after:?}");
}

#[gpui::test]
fn widening_the_view_fits_a_grown_table_again(cx: &mut TestAppContext) {
    let mut w = open_wide(cx, &wide_table(), 1100.0);
    let narrow = w.table();
    let rewraps = w.st.borrow().engine.rewraps;
    assert_eq!(narrow.column.1, document::COLUMN_MAX, "{narrow:?}");
    let area = narrow.area.1 - narrow.area.0;
    assert!(
        narrow.min < area && area < narrow.natural,
        "it grows to the view, its notes wrapping: {narrow:?}"
    );
    assert!((narrow.frame.1 - area).abs() < 0.5, "{narrow:?}");
    assert!(!narrow.unwrapped(), "{narrow:?}");
    // Wider: the column stays at its maximum, the table grows to its
    // natural width and its rows lose their wrapped lines.
    w.resize(1700.0);
    let wide = w.table();
    assert_eq!(wide.column.1, document::COLUMN_MAX);
    assert_eq!(
        w.st.borrow().engine.rewraps,
        rewraps,
        "the column did not change"
    );
    assert!((wide.frame.1 - wide.natural).abs() < 0.5, "{wide:?}");
    assert!(wide.unwrapped(), "{wide:?}");
    let shorter: f32 = wide.rows.iter().map(|r| r.1).sum();
    let taller: f32 = narrow.rows.iter().map(|r| r.1).sum();
    assert!(shorter < taller, "{shorter} vs {taller}");
    let s = w.st.borrow();
    for (ix, h, _) in &wide.rows {
        assert_eq!(s.engine.height_of(*ix), Some(*h), "row {ix}");
    }
    drop(s);
    // And back.
    w.resize(1100.0);
    let again = w.table();
    assert!((again.frame.1 - narrow.frame.1).abs() < 0.5, "{again:?}");
    let back: f32 = again.rows.iter().map(|r| r.1).sum();
    assert!((back - taller).abs() < 0.5, "{back} vs {taller}");
}

#[gpui::test]
fn a_grown_rows_cells_are_hit_selected_and_followed_where_drawn(cx: &mut TestAppContext) {
    let md = wide_table();
    let mut w = open_wide(cx, &md, 1700.0);
    let (x0, column) = w.table().column;
    // A word of the last column right of the column's edge.
    let (word, start, from, to, link) = {
        let s = w.st.borrow();
        let (word, start, from) = ["slug", "algorithm", "heading", "anchors", "links", "them"]
            .into_iter()
            .map(|word| {
                let start = md.find(&format!(" {word}")).expect("the word") + 1;
                (word, start, row_caret(&s, start))
            })
            .find(|(_, _, at)| f32::from(at.x) > x0 + column)
            .expect("a word beside the column");
        let to = row_caret(&s, start + word.len());
        (word, start, from, to, cell_link_point(&s))
    };
    {
        let s = w.st.borrow();
        assert_eq!(s.offset_at(from), Some(start));
        assert_eq!(s.hover_at(from), Hover::Text);
        assert!(f32::from(link.x) > x0 + column, "{link:?}");
        let hit = s.link_at(link).expect("the link under the pointer");
        assert_eq!(hit.destination.as_ref(), "slugs.md");
    }
    w.vcx
        .simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    w.vcx
        .simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    w.vcx
        .simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    draw(&mut w.vcx);
    let selected = w.reader.read_with(&w.vcx, |r, _| r.selected_text());
    assert_eq!(selected.as_deref(), Some(word));
    w.vcx.dispatch_action(CopyAction);
    assert_eq!(clipboard(&mut w.vcx).as_deref(), Some(word));
}

#[gpui::test]
fn find_reveals_a_match_in_a_scrolled_table(cx: &mut TestAppContext) {
    let md = wide_table();
    let mut w = open_wide(cx, &md, 900.0);
    let offset = md.find("slug notes").expect("needle");
    {
        // The end of the last column is out of sight.
        let s = w.st.borrow();
        let t = TableShown::of(&s);
        assert!(t.overflow > 0.0, "{t:?}");
        let at = row_caret(&s, offset);
        assert!(f32::from(at.x) > t.frame.0 + t.frame.1, "{at:?} {t:?}");
    }
    // Find scrolls it in.
    w.vcx.simulate_keystrokes("secondary-f");
    draw(&mut w.vcx);
    w.vcx.simulate_input("slug notes");
    w.settle();
    let s = w.st.borrow();
    let t = TableShown::of(&s);
    let at = row_caret(&s, offset);
    let end = row_caret(&s, offset + "slug notes".len());
    assert!(
        f32::from(at.x) >= t.frame.0 && f32::from(end.x) <= t.frame.0 + t.frame.1,
        "{at:?}..{end:?} in {t:?}"
    );
}

// ----- sideways scrollbars and fades ------------------------------------------

/// A table like [`wide_table`] with `rows` body rows: taller than the view.
fn tall_table(rows: usize) -> String {
    let mut md = String::from(
        "# Tall\n\n| Option | Key | Kind | Default | Notes |\n|---|---|---|---|---|\n",
    );
    for i in 0..rows {
        md.push_str(&format!(
            "| Typographer_extensions | `{KEY}` | `string` | `github_ascii_only_with_dashes` \
             | row {i} of the slug notes |\n"
        ));
    }
    md.push_str("\nafter the table\n");
    md
}

/// A drawn item's clip and its fades (left, right).
type Clipped = (Bounds<Pixels>, (bool, bool));

/// The sideways bar of the first table or code block, and the clips and
/// fades of its drawn items.
fn bar_of(s: &State) -> Option<(HBar, Vec<Clipped>)> {
    let bar = *s.geometry.hbars.first()?;
    let items = s
        .geometry
        .items
        .iter()
        .filter_map(|g| g.code)
        .filter(|c| c.block == bar.block)
        .map(|c| (c.clip, c.fades))
        .collect();
    Some((bar, items))
}

/// The drawn items' fades of the first block that scrolls sideways.
fn fades(s: &State) -> Vec<(bool, bool)> {
    s.geometry
        .items
        .iter()
        .filter_map(|g| g.code)
        .filter(|c| c.overflow > 0.5)
        .map(|c| c.fades)
        .collect()
}

#[gpui::test]
fn a_table_that_scrolls_sideways_shows_a_thumb_at_its_bottom(cx: &mut TestAppContext) {
    let mut w = open_wide(cx, &wide_table(), 900.0);
    let t = w.table();
    assert!(t.overflow > 0.0, "{t:?}");
    {
        let s = w.st.borrow();
        assert_eq!(s.geometry.hbars.len(), 1, "{:?}", s.geometry.hbars);
        let (bar, clips) = bar_of(&s).expect("a sideways bar");
        let first = clips.first().expect("rows").0;
        let last = clips.last().expect("rows").0;
        // Along the table's inside, 4 px in from each side; 6 thick, 3 px
        // above its bottom edge.
        assert_eq!(bar.track.0, f32::from(first.left()) + scrollbar::INSET_Y);
        assert_eq!(
            bar.track.1,
            f32::from(first.size.width) - 2.0 * scrollbar::INSET_Y
        );
        assert_eq!(f32::from(bar.thumb.size.height), SCROLLBAR_THUMB);
        assert_eq!(
            f32::from(bar.thumb.bottom()),
            f32::from(last.bottom()) - scrollbar::INSET_X
        );
        // As long as the part that shows is of the whole, at the start.
        let view = f32::from(first.size.width);
        let want = (bar.track.1 * view / (view + t.overflow)).max(SCROLLBAR_MIN_THUMB);
        assert!(
            (f32::from(bar.thumb.size.width) - want).abs() < 0.5,
            "{bar:?} {want}"
        );
        assert_eq!(f32::from(bar.thumb.left()), bar.track.0);
        assert!((bar.overflow - t.overflow).abs() < 0.01);
        // The pointer over it is not over text.
        assert_eq!(s.hover_at(bar.thumb.center()), Hover::Scrollbar);
    }
    // A table that fits has none.
    w.resize(1700.0);
    assert!(w.st.borrow().geometry.hbars.is_empty());
}

#[gpui::test]
fn the_sideways_thumb_drags_and_a_press_beside_it_jumps(cx: &mut TestAppContext) {
    let mut w = open_wide(cx, &wide_table(), 900.0);
    let (bar, _) = bar_of(&w.st.borrow()).expect("a sideways bar");
    let room = bar.track.1 - f32::from(bar.thumb.size.width);
    // Dragged 40 px right: scrolled as far along.
    let from = bar.thumb.center();
    let to = from + point(px(40.0), px(0.0));
    w.vcx
        .simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    w.vcx
        .simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    w.vcx
        .simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    w.settle();
    {
        let s = w.st.borrow();
        let scrolled = s.hscroll.get(&bar.block).copied().unwrap_or(0.0);
        assert!(
            (scrolled - 40.0 / room * bar.overflow).abs() < 0.5,
            "{scrolled} of {}",
            bar.overflow
        );
        let (now, _) = bar_of(&s).expect("the bar");
        assert!((f32::from(now.thumb.left() - bar.thumb.left()) - 40.0).abs() < 0.5);
        // Nothing was selected on the way.
        assert!(s.selected().is_none());
    }
    // A press at the track's right end: the thumb jumps there, the table to
    // its end.
    let end = point(px(bar.track.0 + bar.track.1 - 1.0), bar.thumb.center().y);
    w.vcx
        .simulate_mouse_down(end, MouseButton::Left, Modifiers::none());
    w.vcx
        .simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    w.settle();
    let s = w.st.borrow();
    let scrolled = s.hscroll.get(&bar.block).copied().unwrap_or(0.0);
    assert!((scrolled - bar.overflow).abs() < 0.5, "{scrolled}");
    let (now, _) = bar_of(&s).expect("the bar");
    assert!(
        (f32::from(now.thumb.right()) - (bar.track.0 + bar.track.1)).abs() < 0.5,
        "{now:?}"
    );
}

#[gpui::test]
fn the_sideways_thumb_stays_on_screen_on_a_tall_table(cx: &mut TestAppContext) {
    let mut w = open_wide(cx, &tall_table(60), 900.0);
    // The table fills the view: the thumb is at the view's bottom.
    {
        let mut s = w.st.borrow_mut();
        let doc = s.doc.as_ref().expect("loaded");
        let row = doc
            .tree
            .items()
            .iter()
            .position(|it| matches!(it.kind, ItemKind::TableRow { row: 20, .. }))
            .expect("row 20");
        s.engine.jump_to(row, 10.0);
    }
    w.settle();
    {
        let s = w.st.borrow();
        let (bar, clips) = bar_of(&s).expect("a sideways bar");
        let view = s.geometry.bounds;
        assert!(
            clips.first().expect("rows").0.top() <= view.top()
                && clips.last().expect("rows").0.bottom() >= view.bottom(),
            "the table covers the view"
        );
        assert_eq!(
            f32::from(bar.thumb.bottom()),
            f32::from(view.bottom()) - scrollbar::INSET_X
        );
    }
    // Its end in sight: the thumb is at the table's bottom edge.
    w.st.borrow_mut().engine.scroll_to_end();
    w.settle();
    let s = w.st.borrow();
    let (bar, clips) = bar_of(&s).expect("a sideways bar");
    let last = clips.last().expect("rows").0;
    assert!(last.bottom() < s.geometry.bounds.bottom(), "{last:?}");
    assert_eq!(
        f32::from(bar.thumb.bottom()),
        f32::from(last.bottom()) - scrollbar::INSET_X
    );
}

#[gpui::test]
fn fades_show_on_the_sides_whose_text_is_out_of_sight(cx: &mut TestAppContext) {
    let mut w = open_wide(cx, &wide_table(), 900.0);
    let (bar, _) = bar_of(&w.st.borrow()).expect("a sideways bar");
    let rows = fades(&w.st.borrow());
    assert!(!rows.is_empty());
    assert!(
        rows.iter().all(|f| *f == (false, true)),
        "at the start: {rows:?}"
    );
    for (scroll, want) in [
        (bar.overflow / 2.0, (true, true)),
        (bar.overflow, (true, false)),
        (0.0, (false, true)),
    ] {
        w.st.borrow_mut().hscroll.insert(bar.block, scroll);
        w.settle();
        let rows = fades(&w.st.borrow());
        assert!(rows.iter().all(|f| *f == want), "at {scroll}: {rows:?}");
    }
    // A table that fits fades nowhere.
    w.resize(1700.0);
    let s = w.st.borrow();
    assert!(
        s.geometry
            .items
            .iter()
            .filter_map(|g| g.code)
            .all(|c| c.fades == (false, false))
    );
}

#[gpui::test]
fn an_unwrapped_code_block_shows_the_thumb_and_fades(cx: &mut TestAppContext) {
    let long = "x".repeat(300);
    let md = format!("```sh\necho {long}\nls\n```\n\nafter\n");
    let mut w = open_wide(cx, &md, 1280.0);
    let (bar, clips) = bar_of(&w.st.borrow()).expect("a sideways bar");
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].1, (false, true));
    assert_eq!(
        f32::from(bar.thumb.bottom()),
        f32::from(clips[0].0.bottom()) - scrollbar::INSET_X
    );
    // Dragged to the end: the left side fades instead.
    let from = bar.thumb.center();
    let to = from + point(px(bar.track.1), px(0.0));
    w.vcx
        .simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    w.vcx
        .simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    w.vcx
        .simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    w.settle();
    {
        let s = w.st.borrow();
        let scrolled = s.hscroll.get(&bar.block).copied().unwrap_or(0.0);
        assert!((scrolled - bar.overflow).abs() < 0.5, "{scrolled}");
        assert_eq!(fades(&s), vec![(true, false)]);
    }
    // Wrapped: nothing scrolls, so no thumb and no fades.
    let wrap = {
        let s = w.st.borrow();
        s.geometry
            .buttons
            .iter()
            .find(|b| b.kind == ButtonKind::Wrap)
            .expect("the wrap button")
            .bounds
            .center()
    };
    w.vcx.simulate_click(wrap, Modifiers::none());
    w.settle();
    let s = w.st.borrow();
    assert!(s.geometry.hbars.is_empty());
    assert!(fades(&s).is_empty());
}
