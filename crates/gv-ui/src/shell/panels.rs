// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The side panels.
//!
//! - **Sidebar:** the switcher in its head picks Files, Search, Tags or
//!   Bookmarks (SPEC § Sidebar); Recent and Sync conflicts open from the
//!   View menu (`workspace.show_*`) and have no switcher button; File ▸
//!   Open recent… shows Recent too, and File ▸ File history shows the
//!   File history panel ([`crate::history`]). The Files, Search
//!   ([`crate::search_panel`]), Tags ([`crate::tags_panel`]), Bookmarks
//!   ([`super::bookmarks`]), Recent, Sync conflicts
//!   ([`crate::conflicts`]) and File history panels are built.
//! - **Right panel:** collapsible sections, the Outline first, then
//!   Backlinks and *Links in this note* ([`super::links_panel`]). The Link
//!   checker ([`super::link_checker`]) joins it once asked for.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Corners, Div, ElementId, Entity, FocusHandle, Focusable as _, FontWeight,
    InteractiveElement as _, ParentElement as _, Pixels, Stateful, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px,
};

use super::Workspace;
use super::controls::{RowStart, icon_button};
use super::link_checker::LinkChecker;
use super::links_panel::LinksPanel;
use super::outline_panel::OutlinePanel;
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::keymap::actions::workspace as commands;
use crate::metrics::size::icon_button as button;
use crate::metrics::spacing::{section_head, sidebar_head};
use crate::metrics::{HAIRLINE, icon, size, text};
use crate::theme::Theme;

/// The sidebar's panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarPanel {
    #[default]
    Files,
    Search,
    Tags,
    Bookmarks,
    Recent,
    SyncConflicts,
    FileHistory,
}

impl SidebarPanel {
    /// The panels of the switcher, in order.
    pub const SWITCHER: [Self; 4] = [Self::Files, Self::Search, Self::Tags, Self::Bookmarks];

    /// The name in debug selectors.
    pub fn id(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Search => "search",
            Self::Tags => "tags",
            Self::Bookmarks => "bookmarks",
            Self::Recent => "recent",
            Self::SyncConflicts => "sync-conflicts",
            Self::FileHistory => "file-history",
        }
    }

    /// The name the session stores, for the panels a window brings back:
    /// not the File history and Sync conflicts panels, which follow a
    /// document or a moment.
    pub fn saved_id(self) -> Option<&'static str> {
        match self {
            Self::Files | Self::Search | Self::Tags | Self::Bookmarks | Self::Recent => {
                Some(self.id())
            }
            Self::SyncConflicts | Self::FileHistory => None,
        }
    }

    /// The panel the session stored as `id`.
    pub fn from_saved_id(id: &str) -> Option<Self> {
        [
            Self::Files,
            Self::Search,
            Self::Tags,
            Self::Bookmarks,
            Self::Recent,
        ]
        .into_iter()
        .find(|p| p.id() == id)
    }

    /// The panel's name for assistive technology.
    pub fn title(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Search => "Search",
            Self::Tags => "Tags",
            Self::Bookmarks => "Bookmarks",
            Self::Recent => "Recent",
            Self::SyncConflicts => "Sync conflicts",
            Self::FileHistory => "File history",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Files | Self::Recent | Self::SyncConflicts | Self::FileHistory => Icon::Files,
            Self::Search => Icon::Search,
            Self::Tags => Icon::Tags,
            Self::Bookmarks => Icon::Bookmarks,
        }
    }
}

/// A right panel section that is drawn only once asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RightSection {
    Outline,
    Backlinks,
    OutgoingLinks,
    LinkChecker,
}

/// Which panels show.
#[derive(Default)]
pub(super) struct Panels {
    pub sidebar: SidebarPanel,
    /// The Link checker, once asked for.
    pub link_checker: Option<Entity<LinkChecker>>,
    pub _checker_events: Option<Subscription>,
    /// The Search panel, once shown ([`super::search`]).
    pub search: Option<Entity<crate::search_panel::SearchPanel>>,
    pub _search_events: Option<Subscription>,
    /// The Tags panel, once shown ([`super::tags`]).
    pub tags: Option<Entity<crate::tags_panel::TagsPanel>>,
    pub _tags_events: Option<Subscription>,
    /// The Sync conflicts panel, once shown ([`super::sync_conflicts`]).
    pub conflicts: Option<Entity<crate::conflicts::SyncConflictsPanel>>,
    pub _conflicts_events: Option<Subscription>,
    /// The File history panel, once shown ([`super::file_history`]).
    pub history: Option<Entity<crate::history::HistoryPanel>>,
    pub _history_events: Option<Subscription>,
    /// Versions to compare or restore once a Source view has loaded.
    pub versions_after_load: Vec<(gpui::EntityId, super::file_history::VersionRequest)>,
    /// The Bookmarks panel, once shown ([`super::bookmarks`]).
    pub bookmarks: Option<Entity<super::bookmarks_panel::BookmarksPanel>>,
    pub _bookmarks_events: Option<Subscription>,
}

impl Workspace {
    /// The sidebar: full height above the status bar, with the panel
    /// switcher in a head that lines up with the title row.
    pub(super) fn render_sidebar(
        &self,
        radii: Corners<Pixels>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let theme = Appearance::theme(cx);
        let panel = self.panels.sidebar;
        div()
            .id("sidebar")
            .a11y_group(Role::Complementary, format!("Sidebar: {}", panel.title()))
            .debug_selector(|| "sidebar".into())
            .w(px(size::SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.surface.panel)
            .border_r_1()
            .border_color(theme.border.default)
            .rounded_tl(radii.top_left)
            .child(
                div()
                    .h(px(size::TITLE_ROW))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(sidebar_head::GAP))
                    .px(px(sidebar_head::PAD_X))
                    .children(self.row_start(RowStart::SidebarHead, theme, window, cx))
                    .child(self.drag_region("sidebar-drag", window, cx).flex_1())
                    .child(self.panel_switcher(theme, cx).accessible()),
            )
            .map(|sidebar| match (panel, self.files.clone()) {
                (SidebarPanel::Files, Some(files)) => sidebar.child(files),
                (SidebarPanel::Files, None) => sidebar,
                (SidebarPanel::Recent, _) => match self.recent.clone() {
                    Some(recent) => sidebar.child(recent),
                    None => sidebar,
                },
                (SidebarPanel::Search, _) => match self.panels.search.clone() {
                    Some(search) => sidebar.child(search),
                    None => sidebar,
                },
                (SidebarPanel::Tags, _) => match self.panels.tags.clone() {
                    Some(tags) => sidebar.child(tags),
                    None => sidebar,
                },
                (SidebarPanel::SyncConflicts, _) => match self.panels.conflicts.clone() {
                    Some(conflicts) => sidebar.child(conflicts),
                    None => sidebar,
                },
                (SidebarPanel::FileHistory, _) => match self.panels.history.clone() {
                    Some(history) => sidebar.child(history),
                    None => sidebar,
                },
                (SidebarPanel::Bookmarks, _) => match self.panels.bookmarks.clone() {
                    Some(bookmarks) => sidebar.child(bookmarks),
                    None => sidebar,
                },
            })
    }

    /// Files, Search, Tags and Bookmarks; the active one has an
    /// `accent.tint` fill and `text.primary`.
    fn panel_switcher(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Stateful<Div> {
        let size = (button::WIDTH, button::HEIGHT);
        let count = SidebarPanel::SWITCHER.len();
        div()
            .id("panel-switcher")
            .a11y_group(Role::TabList, "Sidebar panels")
            .flex()
            .flex_none()
            .gap(px(sidebar_head::SWITCHER_GAP))
            .children(
                SidebarPanel::SWITCHER
                    .into_iter()
                    .enumerate()
                    .map(|(index, panel)| {
                        let id = panel.id();
                        let selected = panel == self.panels.sidebar;
                        let button = icon_button(
                            ElementId::Name(format!("panel-{id}").into()),
                            panel.title(),
                            panel.icon(),
                            icon::TOOLBAR,
                            size,
                            theme,
                        )
                        .a11y_tab(panel.title(), selected, index, count)
                        .debug_selector(move || format!("panel-switch:{id}"))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.show_sidebar_panel(panel, false, window, cx);
                            },
                        ));
                        let button = if selected {
                            button.bg(theme.accent.tint).text_color(theme.text.primary)
                        } else {
                            button
                        };
                        button.accessible()
                    }),
            )
    }

    /// The sidebar's panel.
    #[cfg(test)]
    pub(crate) fn sidebar_panel(&self) -> SidebarPanel {
        self.panels.sidebar
    }

    /// Shows `panel` in the sidebar, showing the sidebar if hidden.
    /// `focus` gives the Files panel the keyboard (the View menu's
    /// commands; the switcher's buttons leave it where it is).
    pub(crate) fn show_sidebar_panel(
        &mut self,
        panel: SidebarPanel,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // A panel that leaves the screen with the keyboard hands it to the
        // document.
        let leaving = self.panels.sidebar;
        let focused = |p: Option<FocusHandle>| p.is_some_and(|h| h.contains_focused(window, cx));
        let had_keyboard = leaving != panel
            && match leaving {
                SidebarPanel::Files => {
                    focused(self.files.as_ref().map(|f| f.read(cx).focus_handle(cx)))
                }
                SidebarPanel::Recent => {
                    focused(self.recent.as_ref().map(|r| r.read(cx).focus_handle(cx)))
                }
                SidebarPanel::Search => self
                    .panels
                    .search
                    .as_ref()
                    .is_some_and(|s| s.read(cx).contains_focus(window, cx)),
                SidebarPanel::Tags => focused(
                    self.panels
                        .tags
                        .as_ref()
                        .map(|t| t.read(cx).focus_handle(cx)),
                ),
                SidebarPanel::SyncConflicts => focused(
                    self.panels
                        .conflicts
                        .as_ref()
                        .map(|c| c.read(cx).focus_handle(cx)),
                ),
                SidebarPanel::FileHistory => focused(
                    self.panels
                        .history
                        .as_ref()
                        .map(|h| h.read(cx).focus_handle(cx)),
                ),
                SidebarPanel::Bookmarks => focused(
                    self.panels
                        .bookmarks
                        .as_ref()
                        .map(|b| b.read(cx).focus_handle(cx)),
                ),
            };
        if self.panels.sidebar != panel {
            self.session_changed(cx);
        }
        self.panels.sidebar = panel;
        let takes_keyboard = matches!(
            panel,
            SidebarPanel::Files
                | SidebarPanel::Recent
                | SidebarPanel::Search
                | SidebarPanel::Tags
                | SidebarPanel::SyncConflicts
                | SidebarPanel::FileHistory
                | SidebarPanel::Bookmarks
        );
        if had_keyboard && !(focus && takes_keyboard) {
            self.focus_document(window, cx);
        }
        if !self.sidebar_open {
            self.set_sidebar_open(true, window, cx);
        }
        if panel == SidebarPanel::Recent {
            let recent = self.ensure_recent_panel(window, cx);
            if focus {
                recent.read(cx).focus_handle(cx).focus(window, cx);
            }
        }
        if focus && panel == SidebarPanel::Files {
            self.focus_files(window, cx);
        }
        if panel == SidebarPanel::Tags {
            let tags = self.ensure_tags_panel(window, cx);
            if focus {
                tags.read(cx).focus_handle(cx).focus(window, cx);
            }
        }
        if panel == SidebarPanel::SyncConflicts {
            let conflicts = self.ensure_conflicts_panel(window, cx);
            if focus {
                conflicts.read(cx).focus_handle(cx).focus(window, cx);
            }
        }
        if panel == SidebarPanel::FileHistory {
            let history = self.ensure_history_panel(window, cx);
            if focus {
                history.read(cx).focus_handle(cx).focus(window, cx);
            }
        }
        if panel == SidebarPanel::Bookmarks {
            let bookmarks = self.ensure_bookmarks_panel(window, cx);
            if focus {
                bookmarks.read(cx).focus_handle(cx).focus(window, cx);
            }
        }
        if panel == SidebarPanel::Search {
            let search = self.ensure_search_panel(window, cx);
            if focus {
                search.update(cx, |s, cx| s.focus_field(window, cx));
            }
        }
        cx.notify();
    }

    /// Gives the Files panel the keyboard.
    pub(super) fn focus_files(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let focus: Option<FocusHandle> = self.files.as_ref().map(|f| f.read(cx).focus_handle(cx));
        if let Some(focus) = focus {
            focus.focus(window, cx);
        }
    }

    /// Shows the right panel with `section` in it, expanded.
    pub(super) fn show_right_section(
        &mut self,
        section: RightSection,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.panel_open {
            self.set_panel_open(true, window, cx);
        }
        if section == RightSection::LinkChecker {
            let checker = self.ensure_link_checker(cx);
            checker.update(cx, |c, cx| c.expand(cx));
        }
        if section == RightSection::Outline
            && let Some(outline) = &self.outline
        {
            outline.update(cx, |o, cx| o.expand(cx));
        }
        if let Some(links) = &self.links {
            match section {
                RightSection::Backlinks => links.update(cx, |l, cx| l.expand_backlinks(cx)),
                RightSection::OutgoingLinks => links.update(cx, |l, cx| l.expand_outgoing(cx)),
                RightSection::Outline | RightSection::LinkChecker => {}
            }
        }
        cx.notify();
    }

    /// The `workspace.show_*` commands: each shows its panel.
    pub(super) fn panel_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let sidebar = |panel: SidebarPanel| {
            move |this: &mut Self, window: &mut Window, cx: &mut Context<'_, Self>| {
                this.show_sidebar_panel(panel, true, window, cx);
            }
        };
        let right = |section: RightSection| {
            move |this: &mut Self, window: &mut Window, cx: &mut Context<'_, Self>| {
                this.show_right_section(section, window, cx);
            }
        };
        let files = sidebar(SidebarPanel::Files);
        let tags = sidebar(SidebarPanel::Tags);
        let bookmarks = sidebar(SidebarPanel::Bookmarks);
        let recent = sidebar(SidebarPanel::Recent);
        let conflicts = sidebar(SidebarPanel::SyncConflicts);
        let outline = right(RightSection::Outline);
        let backlinks = right(RightSection::Backlinks);
        let outgoing = right(RightSection::OutgoingLinks);
        let checker = right(RightSection::LinkChecker);
        let root = self.link_checker_actions(root, cx);
        root.on_action(
            cx.listener(move |this, _: &commands::ShowFiles, window, cx| {
                files(this, window, cx);
            }),
        )
        .on_action(
            cx.listener(move |this, _: &commands::ShowTags, window, cx| {
                tags(this, window, cx);
            }),
        )
        .on_action(
            cx.listener(move |this, _: &commands::ShowBookmarks, window, cx| {
                bookmarks(this, window, cx);
            }),
        )
        .on_action(
            cx.listener(move |this, _: &commands::ShowRecent, window, cx| {
                recent(this, window, cx);
            }),
        )
        .on_action(
            cx.listener(move |this, _: &commands::ShowSyncConflicts, window, cx| {
                conflicts(this, window, cx);
            }),
        )
        .on_action(
            cx.listener(move |this, _: &commands::ShowOutline, window, cx| {
                outline(this, window, cx);
            }),
        )
        .on_action(
            cx.listener(move |this, _: &commands::ShowBacklinks, window, cx| {
                backlinks(this, window, cx);
            }),
        )
        .on_action(
            cx.listener(move |this, _: &commands::ShowOutgoingLinks, window, cx| {
                outgoing(this, window, cx);
            }),
        )
        .on_action(
            cx.listener(move |this, _: &commands::ShowLinkChecker, window, cx| {
                checker(this, window, cx);
            }),
        )
    }
}

/// The right panel: collapsible sections stacked from the top. The Outline
/// comes first, then Backlinks and *Links in this note*, then the Link
/// checker when asked for.
pub(super) fn right_panel(
    theme: &Theme,
    outline: Option<Entity<OutlinePanel>>,
    links: Option<Entity<LinksPanel>>,
    panels: &Panels,
) -> Stateful<Div> {
    div()
        .id("right-panel")
        .a11y_group(Role::Complementary, "Right panel")
        .w(px(size::PANEL_WIDTH))
        .flex_none()
        .h_full()
        .min_h_0()
        .flex()
        .flex_col()
        .bg(theme.surface.panel)
        .border_l_1()
        .border_color(theme.border.default)
        .when_some(outline, |panel, outline| panel.child(outline))
        .when_some(links, |panel, links| panel.child(links))
        .when_some(panels.link_checker.clone(), |panel, checker| {
            panel.child(checker)
        })
}

/// A section head: chevron, title and, at the right, a count. A
/// separating line above it sits inside the head, which grows by its width.
pub(super) fn section_head(
    id: impl Into<ElementId>,
    title: &'static str,
    count: Option<usize>,
    open: bool,
    separated: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let height = if separated {
        size::PANEL_HEADER + HAIRLINE
    } else {
        size::PANEL_HEADER
    };
    let chevron = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    let name = match count {
        Some(n) => format!("{title} ({n})"),
        None => title.to_owned(),
    };
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(name)
        .aria_expanded(open)
        .keyboard_control(theme)
        .h(px(height))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(section_head::GAP))
        .px(px(section_head::PAD_X))
        .when(separated, |head| {
            head.border_t_1().border_color(theme.border.default)
        })
        .text_size(px(text::UI_SECTION.size))
        .line_height(px(text::UI_SECTION.line))
        .font_weight(FontWeight(text::weight::STRONG))
        .text_color(theme.text.secondary)
        .child(chevron.sized(icon::CHEVRON))
        .child(div().flex_1().min_w_0().truncate().child(title))
        .when_some(count, |head, count| {
            head.child(
                div()
                    .flex_none()
                    .font_weight(FontWeight(text::weight::ACTIVE))
                    .child(count.to_string()),
            )
        })
}
