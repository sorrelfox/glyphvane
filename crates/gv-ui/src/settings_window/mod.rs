// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The settings window (`app.settings`, Mod+,; FR-SET-01, FR-SET-04).
//!
//! A separate window, 880 × 660 by default (SPEC § Settings window): a
//! sidebar with the window controls, a search field, the sections and a row
//! that opens `settings.toml`; a head with the section title and the scope
//! switch; the section's rows below. There is one settings window per
//! application; opening it again brings it forward.
//!
//! # What is designed
//!
//! - The frame, the sidebar, the head and the Appearance section with the
//!   theme picker ([`appearance`]).
//! - **Not designed** (built from the design's controls, NFR-UI-01): every
//!   other section, generated from the settings schema ([`model`]): the
//!   label from the key, the setting's documentation as the description, a
//!   switch, select, number or text field by value kind, and the unit from
//!   the key. Also not designed: search results (matching rows of every
//!   section under section headings), the inline errors, the Reset button
//!   of a value the scope sets, the select's list, the Keymap section (a
//!   button that opens `keymap.toml`, and the keymap editor,
//!   [`keymap_editor`]), the About section, the window's chrome on Windows
//!   (the system title bar) and Linux (a round close button in the
//!   sidebar's top row of a client-decorated window).
//!
//! # Behaviour
//!
//! - **Scope** (FR-SET-04). "All vaults" edits the global `settings.toml`;
//!   the vault's name edits that vault's overlay in the app data dir
//!   ([`AppSettings`]). The vault is the one of the main window the
//!   settings were opened from. Settings that only the global file may set
//!   are disabled in the vault scope. Values from the vault's shared
//!   `.glyphvane/settings.toml` are shown but never written.
//! - **Applying.** A switch, a select, a segment or a theme card applies at
//!   once; a text or number field applies on Enter or when it loses the
//!   keyboard. The value is checked against the schema first: an invalid
//!   one is not saved and its row shows why. A value the file already holds
//!   that is invalid, or that the layering ignored, shows its diagnostic in
//!   the row too.
//! - **Search** filters the settings of every section by label,
//!   description and key; the sidebar then lists only the sections with
//!   matches. Esc closes an open list, then clears the search, then closes
//!   the window.
//! - **Export and import** (`settings.export`, `settings.import`,
//!   FR-SET-05): rows in the About section; an import shows its changes
//!   for review before anything is saved ([`transfer`]).

mod appearance;
pub(crate) mod controls;
pub mod keymap_editor;
#[cfg(test)]
mod keymap_tests;
pub(crate) mod metrics;
pub mod model;
#[cfg(test)]
mod tests;
mod transfer;
#[cfg(test)]
mod transfer_tests;

pub use transfer::PendingImport;

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::assets::IconName;
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, Decorations, Entity,
    FocusHandle, Focusable, FontWeight, Global, InteractiveElement as _, IntoElement, KeyBinding,
    MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, TitlebarOptions, WeakEntity, Window, WindowBounds,
    WindowControlArea, WindowOptions, div, px, size,
};
use gv_settings::{Kind, Spec};
use toml::Value;

use self::controls::{OnClick, Segment};
use self::metrics::{content, field, head, row as row_metrics, select, sidebar, window as win};
use self::model::{Control, SectionId};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Axis, Role, RovingTabs};
use crate::appearance::Appearance;
use crate::chrome::WindowChrome;
use crate::keymap::actions;
use crate::metrics::size::{LIST_ROW, SETTINGS_SIDEBAR_WIDTH};
use crate::metrics::spacing::linux_controls;
use crate::metrics::{HAIRLINE, font, icon as icon_size, radius, text};
use crate::settings::{AppSettings, Target};
use crate::shell::{OpenTarget, Workspace, most_recent_window};
use crate::theme::Theme;

gpui::actions!(
    settings_window,
    [
        /// Close the open list, else clear the search, else close the
        /// window.
        Dismiss,
    ]
);

/// The settings window's key context.
pub const KEY_CONTEXT: &str = "SettingsWindow";

/// The window title.
const TITLE: &str = "Settings — Glyphvane";

/// Installs the window's keys, the `app.settings` handler, and the
/// tracking of the focused main window's vault for the settings layers.
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some("SettingsWindow > Input")),
    ]);
    cx.on_action(|_: &actions::app::Settings, cx| {
        if let Err(err) = open(cx) {
            tracing::warn!("settings: cannot open the settings window: {err}");
        }
    });
    cx.on_action(|_: &actions::settings::Export, cx| {
        with_window(cx, |this, cx| this.export_settings(cx));
    });
    cx.on_action(|_: &actions::settings::Import, cx| {
        with_window(cx, |this, cx| this.import_settings(cx));
    });
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        use_vault_of(workspace, cx);
        cx.observe_window_activation(window, |workspace, window, cx| {
            if window.is_window_active() {
                use_vault_of(workspace, cx);
            }
        })
        .detach();
    })
    .detach();
}

/// Opens the settings window on its About section and runs `f` on it:
/// `settings.export` and `settings.import` from the palette.
fn with_window(
    cx: &mut App,
    f: impl FnOnce(&mut SettingsWindow, &mut Context<'_, SettingsWindow>) + 'static,
) {
    // Deferred: the command may come from inside the settings window's own
    // update, which `open` cannot enter again.
    cx.defer(move |cx| {
        if let Err(err) = open(cx) {
            tracing::warn!("settings: cannot open the settings window: {err}");
            return;
        }
        if let Some((_, view)) = settings_window(cx) {
            view.update(cx, |this, cx| {
                this.select_section(SectionId::About, cx);
                f(this, cx);
            });
        }
    });
}

/// Uses the settings layers of `workspace`'s vault: the window's own
/// (opened as a folder, or made for the sidebar), else the one its active
/// document is in.
fn use_vault_of(workspace: &Workspace, cx: &mut App) {
    let Some(root) = vault_root_of(workspace, cx) else {
        return;
    };
    cx.defer(move |cx| AppSettings::open_vault(cx, &root));
}

/// The vault root of `workspace`, as [`use_vault_of`] picks it.
fn vault_root_of(workspace: &Workspace, cx: &App) -> Option<PathBuf> {
    if let Some(root) = workspace.vault_root(cx) {
        return Some(root);
    }
    let path = workspace.active_tab().path()?;
    Some(gv_vault::detect_root(path).root)
}

/// The vault root of the main window focused last.
fn recent_vault_root(cx: &mut App) -> Option<PathBuf> {
    let recent = most_recent_window(cx)?;
    let workspace = recent.workspace.upgrade()?;
    vault_root_of(workspace.read(cx), cx)
}

/// The open settings window.
struct OpenWindow {
    window: AnyWindowHandle,
    view: WeakEntity<SettingsWindow>,
}

impl Global for OpenWindow {}

/// The settings window and its view, if it is open.
pub fn settings_window(cx: &App) -> Option<(AnyWindowHandle, Entity<SettingsWindow>)> {
    let open = cx.try_global::<OpenWindow>()?;
    let view = open.view.upgrade()?;
    cx.windows()
        .contains(&open.window)
        .then_some((open.window, view))
}

/// Opens the settings window, or brings it forward, with the vault of the
/// main window focused last as its vault.
pub fn open(cx: &mut App) -> gpui::Result<AnyWindowHandle> {
    if let Some(root) = recent_vault_root(cx) {
        AppSettings::open_vault(cx, &root);
    }
    if let Some((window, view)) = settings_window(cx) {
        view.update(cx, |_, cx| cx.notify());
        cx.update_window(window, |_, window, _| window.activate_window())?;
        return Ok(window);
    }
    let options = window_options(cx);
    let (window, view) = gpui::open_window(options, cx, |window, cx| {
        cx.new(|cx| SettingsWindow::new(window, cx))
    })?;
    cx.set_global(OpenWindow {
        window,
        view: view.downgrade(),
    });
    cx.activate(true);
    Ok(window)
}

fn window_options(cx: &App) -> WindowOptions {
    let chrome = WindowChrome::current();
    let bounds = Bounds::centered(None, size(px(win::WIDTH), px(win::HEIGHT)), cx);
    let titlebar = match chrome {
        // Not designed: Windows keeps the system title bar, since the
        // window has no title row for caption buttons.
        WindowChrome::Windows => TitlebarOptions {
            title: Some(TITLE.into()),
            appears_transparent: false,
            traffic_light_position: None,
        },
        WindowChrome::MacOs | WindowChrome::Linux => chrome.titlebar(TITLE.into()),
    };
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(titlebar),
        window_decorations: Some(chrome.decorations()),
        app_owns_titlebar_drag: chrome.app_owns_titlebar_drag(),
        window_min_size: Some(size(px(win::MIN_WIDTH), px(win::MIN_HEIGHT))),
        app_id: Some(crate::APP_ID.to_owned()),
        ..WindowOptions::default()
    }
}

/// An element id made of a kind and a setting's key.
fn eid(kind: &str, path: &str) -> gpui::ElementId {
    gpui::ElementId::Name(format!("{kind}-{path}").into())
}

/// A Lucide icon at `size`, in the current text colour.
fn icon(name: IconName, size: f32) -> gpui::component::Icon {
    gpui::component::Icon::new(name).size(px(size))
}

/// The sidebar icon of a section.
fn section_icon(section: SectionId) -> IconName {
    match section {
        SectionId::General => IconName::SlidersHorizontal,
        SectionId::Appearance => IconName::Contrast,
        SectionId::Editor => IconName::PencilLine,
        SectionId::Preview => IconName::BookOpen,
        SectionId::Links => IconName::Link,
        SectionId::Files => IconName::FileText,
        SectionId::Vault => IconName::Folder,
        SectionId::Profiles => IconName::Layers2,
        SectionId::Spelling => IconName::SpellCheck,
        SectionId::Keymap => IconName::Keyboard,
        SectionId::PrivacyAndSecurity => IconName::Lock,
        SectionId::About => IconName::Info,
    }
}

/// The settings window's view.
pub struct SettingsWindow {
    section: SectionId,
    target: Target,
    search: Entity<InputState>,
    query: String,
    /// The text and number fields, by setting.
    fields: HashMap<&'static str, Entity<InputState>>,
    /// Why the last edit of a setting was refused.
    errors: HashMap<&'static str, String>,
    /// The setting whose select list is open.
    open_select: Option<&'static str>,
    /// The installed font families, for the font selects.
    fonts: Vec<SharedString>,
    /// An import waiting for Apply or Cancel.
    import: Option<PendingImport>,
    /// The last export or import message.
    transfer_status: Option<String>,
    /// The keymap editor, made when the Keymap section is first shown.
    keymap_editor: Option<Entity<keymap_editor::KeymapEditor>>,
    /// The sections' focus handles, for the arrow keys of the tab list.
    section_keys: RovingTabs,
    focus: FocusHandle,
    subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for SettingsWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsWindow")
            .field("section", &self.section)
            .field("target", &self.target)
            .field("query", &self.query)
            .field("errors", &self.errors)
            .finish_non_exhaustive()
    }
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsWindow {
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
        let search_events = cx.subscribe_in(&search, window, |this, input, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = input.read(cx).value().to_string();
                this.open_select = None;
                cx.notify();
            }
        });
        let settings_changed = cx.observe_global_in::<AppSettings>(window, |this, window, cx| {
            this.sync_fields(window, cx);
            cx.notify();
        });
        let mut fonts: Vec<SharedString> = cx
            .text_system()
            .all_font_names()
            .into_iter()
            .filter(|name| !name.starts_with('.'))
            .map(SharedString::from)
            .collect();
        fonts.sort();
        fonts.dedup();
        let focus = cx.focus_handle();
        search.update(cx, |input, cx| input.focus(window, cx));
        Self {
            section: SectionId::Appearance,
            target: Target::AllVaults,
            search,
            query: String::new(),
            fields: HashMap::new(),
            errors: HashMap::new(),
            open_select: None,
            fonts,
            import: None,
            transfer_status: None,
            keymap_editor: None,
            section_keys: RovingTabs::new(Axis::Vertical),
            focus,
            subscriptions: vec![search_events, settings_changed],
        }
    }

    /// The section shown.
    pub fn section(&self) -> SectionId {
        self.section
    }

    /// Shows `section`, as clicking it in the sidebar does.
    pub fn select_section(&mut self, section: SectionId, cx: &mut Context<'_, Self>) {
        self.section = section;
        self.open_select = None;
        cx.notify();
    }

    /// What edits change: all vaults or the open vault.
    pub fn target(&self) -> Target {
        self.target
    }

    /// Switches the scope, as the head's segmented control does. The vault
    /// scope needs an open vault.
    pub fn set_target(&mut self, target: Target, window: &mut Window, cx: &mut Context<'_, Self>) {
        if target == Target::Vault && cx.global::<AppSettings>().vault().is_none() {
            return;
        }
        self.target = target;
        self.errors.clear();
        self.open_select = None;
        self.sync_fields(window, cx);
        cx.notify();
    }

    /// The search query.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Replaces the search query, as typing it does.
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.search.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx)
        });
        self.query = query.to_owned();
        cx.notify();
    }

    /// The sections the sidebar lists: all of them, or with a query those
    /// with matching settings.
    pub fn listed_sections(&self) -> Vec<SectionId> {
        if self.query.trim().is_empty() {
            SectionId::ALL.to_vec()
        } else {
            model::sections_of(&model::search(&self.query))
        }
    }

    /// The settings the content lists, in order.
    pub fn listed_settings(&self) -> Vec<&'static str> {
        if self.query.trim().is_empty() {
            self.section.specs().iter().map(|s| s.path).collect()
        } else {
            model::search(&self.query).iter().map(|h| h.path).collect()
        }
    }

    /// The inline error of a setting's row, if any: a refused edit, else a
    /// problem with the value in a file.
    pub fn error(&self, path: &str, cx: &App) -> Option<String> {
        if let Some(error) = self.errors.get(path) {
            return Some(error.clone());
        }
        cx.global::<AppSettings>()
            .problems(self.target, path)
            .first()
            .map(|d| capitalised(&d.message))
    }

    /// The value shown for a setting in the current scope.
    pub fn value(&self, path: &str, cx: &App) -> Option<Value> {
        cx.global::<AppSettings>()
            .effective_for(self.target)
            .value(path)
            .cloned()
    }

    /// Sets a setting in the current scope, as its control does. A refused
    /// edit shows in the setting's row.
    pub fn apply(&mut self, path: &'static str, value: Value, cx: &mut Context<'_, Self>) {
        match AppSettings::set_value(cx, self.target, path, &value) {
            Ok(()) => {
                self.errors.remove(path);
            }
            Err(err) => {
                self.errors.insert(path, err.0);
            }
        }
        self.open_select = None;
        cx.notify();
    }

    /// Removes a setting from the current scope's file, as Reset does.
    pub fn reset(&mut self, path: &'static str, window: &mut Window, cx: &mut Context<'_, Self>) {
        match AppSettings::reset_value(cx, self.target, path) {
            Ok(()) => {
                self.errors.remove(path);
            }
            Err(err) => {
                self.errors.insert(path, err.0);
            }
        }
        self.sync_fields(window, cx);
        cx.notify();
    }

    /// Commits `text` as the value of a text or number field, as Enter
    /// does.
    pub fn commit_text(
        &mut self,
        path: &'static str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let input = self.field(path, window, cx);
        input.update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
        self.commit(path, cx);
    }

    /// Applies what the field of `path` holds, unless it is what is shown
    /// already.
    fn commit(&mut self, path: &'static str, cx: &mut Context<'_, Self>) {
        let (Some(spec), Some(input)) = (gv_settings::spec(path), self.fields.get(path)) else {
            return;
        };
        let text = input.read(cx).value().to_string();
        match model::parse_field(spec, &text) {
            Err(error) => {
                self.errors.insert(path, error);
                cx.notify();
            }
            Ok(value) => {
                let current = self.value(path, cx);
                if current.as_ref().is_some_and(|c| same_value(c, &value))
                    && !self.errors.contains_key(path)
                {
                    return;
                }
                self.apply(path, value, cx);
            }
        }
    }

    /// The field of a text or number setting, made on first use.
    fn field(
        &mut self,
        path: &'static str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<InputState> {
        if let Some(field) = self.fields.get(path) {
            return field.clone();
        }
        let text = self
            .value(path, cx)
            .map(|v| model::field_text(&v))
            .unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(text));
        let events = cx.subscribe_in(&input, window, move |this, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.commit(path, cx);
            }
        });
        self.subscriptions.push(events);
        self.fields.insert(path, input.clone());
        input
    }

    /// Shows the values in effect in every field that does not have the
    /// keyboard and holds no refused text.
    fn sync_fields(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let effective = cx.global::<AppSettings>().effective_for(self.target);
        for (path, input) in &self.fields {
            if self.errors.contains_key(path) {
                continue;
            }
            if input.read(cx).focus_handle(cx).is_focused(window) {
                continue;
            }
            let Some(value) = effective.value(path) else {
                continue;
            };
            let text = model::field_text(value);
            if input.read(cx).value().as_ref() != text {
                input.update(cx, |input, cx| input.set_value(text, window, cx));
            }
        }
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.open_select.take().is_some() {
            cx.notify();
        } else if !self.query.is_empty() {
            self.set_query("", window, cx);
        } else {
            window.remove_window();
        }
    }

    /// A click handler that runs `f` on this view.
    fn on_click(
        &self,
        cx: &mut Context<'_, Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<'_, Self>) + 'static,
    ) -> OnClick {
        let view = cx.entity().downgrade();
        Box::new(move |_, window, cx| {
            // Fails only when the window has closed.
            let _ = view.update(cx, |this, cx| f(this, window, cx));
        })
    }

    /// A click handler that sets `path` to `value`.
    fn set_on_click(
        &self,
        path: &'static str,
        value: Value,
        cx: &mut Context<'_, Self>,
    ) -> OnClick {
        self.on_click(cx, move |this, _, cx| this.apply(path, value.clone(), cx))
    }

    /// A click handler that opens or closes the list of `path`'s select.
    fn toggle_select(&self, path: &'static str, cx: &mut Context<'_, Self>) -> OnClick {
        self.on_click(cx, move |this, _, cx| {
            this.open_select = if this.open_select == Some(path) {
                None
            } else {
                Some(path)
            };
            cx.notify();
        })
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        self.section_keys.sync(self.listed_sections().len(), cx);
        div()
            .id("settings-window")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::dismiss))
            .size_full()
            .flex()
            .overflow_hidden()
            .bg(theme.surface.page)
            .text_color(theme.text.primary)
            .font_family(Appearance::interface_font(cx))
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(self.sidebar(theme, window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(self.head(theme, window, cx))
                    .child(
                        div()
                            .id("settings-content")
                            .role(Role::TabPanel)
                            .aria_label(self.section.label())
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .pt(px(content::PAD_TOP))
                            .px(px(content::PAD_X))
                            .pb(px(content::PAD_BOTTOM))
                            .children(self.content(theme, window, cx)),
                    ),
            )
    }
}

impl SettingsWindow {
    fn sidebar(&self, theme: &Theme, window: &Window, cx: &mut Context<'_, Self>) -> AnyElement {
        let search_focused = self.search.read(cx).focus_handle(cx).is_focused(window);
        let listed = self.listed_sections();
        let count = listed.len();
        let searching = !self.query.trim().is_empty();
        let hover = theme.state.hover;
        // One Tab stop; Up, Down, Home and End move between the sections
        // and show each (automatic activation). While searching nothing
        // is shown: the keys only move, and Enter shows the section.
        let shown = listed.iter().position(|s| !searching && *s == self.section);
        let stop = self.section_keys.stop(window, cx, shown);
        let settings = cx.entity().downgrade();
        let moves: std::rc::Rc<[SectionId]> = listed.clone().into();
        div()
            .w(px(SETTINGS_SIDEBAR_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.surface.panel)
            .border_r(px(HAIRLINE))
            .border_color(theme.border.default)
            .child(self.controls_row(theme, window, cx))
            .child(
                div()
                    .mx(px(sidebar::SEARCH_MARGIN_X))
                    .mb(px(sidebar::SEARCH_MARGIN_BOTTOM))
                    .child(
                        controls::field_frame(search_focused, false, None, theme)
                            .h(px(crate::metrics::size::FIELD))
                            .bg(theme.surface.page)
                            .gap(px(sidebar::SEARCH_GAP))
                            .px(px(sidebar::SEARCH_PAD_X))
                            .child(
                                div()
                                    .text_color(theme.text.secondary)
                                    .child(icon(IconName::Search, sidebar::SEARCH_ICON)),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.search)
                                        .aria_label("Search settings")
                                        .appearance(false)
                                        .h(px(text::UI.line))
                                        .px_0()
                                        .py_0()
                                        .text_size(px(text::UI.size))
                                        .text_color(theme.text.primary),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .id("settings-sections")
                    .role(Role::TabList)
                    .aria_label("Sections")
                    .aria_orientation(gpui::Orientation::Vertical)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(listed.into_iter().enumerate().map(|(index, section)| {
                        let current = !searching && section == self.section;
                        div()
                            .id(section.slug())
                            .a11y_tab(section.label(), current, index, count)
                            .roving_tab(theme, &self.section_keys, index, index == stop, {
                                let settings = settings.clone();
                                let moves = moves.clone();
                                move |to, _, cx| {
                                    let Some(&section) = moves.get(to).filter(|_| !searching)
                                    else {
                                        return;
                                    };
                                    let _ = settings
                                        .update(cx, |this, cx| this.select_section(section, cx));
                                }
                            })
                            .debug_selector(move || format!("settings-section-{}", section.slug()))
                            .h(px(LIST_ROW))
                            .flex_none()
                            .mx(px(sidebar::ROW_MARGIN_X))
                            .px(px(sidebar::ROW_PAD_X))
                            .rounded(px(radius::CONTROL))
                            .flex()
                            .items_center()
                            .gap(px(sidebar::ROW_GAP))
                            .when(current, |d| {
                                d.bg(theme.accent.tint)
                                    .font_weight(FontWeight(text::weight::ACTIVE))
                            })
                            .when(!current, |d| d.hover(move |s| s.bg(hover)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_query("", window, cx);
                                this.select_section(section, cx);
                            }))
                            .child(
                                div()
                                    .text_color(theme.text.secondary)
                                    .child(icon(section_icon(section), icon_size::ROW)),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(section.label()))
                            .accessible()
                    })),
            )
            .child(
                div()
                    .id("settings-file")
                    .role(Role::Link)
                    .aria_label(format!("Open {}", gv_settings::SETTINGS_FILE))
                    .keyboard_control(theme)
                    .debug_selector(|| "settings-file".into())
                    .h(px(sidebar::FILE_ROW))
                    .flex_none()
                    .px(px(sidebar::FILE_ROW_PAD_X))
                    .flex()
                    .items_center()
                    .gap(px(sidebar::ROW_GAP))
                    .border_t(px(HAIRLINE))
                    .border_color(theme.border.default)
                    .text_color(theme.text.secondary)
                    .text_size(px(sidebar::FILE_ROW_TEXT))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(|this, _, _, cx| this.open_settings_file(cx)))
                    .child(icon(IconName::FileText, sidebar::FILE_ICON))
                    .child(
                        div()
                            .flex_1()
                            .font_family(font::EDITOR)
                            .text_size(px(sidebar::FILE_NAME_TEXT))
                            .child(gv_settings::SETTINGS_FILE),
                    )
                    .child(icon(IconName::ArrowUpRight, sidebar::FILE_ARROW)),
            )
            .into_any_element()
    }

    /// The sidebar's top row: room for the system's window controls on
    /// macOS, the close button of a client-decorated window on Linux, and
    /// a place to drag the window by.
    fn controls_row(
        &self,
        theme: &Theme,
        window: &Window,
        _: &mut Context<'_, Self>,
    ) -> AnyElement {
        let chrome = WindowChrome::current();
        let client_decorated = matches!(window.window_decorations(), Decorations::Client { .. });
        let moves_itself = chrome == WindowChrome::MacOs || client_decorated;
        let hover = theme.state.hover;
        div()
            .id("settings-window-controls")
            .h(px(sidebar::CONTROLS_ROW))
            .flex_none()
            .flex()
            .items_center()
            .px(px(sidebar::CLOSE_INSET))
            .window_control_area(WindowControlArea::Drag)
            .when(moves_itself, |d| {
                d.on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            })
            .when(client_decorated && chrome == WindowChrome::Linux, |d| {
                d.child(
                    div()
                        .id("settings-close")
                        .a11y_button("Close")
                        .keyboard_control(theme)
                        .debug_selector(|| "settings-close".into())
                        .size(px(linux_controls::DIAMETER))
                        .rounded_full()
                        .bg(theme.state.press)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(move |s| s.bg(hover))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(|_, window, _| window.remove_window())
                        .child(crate::icons::Icon::WindowClose.sized(linux_controls::GLYPH)),
                )
            })
            .into_any_element()
    }

    fn head(&self, theme: &Theme, window: &mut Window, cx: &mut Context<'_, Self>) -> AnyElement {
        let title: SharedString = if self.import.is_some() {
            "Import settings".into()
        } else if self.query.trim().is_empty() {
            self.section.label().into()
        } else {
            "Search results".into()
        };
        let vault = cx
            .global::<AppSettings>()
            .vault()
            .map(|v| SharedString::from(v.name().to_owned()));
        let mut segments = vec![Segment {
            id: "settings-scope-all".into(),
            label: "All vaults".into(),
            active: self.target == Target::AllVaults,
            on_click: Some(self.on_click(cx, |this, window, cx| {
                this.set_target(Target::AllVaults, window, cx)
            })),
        }];
        // Not designed: without an open vault, only "All vaults" is shown.
        if let Some(name) = vault {
            segments.push(Segment {
                id: "settings-scope-vault".into(),
                label: name,
                active: self.target == Target::Vault,
                on_click: Some(self.on_click(cx, |this, window, cx| {
                    this.set_target(Target::Vault, window, cx)
                })),
            });
        }
        let _ = window;
        div()
            .min_h(px(head::HEIGHT))
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(head::GAP_X))
            .py(px(head::PAD_Y))
            .pl(px(head::PAD_LEFT))
            .pr(px(head::PAD_RIGHT))
            .bg(theme.surface.window)
            .border_b(px(HAIRLINE))
            .border_color(theme.border.default)
            .child(
                div()
                    .flex_1()
                    .debug_selector(|| "settings-title".into())
                    .text_size(px(text::UI_TITLE.size))
                    .line_height(px(text::UI_TITLE.line))
                    .font_weight(FontWeight(text::weight::STRONG))
                    .child(title),
            )
            .child(controls::segmented(segments, true, theme))
            .into_any_element()
    }

    fn content(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        if self.import.is_some() {
            return self.import_preview(theme, cx);
        }
        if !self.query.trim().is_empty() {
            return self.search_results(theme, window, cx);
        }
        match self.section {
            SectionId::Appearance => self.appearance(theme, window, cx),
            SectionId::Keymap => self.keymap_section(theme, window, cx),
            SectionId::About => self.about_section(theme, cx),
            section => {
                let specs = section.specs();
                vec![self.generated_group(None, &specs, true, theme, window, cx)]
            }
        }
    }

    /// Matching settings of every section under the sections' names (not
    /// designed).
    fn search_results(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let hits = model::search(&self.query);
        if hits.is_empty() {
            return vec![
                div()
                    .debug_selector(|| "settings-no-results".into())
                    .text_color(theme.text.secondary)
                    .child("No settings match.")
                    .into_any_element(),
            ];
        }
        model::sections_of(&hits)
            .into_iter()
            .enumerate()
            .map(|(index, section)| {
                let specs: Vec<&'static Spec> = hits
                    .iter()
                    .filter(|h| h.section == section)
                    .filter_map(|h| gv_settings::spec(h.path))
                    .collect();
                self.generated_group(Some(section.label()), &specs, index == 0, theme, window, cx)
            })
            .collect()
    }

    /// A group of generated rows (not designed).
    fn generated_group(
        &mut self,
        title: Option<&'static str>,
        specs: &[&'static Spec],
        first: bool,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let rows: Vec<AnyElement> = specs
            .iter()
            .map(|spec| self.generated_row(spec, theme, window, cx))
            .collect();
        div()
            .when_some(title, |d, title| {
                d.child(controls::heading(title, first, theme))
            })
            .child(
                div()
                    .border_b(px(HAIRLINE))
                    .border_color(theme.border.default)
                    .children(rows),
            )
            .into_any_element()
    }

    /// One generated row: label, documentation, the control for the value
    /// kind and, when the scope sets the value, Reset (not designed).
    fn generated_row(
        &mut self,
        spec: &'static Spec,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let path = spec.path;
        let enabled = self.target == Target::AllVaults || model::per_vault(spec);
        let mut description = spec.doc.clone();
        if !enabled {
            description.push_str(" Applies to all vaults only.");
        }
        let settings = cx.global::<AppSettings>();
        if self.target == Target::Vault
            && settings.effective().source(path) == Some(gv_settings::LayerKind::InVault)
        {
            description.push_str(" Set by the vault's .glyphvane/settings.toml.");
        }
        let sets = settings.sets(self.target, path);
        let error = self.error(path, cx);
        let mut controls_: Vec<AnyElement> = Vec::new();
        if sets {
            let label = match self.target {
                Target::AllVaults => "Reset",
                Target::Vault => "Use all vaults",
            };
            controls_.push(
                controls::quiet_button(
                    eid("reset", path),
                    label,
                    self.on_click(cx, move |this, window, cx| this.reset(path, window, cx)),
                    theme,
                )
                .debug_selector(move || format!("settings-reset-{path}"))
                .into_any_element(),
            );
        }
        controls_.push(self.control(spec, enabled, error.is_some(), theme, window, cx));
        controls::row(
            eid("row", path),
            model::row_label(spec),
            Some(description.into()),
            error.map(SharedString::from),
            controls_,
            theme,
        )
        .debug_selector(move || format!("settings-row-{path}"))
        .into_any_element()
    }

    /// The control of a setting, by its value kind.
    fn control(
        &mut self,
        spec: &'static Spec,
        enabled: bool,
        error: bool,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let path = spec.path;
        let value = self.value(path, cx);
        match Control::of(spec) {
            Control::Switch => {
                let on = value.as_ref().and_then(Value::as_bool).unwrap_or(false);
                controls::switch(
                    eid("switch", path),
                    on,
                    enabled,
                    self.set_on_click(path, Value::Boolean(!on), cx),
                    theme,
                )
                .debug_selector(move || format!("settings-switch-{path}"))
                .into_any_element()
            }
            Control::Select => {
                let Kind::Enum(variants) = spec.kind else {
                    return div().into_any_element();
                };
                let current = value
                    .as_ref()
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let options = variants
                    .iter()
                    .map(|v| ((*v).to_owned(), model::variant_label(spec, v)))
                    .collect();
                self.select(path, &current, options, enabled, theme, cx)
            }
            Control::Number => {
                let unit = model::unit(spec);
                self.text_field(
                    path,
                    Some(field::NUMBER_WIDTH),
                    unit,
                    enabled,
                    error,
                    theme,
                    window,
                    cx,
                )
            }
            Control::Text | Control::List => {
                self.text_field(path, None, None, enabled, error, theme, window, cx)
            }
        }
    }

    /// A select and, while open, its list. `options` are values with their
    /// labels.
    fn select(
        &self,
        path: &'static str,
        current: &str,
        options: Vec<(String, String)>,
        enabled: bool,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let label = options
            .iter()
            .find(|(v, _)| v == current)
            .map_or_else(|| current.to_owned(), |(_, l)| l.clone());
        let open = self.open_select == Some(path);
        let list = open.then(|| {
            let items = options
                .into_iter()
                .map(|(value, label)| {
                    let chosen = value == current;
                    let selector = format!("settings-option-{path}-{value}");
                    let handler = self.set_on_click(path, Value::String(value), cx);
                    (SharedString::from(label), selector, chosen, handler)
                })
                .collect();
            let close = cx.listener(move |this, _, _, cx| {
                if this.open_select == Some(path) {
                    this.open_select = None;
                    cx.notify();
                }
            });
            div()
                .absolute()
                .top(px(crate::metrics::size::CONTROL + select::LIST_OFFSET))
                .left_0()
                .child(
                    gpui::deferred(
                        gpui::anchored()
                            .snap_to_window_with_margin(px(select::LIST_OFFSET))
                            .child(
                                controls::select_list(
                                    eid("list", path),
                                    items,
                                    select::MIN_WIDTH,
                                    theme,
                                )
                                .debug_selector(move || format!("settings-list-{path}"))
                                .on_mouse_down_out(close),
                            ),
                    )
                    .with_priority(1),
                )
        });
        div()
            .relative()
            .flex_none()
            .child(
                controls::select_button(
                    eid("select", path),
                    label,
                    enabled,
                    self.toggle_select(path, cx),
                    theme,
                )
                .debug_selector(move || format!("settings-select-{path}")),
            )
            .children(list)
            .into_any_element()
    }

    /// A text or number field; `width` fixes a number field's width.
    #[allow(clippy::too_many_arguments)]
    fn text_field(
        &mut self,
        path: &'static str,
        width: Option<f32>,
        unit: Option<&'static str>,
        enabled: bool,
        error: bool,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let input = self.field(path, window, cx);
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let number = width.is_some();
        controls::field_frame(focused, error, width, theme)
            .debug_selector(move || format!("settings-field-{path}"))
            .when(!enabled, |d| d.opacity(crate::metrics::DISABLED_OPACITY))
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&input)
                        .aria_label(SharedString::from(path))
                        .appearance(false)
                        .disabled(!enabled)
                        .h(px(text::UI.line))
                        .px_0()
                        .py_0()
                        .when(number, |i| {
                            i.font_family(font::EDITOR)
                                .text_size(px(field::NUMBER_TEXT))
                        })
                        .when(!number, |i| i.text_size(px(text::UI.size)))
                        .text_color(theme.text.primary),
                ),
            )
            .when_some(unit, |d, unit| d.child(controls::unit(unit, theme)))
            .into_any_element()
    }

    /// The keymap editor of the Keymap section, made on first use.
    pub fn keymap_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<keymap_editor::KeymapEditor> {
        self.keymap_editor
            .get_or_insert_with(|| cx.new(|cx| keymap_editor::KeymapEditor::new(window, cx)))
            .clone()
    }

    /// The Keymap section (not designed): where the keymap file is, a
    /// button that opens it, and the keymap editor (FR-SET-02).
    fn keymap_section(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let editor = self.keymap_editor(window, cx);
        let path = cx
            .global::<AppSettings>()
            .locations()
            .map(|l| l.keymap_file());
        let description: SharedString = match &path {
            Some(path) => format!(
                "Keys are kept in {}. Changes made below or in the file apply at once.",
                path.display()
            )
            .into(),
            None => "There is no keymap file without a home folder.".into(),
        };
        let button = path.map(|path| {
            secondary_button(
                "settings-open-keymap",
                "Open keymap.toml",
                self.on_click(cx, move |_, _, cx| open_file(path.clone(), cx)),
                theme,
            )
            .into_any_element()
        });
        vec![
            div()
                .border_b(px(HAIRLINE))
                .border_color(theme.border.default)
                .child(controls::row(
                    "keymap-file",
                    "Keyboard shortcuts",
                    Some(description),
                    None,
                    button.into_iter().collect(),
                    theme,
                ))
                .into_any_element(),
            editor.into_any_element(),
        ]
    }

    /// The About section (not designed): the version and where the
    /// settings are.
    fn about_section(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let settings = cx.global::<AppSettings>();
        let mut rows = vec![("Version", env!("CARGO_PKG_VERSION").to_owned())];
        if let Some(file) = settings.global_file() {
            rows.push(("Settings for all vaults", file.path().display().to_string()));
        }
        if let Some(file) = settings.vault().and_then(|v| v.overlay()) {
            rows.push(("Settings for this vault", file.path().display().to_string()));
        }
        if settings.safe_mode() {
            rows.push((
                "Safe mode",
                "On: changes apply until the app quits and are not saved.".to_owned(),
            ));
        }
        let transfer = self.transfer_rows(theme, cx);
        vec![
            div()
                .border_b(px(HAIRLINE))
                .border_color(theme.border.default)
                .children(rows.into_iter().enumerate().map(|(index, (label, value))| {
                    controls::row(
                        ("about", index),
                        label,
                        None,
                        None,
                        vec![
                            div()
                                .text_color(theme.text.secondary)
                                .child(value)
                                .into_any_element(),
                        ],
                        theme,
                    )
                }))
                .into_any_element(),
            transfer,
        ]
    }

    /// Opens the settings file of the current scope in the main window,
    /// creating it when it does not exist yet (not in safe mode).
    fn open_settings_file(&mut self, cx: &mut Context<'_, Self>) {
        let settings = cx.global::<AppSettings>();
        let Some(file) = settings.file_of(self.target) else {
            return;
        };
        let path = file.path().to_path_buf();
        if !path.exists() && !settings.safe_mode() {
            let text = format!("version = {}\n", gv_settings::CURRENT_VERSION);
            if let Err(err) = gv_settings::atomic_write(&path, text.as_bytes()) {
                tracing::warn!("settings: cannot create {}: {err}", path.display());
                return;
            }
        }
        open_file(path, cx);
    }
}

/// A secondary button: 24 high, a `border.strong` hairline on
/// `surface.raised`, 12/500.
fn secondary_button(
    id: &'static str,
    label: &'static str,
    on_click: OnClick,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let (hover, press) = (theme.state.hover, theme.state.press);
    div()
        .id(id)
        .a11y_button(label)
        .keyboard_control(theme)
        .debug_selector(move || id.into())
        .flex_none()
        .h(px(crate::metrics::size::CONTROL))
        .px(px(row_metrics::SEGMENT_PAD_X))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(theme.border.strong)
        .bg(theme.surface.raised)
        .font_weight(FontWeight(text::weight::ACTIVE))
        .hover(move |s| s.bg(hover))
        .active(move |s| s.bg(press))
        .on_click(on_click)
        .child(label)
}

/// Opens `path` as a tab of the main window focused last.
fn open_file(path: PathBuf, cx: &mut App) {
    if let Err(err) = crate::open(
        cx,
        vec![OpenTarget::new(path)],
        crate::Placement::RecentWindow,
    ) {
        tracing::warn!("settings: cannot open the file: {err}");
    }
}

/// Whether two values are the same setting value (numbers compared as
/// numbers).
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Integer(y)) | (Value::Integer(y), Value::Float(x)) => {
            *x == *y as f64
        }
        _ => a == b,
    }
}

fn capitalised(message: &str) -> String {
    let mut chars = message.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
