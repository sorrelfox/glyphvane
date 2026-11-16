// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! About Glyphvane (`app.about`), the third-party licences (`app.licenses`,
//! NFR-LIC-03) and Copy debug info (`app.copy_debug_info`, FR-PLAT-06).
//!
//! - **The About window** shows the name, version, copyright, the licence
//!   and the source repository, with buttons for the third-party licences,
//!   Copy debug info and Close. Escape closes it.
//! - **Licenses** is a second page of the same window: the third-party
//!   notices, embedded in the binary (the same file the packages install
//!   and `glyphvane --licenses` prints), one line per row.
//! - **Copy debug info** copies what a bug report needs: the version, the
//!   operating system and display server, the GPU the focused window draws
//!   with, the theme, zoom and safe mode, the active document's profile and
//!   which settings differ from the defaults (names only, never values,
//!   paths or document contents).
//!
//! Not designed (built from the settings window's look and the infobar's
//! buttons, NFR-UI-01): the whole window, its size, the Licenses page and
//! the window's chrome. It keeps the system title bar on every platform;
//! where the system draws none (Wayland compositors without server-side
//! decorations), Close and Escape close it.

use std::fmt::{self, Write as _};
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, ClipboardItem, Context, Div, FocusHandle,
    FontWeight, Global, InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _,
    Render, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, TitlebarOptions,
    UniformListScrollHandle, WeakEntity, Window, WindowBounds, WindowDecorations, WindowOptions,
    div, px, size, uniform_list,
};

use crate::appearance::Appearance;
use crate::keymap::actions::app::{About, CopyDebugInfo, Licenses};
use crate::metrics::font;
use crate::metrics::size::infobar;
use crate::metrics::spacing::infobar as infobar_spacing;
use crate::metrics::{HAIRLINE, radius, text};
use crate::settings::AppSettings;
use crate::theme::Theme;

/// The third-party notices: every bundled component's licence and
/// copyright notice.
pub const THIRD_PARTY_NOTICES: &str = include_str!("../../../packaging/THIRD-PARTY-NOTICES.txt");

/// The application's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The application's copyright line.
pub const COPYRIGHT: &str = "Copyright (C) 2026 Glyphvane contributors";

/// The application's licence, in words.
pub const LICENSE: &str = "Licensed under the GNU General Public License, version 3 or later.";

/// Where the source code is.
pub const REPOSITORY: &str = "https://github.com/sorrelfox/glyphvane";

/// The key context of the About window.
pub const KEY_CONTEXT: &str = "AboutWindow";

/// The window's size and its smallest size (not designed).
const WIDTH: f32 = 560.0;
const HEIGHT: f32 = 480.0;
const MIN_WIDTH: f32 = 400.0;
const MIN_HEIGHT: f32 = 320.0;
/// Padding of the window's content (not designed).
const PAD: f32 = 24.0;
/// Space between the About page's paragraphs and between its buttons.
const GAP: f32 = 8.0;
/// Height of a notices line.
const NOTICE_LINE: f32 = text::UI_SMALL.line;

gpui::actions!(
    about,
    [
        /// Close the About window.
        Dismiss,
    ]
);

/// A page of the About window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    About,
    Licenses,
}

/// The About window's view.
pub struct AboutWindow {
    focus: FocusHandle,
    page: Page,
    /// Whether the debug info was just copied (the button says so).
    copied: bool,
    lines: Rc<[SharedString]>,
    scroll: UniformListScrollHandle,
}

impl fmt::Debug for AboutWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AboutWindow")
            .field("page", &self.page)
            .field("copied", &self.copied)
            .finish_non_exhaustive()
    }
}

struct OpenWindow {
    window: AnyWindowHandle,
    view: WeakEntity<AboutWindow>,
}

impl Global for OpenWindow {}

/// Installs the window's keys and the handlers of `app.about`,
/// `app.licenses` and `app.copy_debug_info`.
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT))]);
    cx.on_action(|_: &About, cx| show(Page::About, cx));
    cx.on_action(|_: &Licenses, cx| show(Page::Licenses, cx));
    cx.on_action(|_: &CopyDebugInfo, cx| copy_debug_info(cx));
}

fn show(page: Page, cx: &mut App) {
    if let Err(err) = open(page, cx) {
        tracing::warn!("about: cannot open the About window: {err}");
    }
}

/// The About window and its view, while it is open.
pub fn about_window(cx: &App) -> Option<(AnyWindowHandle, gpui::Entity<AboutWindow>)> {
    let open = cx.try_global::<OpenWindow>()?;
    let view = open.view.upgrade()?;
    cx.windows()
        .contains(&open.window)
        .then_some((open.window, view))
}

/// Opens the About window on `page`, or brings it forward on that page.
pub fn open(page: Page, cx: &mut App) -> gpui::Result<AnyWindowHandle> {
    if let Some((window, view)) = about_window(cx) {
        view.update(cx, |this, cx| this.set_page(page, cx));
        cx.update_window(window, |_, window, _| window.activate_window())?;
        return Ok(window);
    }
    let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("About Glyphvane".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        window_decorations: Some(WindowDecorations::Server),
        window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
        app_id: Some(crate::APP_ID.to_owned()),
        ..WindowOptions::default()
    };
    let (window, view) = gpui::open_window(options, cx, |window, cx| {
        cx.new(|cx| AboutWindow::new(page, window, cx))
    })?;
    cx.set_global(OpenWindow {
        window,
        view: view.downgrade(),
    });
    cx.activate(true);
    Ok(window)
}

/// Copies the debug info to the clipboard.
pub fn copy_debug_info(cx: &mut App) {
    let info = DebugInfo::collect(cx).to_string();
    cx.write_to_clipboard(ClipboardItem::new_string(info));
}

impl AboutWindow {
    fn new(page: Page, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            focus,
            page,
            copied: false,
            lines: THIRD_PARTY_NOTICES
                .lines()
                .map(|line| SharedString::from(line.to_owned()))
                .collect(),
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// The page shown.
    pub fn page(&self) -> Page {
        self.page
    }

    fn set_page(&mut self, page: Page, cx: &mut Context<'_, Self>) {
        self.page = page;
        self.copied = false;
        cx.notify();
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, _: &mut Context<'_, Self>) {
        window.remove_window();
    }

    fn about_page(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let copy_label = if self.copied {
            "Debug info copied"
        } else {
            "Copy debug info"
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .child(
                div()
                    .debug_selector(|| "about-name".into())
                    .text_size(px(text::PREVIEW_H2.size))
                    .line_height(px(text::PREVIEW_H2.line))
                    .font_weight(FontWeight(text::weight::STRONG))
                    .child("Glyphvane"),
            )
            .child(
                div()
                    .debug_selector(|| "about-version".into())
                    .text_color(theme.text.secondary)
                    .child(format!("Version {VERSION}")),
            )
            .child(div().child("A Markdown viewer and editor."))
            .child(
                div()
                    .text_color(theme.text.secondary)
                    .child(COPYRIGHT)
                    .child(LICENSE),
            )
            .child(
                div()
                    .text_color(theme.text.secondary)
                    .font_family(font::EDITOR)
                    .text_size(px(text::UI_SMALL.size))
                    .child(REPOSITORY),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(GAP))
                    .child(
                        button("about-licenses", "Third-party licenses", theme).on_click(
                            cx.listener(|this, _, _, cx| this.set_page(Page::Licenses, cx)),
                        ),
                    )
                    .child(button("about-copy-debug-info", copy_label, theme).on_click(
                        cx.listener(|this, _, _, cx| {
                            copy_debug_info(cx);
                            this.copied = true;
                            cx.notify();
                        }),
                    ))
                    .child(div().flex_1())
                    .child(
                        button("about-close", "Close", theme)
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            )
    }

    fn licenses_page(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let count = self.lines.len();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(GAP))
                    .child(
                        button("about-back", "About", theme)
                            .on_click(cx.listener(|this, _, _, cx| this.set_page(Page::About, cx))),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight(text::weight::STRONG))
                            .child("Third-party licenses"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .border_1()
                    .border_color(theme.border.default)
                    .rounded(px(radius::BLOCK))
                    .bg(theme.surface.sunken)
                    .p(px(GAP))
                    .font_family(font::EDITOR)
                    .text_size(px(text::UI_SMALL.size))
                    .line_height(px(NOTICE_LINE))
                    .child(
                        uniform_list(
                            "about-notices",
                            count,
                            cx.processor(|this, range: Range<usize>, _, _| {
                                range
                                    .map(|ix| {
                                        div()
                                            .h(px(NOTICE_LINE))
                                            .whitespace_nowrap()
                                            .child(this.lines[ix].clone())
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .debug_selector(|| "about-notices".into())
                        .size_full()
                        .track_scroll(&self.scroll),
                    ),
            )
    }
}

/// A bordered button like the infobar's (height 22, radius 5, 1 px
/// `border.strong`, `surface.raised`, 12/500, padding `0 9`).
fn button(id: &'static str, label: &'static str, theme: &Theme) -> Stateful<Div> {
    let (hover, press) = (theme.state.hover, theme.state.press);
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .h(px(infobar::BUTTON_HEIGHT))
        .px(px(infobar_spacing::BUTTON_PAD_X - HAIRLINE))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .border_1()
        .border_color(theme.border.strong)
        .bg(theme.surface.raised)
        .font_weight(FontWeight(text::weight::ACTIVE))
        .hover(move |s| s.bg(hover))
        .active(move |s| s.bg(press))
        .child(label)
}

impl Render for AboutWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        div()
            .id("about-window")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::dismiss))
            .size_full()
            .flex()
            .flex_col()
            .p(px(PAD))
            .bg(theme.surface.page)
            .text_color(theme.text.primary)
            .font_family(Appearance::interface_font(cx))
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .map(|root| match self.page {
                Page::About => root.child(self.about_page(theme, cx)),
                Page::Licenses => root.child(self.licenses_page(theme, cx)),
            })
    }
}

/// What Copy debug info copies.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugInfo {
    pub version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    /// Linux and FreeBSD: Wayland or X11.
    pub display_server: Option<&'static str>,
    /// The focused window's GPU: device, driver and whether it is a
    /// software renderer.
    pub gpu: Option<String>,
    pub theme: String,
    pub zoom_percent: u32,
    pub safe_mode: bool,
    /// The active document's profile.
    pub profile: Option<String>,
    /// The settings that differ from the defaults, by name.
    pub changed_settings: Vec<String>,
}

impl DebugInfo {
    /// The debug info of the running application.
    pub fn collect(cx: &mut App) -> Self {
        let recent = crate::shell::most_recent_window(cx);
        let gpu = recent.as_ref().and_then(|w| {
            w.window
                .update(cx, |_, window, _| window.gpu_specs())
                .ok()
                .flatten()
                .map(|specs| {
                    let mut gpu = format!("{} ({}", specs.device_name, specs.driver_name);
                    if !specs.driver_info.is_empty() {
                        let _ = write!(gpu, " {}", specs.driver_info);
                    }
                    gpu.push(')');
                    if specs.is_software_emulated {
                        gpu.push_str(", software renderer");
                    }
                    gpu
                })
        });
        let profile = recent.and_then(|w| {
            let workspace = w.workspace.upgrade()?;
            let tab = workspace.read(cx).active_tab();
            tab.profile().map(|p| format!("{:?}", p.id()))
        });
        let theme = format!(
            "{} (setting: {:?})",
            Appearance::theme(cx).name,
            Appearance::choice(cx)
        );
        let (safe_mode, changed_settings) = cx
            .try_global::<AppSettings>()
            .map(|s| (s.safe_mode(), changed_settings(s.settings())))
            .unwrap_or_default();
        Self {
            version: VERSION,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            display_server: display_server(),
            gpu,
            theme,
            zoom_percent: crate::zoom::percent(cx),
            safe_mode,
            profile,
            changed_settings,
        }
    }
}

impl fmt::Display for DebugInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Glyphvane {}", self.version)?;
        write!(f, "OS: {} ({})", self.os, self.arch)?;
        if let Some(server) = self.display_server {
            write!(f, ", {server}")?;
        }
        writeln!(f)?;
        writeln!(f, "GPU: {}", self.gpu.as_deref().unwrap_or("not reported"))?;
        writeln!(f, "Theme: {}", self.theme)?;
        writeln!(f, "Zoom: {}%", self.zoom_percent)?;
        writeln!(
            f,
            "Safe mode: {}",
            if self.safe_mode { "on" } else { "off" }
        )?;
        writeln!(
            f,
            "Profile: {}",
            self.profile.as_deref().unwrap_or("no document")
        )?;
        if self.changed_settings.is_empty() {
            write!(f, "Settings: all defaults")
        } else {
            write!(f, "Settings changed: {}", self.changed_settings.join(", "))
        }
    }
}

/// The names (`section.key`) of the settings in `settings` that differ
/// from the defaults.
pub fn changed_settings(settings: &gv_settings::Settings) -> Vec<String> {
    let (Ok(toml::Value::Table(now)), Ok(toml::Value::Table(defaults))) = (
        toml::Value::try_from(settings),
        toml::Value::try_from(gv_settings::Settings::default()),
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (section, values) in &now {
        let toml::Value::Table(values) = values else {
            continue;
        };
        let default = defaults.get(section).and_then(toml::Value::as_table);
        for (key, value) in values {
            if default.and_then(|d| d.get(key)) != Some(value) {
                out.push(format!("{section}.{key}"));
            }
        }
    }
    out.sort();
    out
}

/// The display server a Linux or FreeBSD session runs, as GPUI picks it:
/// Wayland when a Wayland socket is named, else X11.
fn display_server() -> Option<&'static str> {
    if !cfg!(any(target_os = "linux", target_os = "freebsd")) {
        return None;
    }
    let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    Some(if set("WAYLAND_DISPLAY") {
        "Wayland"
    } else if set("DISPLAY") {
        "X11"
    } else {
        "no display server found"
    })
}

#[cfg(test)]
mod tests;
