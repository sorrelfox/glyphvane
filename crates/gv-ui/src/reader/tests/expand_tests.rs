// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The expanded table, in headless windows: the Expand button shows while
//! the pointer is over a table that is not at its natural width, opens the
//! table on its own over the content area, fitted to it, and Esc or the
//! close button closes it with the document where it was. Links in it
//! resolve against the document.

use gv_render_tree::ItemKind;

use super::*;
use crate::metrics::size::CODE_BUTTON;
use crate::metrics::spacing::code_head;
use crate::reader::ExpandedTable;
use crate::reader::link_info::TOOLTIP_DELAY;
use crate::reader::link_tip::EXPAND_TIP;
use crate::reader::state::CodeButton;

/// A widest word: the table's words alone do not fit the column.
const KEY: &str = "markup.goldmark.parser.autoHeadingIDType.legacy";

/// Text above the tables, so the document can scroll.
fn intro() -> String {
    (0..12)
        .map(|i| {
            format!(
                "Paragraph {i} has enough words to wrap over a line or two in the text column.\n\n"
            )
        })
        .collect()
}

/// A small table at its natural width, then a wide one (with a link to
/// `other.md`) whose cells wrap even beside the column.
fn doc() -> String {
    let notes = "the slug algorithm for heading anchors and all the links to them, \
                 kept for older documents that were written before the change";
    format!(
        "# Doc\n\n{}## Slug algorithms\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n\
         | Option | Key | Kind | Default | Notes |\n|---|---|---|---|---|\n\
         | Typographer_extensions | `{KEY}` | `string` | `github_ascii_only_with_dashes` | \
         {notes}, see [the other note](other.md) |\n\
         | Unsafe | `markup.goldmark.renderer.unsafe` | `bool` | `false` | raw HTML |\n\
         \nafter the tables\n",
        intro()
    )
}

struct Fixture {
    dir: tempfile::TempDir,
    vcx: VisualTestContext,
    workspace: Entity<Workspace>,
    reader: Entity<Reader>,
    st: Rc<RefCell<State>>,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    let dir = temp(&[
        ("doc.md", &doc()),
        ("other.md", "# Other\n\nThe other note.\n"),
    ]);
    let file = dir.path().join("doc.md");
    let (window, workspace) = open(cx, &file);
    let mut vcx = drawn(window, cx);
    let reader = reader(&workspace, &mut vcx);
    set_profile(&mut vcx, &file, gv_profile::ProfileId::GfmFile);
    let st = state(&reader, &mut vcx);
    // The tables in view.
    {
        let mut s = st.borrow_mut();
        let doc = s.doc.as_ref().expect("loaded");
        let heading = doc
            .tree
            .items()
            .iter()
            .position(|it| matches!(it.kind, ItemKind::Heading { level: 2 }))
            .expect("the heading");
        s.engine.jump_to(heading, 0.0);
    }
    let mut f = Fixture {
        dir,
        vcx,
        workspace,
        reader,
        st,
    };
    f.settle();
    f
}

impl Fixture {
    fn settle(&mut self) {
        for _ in 0..3 {
            draw(&mut self.vcx);
        }
    }

    /// The middle of a cell of the `n`th table (0-based) as drawn.
    fn over_table(&self, n: usize) -> Point<Pixels> {
        let s = self.st.borrow();
        let doc = s.doc.as_ref().expect("loaded");
        let mut blocks = Vec::new();
        for g in &s.geometry.items {
            let item = &doc.tree.items()[g.ix];
            if matches!(item.kind, ItemKind::TableRow { .. }) && !blocks.contains(&item.id.block) {
                blocks.push(item.id.block);
            }
        }
        let block = blocks[n];
        let clip = s
            .geometry
            .items
            .iter()
            .filter_map(|g| g.code)
            .find(|c| c.block == block)
            .expect("a drawn row")
            .clip;
        clip.center()
    }

    fn hover(&mut self, at: Point<Pixels>) {
        self.vcx.simulate_mouse_move(at, None, Modifiers::none());
        self.settle();
    }

    fn expand_button(&self) -> Option<CodeButton> {
        self.st
            .borrow()
            .geometry
            .buttons
            .iter()
            .find(|b| b.kind == ButtonKind::Expand)
            .copied()
    }

    fn expanded(&mut self) -> Option<Entity<ExpandedTable>> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.expanded_table().cloned())
    }

    /// Opens the wide table's expanded view with its button.
    fn open_expanded(&mut self) -> Entity<ExpandedTable> {
        let at = self.over_table(1);
        self.hover(at);
        let button = self.expand_button().expect("the Expand button");
        self.vcx
            .simulate_click(button.bounds.center(), Modifiers::none());
        self.settle();
        self.expanded().expect("the expanded table")
    }

    fn drawn(&mut self, selector: &'static str) -> Option<Bounds<Pixels>> {
        self.vcx.debug_bounds(selector)
    }
}

#[gpui::test]
fn the_expand_button_shows_over_tables_not_at_their_natural_width(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    // Not over a table: no button.
    assert!(f.expand_button().is_none());
    // Over the small table, at its natural width: none either.
    let small = f.over_table(0);
    f.hover(small);
    assert!(f.expand_button().is_none());
    // Over the wide one, whose cells wrap: at the top right of its
    // visible part.
    let wide = f.over_table(1);
    f.hover(wide);
    let button = f.expand_button().expect("the Expand button");
    {
        let s = f.st.borrow();
        let clip = s
            .geometry
            .items
            .iter()
            .filter_map(|g| g.code)
            .find(|c| c.block == button.block)
            .expect("the table's rows")
            .clip;
        let widths = s.cache.table(button.block).expect("measured");
        assert!(!widths.is_natural(), "its cells wrap");
        assert_eq!(f32::from(button.bounds.size.width), CODE_BUTTON);
        assert_eq!(f32::from(button.bounds.size.height), CODE_BUTTON);
        assert_eq!(
            f32::from(button.bounds.right()),
            f32::from(clip.right()) - code_head::PAD_RIGHT
        );
        assert_eq!(
            f32::from(button.bounds.top()),
            f32::from(clip.top()) + code_head::PAD_RIGHT
        );
    }
    // Resting on it: the tooltip.
    f.hover(button.bounds.center());
    f.vcx.executor().advance_clock(TOOLTIP_DELAY);
    f.settle();
    assert!(f.drawn("link-tooltip").is_some());
    assert_eq!(EXPAND_TIP, "Expand table");
    // Off the tables again: gone.
    let away = point(px(5.0), wide.y);
    f.hover(away);
    assert!(f.expand_button().is_none());
}

#[gpui::test]
fn the_expanded_table_fills_the_content_area(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let expanded = f.open_expanded();
    let area = f.drawn("expanded-table").expect("drawn");
    // Below the title row, above the status bar.
    let window = f.vcx.update(|window, _| window.bounds().size);
    assert_eq!(f32::from(area.top()), crate::metrics::size::TITLE_ROW);
    assert_eq!(
        f32::from(area.bottom()),
        f32::from(window.height) - crate::metrics::size::STATUS_BAR
    );
    assert_eq!(f32::from(area.size.width), f32::from(window.width));
    // The table on its own, read as the document (links resolve against
    // it), with the keyboard.
    let table = expanded.read_with(&f.vcx, |e, _| e.reader().clone());
    let path = f
        .reader
        .read_with(&f.vcx, |r, _| r.path().map(Path::to_path_buf));
    assert_eq!(
        table.read_with(&f.vcx, |r, _| r.path().map(Path::to_path_buf)),
        path
    );
    let focused = f
        .vcx
        .update(|window, cx| gpui::Focusable::focus_handle(table.read(cx), cx).is_focused(window));
    assert!(focused);
    let st = state(&table, &mut f.vcx);
    let s = st.borrow();
    let doc = s.doc.as_ref().expect("loaded");
    assert!(doc.text.starts_with("| Option | Key |"), "{}", doc.text);
    assert!(!doc.text.contains("| A | B |"));
    // Fitted to the view, whatever the readable line width: the column is
    // the view, and the table fills it with its cells wrapping.
    let bounds = s.geometry.bounds;
    let (_, column) = s.geometry.column;
    assert_eq!(
        column,
        (f32::from(bounds.size.width) - 2.0 * document::PAD_X).floor()
    );
    let rows: Vec<_> = s.geometry.items.iter().filter_map(|g| g.code).collect();
    assert!(!rows.is_empty());
    let block = rows[0].block;
    let widths = s.cache.table(block).expect("measured");
    let m = doc.tree.options().metrics;
    let width = widths.width(m.table_padding_x) + 2.0 * m.table_border;
    assert!(width <= column + 0.5, "{width} in {column}");
    assert!(rows.iter().all(|c| c.overflow < 0.5));
    // Its scrollbars and buttons: no Expand button inside.
    assert!(
        s.geometry
            .buttons
            .iter()
            .all(|b| b.kind != ButtonKind::Expand)
    );
    drop(s);
    let link = table
        .read_with(&f.vcx, |r, _| r.link_statuses_for_tests())
        .into_iter()
        .find(|(d, _)| d == "other.md")
        .expect("the link");
    assert_ne!(link.1, gv_render_tree::LinkStatus::Unresolved);
}

#[gpui::test]
fn esc_closes_the_expanded_table_and_the_document_stays(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let before = f.st.borrow().engine.scroll_top();
    f.open_expanded();
    // Esc in the table closes it.
    f.vcx.simulate_keystrokes("escape");
    f.settle();
    assert!(f.expanded().is_none());
    assert!(f.drawn("expanded-table").is_none());
    assert_eq!(f.st.borrow().engine.scroll_top(), before);
    let reader = f.reader.clone();
    let focused = f
        .vcx
        .update(|window, cx| gpui::Focusable::focus_handle(reader.read(cx), cx).is_focused(window));
    assert!(focused, "the document has the keyboard again");
    // So does the close button.
    f.open_expanded();
    let close = f.drawn("expanded-table-close").expect("the close button");
    f.vcx.simulate_click(close.center(), Modifiers::none());
    f.settle();
    assert!(f.expanded().is_none());
    assert_eq!(f.st.borrow().engine.scroll_top(), before);
}

#[gpui::test]
fn a_link_in_the_expanded_table_opens_from_the_document(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let expanded = f.open_expanded();
    let table = expanded.read_with(&f.vcx, |e, _| e.reader().clone());
    let at = table
        .read_with(&f.vcx, |r, _| r.link_point_for_tests("the other note"))
        .or_else(|| {
            // A table row's link: the first link in the row that holds it.
            let st = state(&table, &mut f.vcx);
            let s = st.borrow();
            let doc = s.doc.as_ref()?;
            s.geometry.items.iter().find_map(|g| {
                let content = doc.tree.item_inline(g.ix)?;
                let piece = content.spans.iter().position(|sp| {
                    sp.link.is_some() && sp.role == gv_render_tree::InlineRole::Text
                })?;
                let block = g.text.as_ref()?.borrow();
                let r = block.layout()?.item_rects(piece)[0];
                Some(
                    block.bounds()?.origin
                        + point(px(r.x + r.width / 2.0), px(r.y + r.height / 2.0)),
                )
            })
        })
        .expect("the link is drawn");
    f.vcx.simulate_click(at, Modifiers::none());
    f.settle();
    assert!(f.expanded().is_none());
    assert_eq!(file_of(&f.workspace, &mut f.vcx), "other.md");
    let _ = &f.dir;
}
