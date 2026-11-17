// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! First-run onboarding (FR-SET-06): a welcome window shown once, on the
//! first start without files to open.
//!
//! One page, top to bottom:
//!
//! - **Start**: *Open folder…* opens a folder of notes (or an Obsidian
//!   vault) as a vault in the main window.
//! - **Theme**: System, Light, Semi-light or Dark (`appearance.theme`).
//! - **Markdown flavor**: what profiles are, and the profile used when
//!   detection finds nothing (`profiles.default_profile`).
//! - **Privacy**: nothing leaves the computer unless allowed here: the
//!   daily update check (`general.check_for_updates`), local crash reports
//!   (`privacy_and_security.crash_reports`) and remote images
//!   (`privacy_and_security.remote_content`). The two opt-ins start off.
//! - **Default app**: the offer to open Markdown files with Glyphvane
//!   ([`crate::integration::default_app`]).
//!
//! Every choice applies and is saved at once, for all vaults, as in the
//! settings window. *Get started*, Escape or closing the window ends the
//! onboarding for good (the application state remembers it); safe mode
//! never shows it.
//!
//! Not designed (NFR-UI-01): the whole window, built from the settings
//! window's rows and controls.

#[cfg(test)]
mod tests;

use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, FocusHandle, Focusable,
    FontWeight, Global, InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, TitlebarOptions,
    WeakEntity, Window, WindowBounds, WindowDecorations, WindowOptions, div, px, size,
};
use toml::Value;

use crate::app_state;
use crate::appearance::Appearance;
use crate::files::open_dialog::{self, OpenPrompt};
use crate::integration::default_app::{self, Offer};
use crate::metrics::{HAIRLINE, radius, text};
use crate::settings::{AppSettings, Target};
use crate::settings_window::controls::{self, OnClick, Segment};
use crate::settings_window::metrics::row as row_metrics;
use crate::theme::Theme;

gpui::actions!(
    onboarding,
    [
        /// Finish the onboarding and close its window.
        Dismiss,
    ]
);

/// The window's key context.
pub const KEY_CONTEXT: &str = "OnboardingWindow";

/// The window's size and its smallest size (not designed).
const WIDTH: f32 = 680.0;
const HEIGHT: f32 = 640.0;
const MIN_WIDTH: f32 = 480.0;
const MIN_HEIGHT: f32 = 400.0;
/// Padding of the window's content (not designed).
const PAD: f32 = 24.0;

/// The profiles offered, with a sentence each.
const PROFILES: &[(&str, &str, &str)] = &[
    (
        "commonmark",
        "CommonMark",
        "The standard core of Markdown, without extensions.",
    ),
    (
        "gfm-file",
        "GitHub",
        "As github.com shows files: tables, task lists, strikethrough, autolinks.",
    ),
    (
        "obsidian",
        "Obsidian",
        "Wikilinks, embeds, callouts, tags and properties, as in Obsidian.",
    ),
    (
        "extended",
        "Extended",
        "A broad superset for loose folders: GitHub and Obsidian syntax, footnotes, \
         highlights, sub- and superscript, front matter.",
    ),
];

/// Installs the window's keys. Called by [`crate::init`].
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT))]);
}

/// Whether the onboarding is still to be shown: not done before, and not
/// in safe mode.
pub fn should_show(cx: &App) -> bool {
    let safe_mode = cx
        .try_global::<AppSettings>()
        .is_some_and(AppSettings::safe_mode);
    !safe_mode && !app_state::get(cx).onboarding_done
}

/// Opens the onboarding when [`should_show`] says so. Returns the window.
pub fn show_if_first_run(cx: &mut App) -> Option<AnyWindowHandle> {
    if !should_show(cx) {
        return None;
    }
    match open(cx) {
        Ok(window) => Some(window),
        Err(err) => {
            tracing::warn!("onboarding: cannot open the window: {err}");
            None
        }
    }
}

/// The open onboarding window.
struct OpenWindow {
    window: AnyWindowHandle,
    view: WeakEntity<OnboardingWindow>,
}

impl Global for OpenWindow {}

/// The onboarding window and its view, while it is open.
pub fn onboarding_window(cx: &App) -> Option<(AnyWindowHandle, gpui::Entity<OnboardingWindow>)> {
    let open = cx.try_global::<OpenWindow>()?;
    let view = open.view.upgrade()?;
    cx.windows()
        .contains(&open.window)
        .then_some((open.window, view))
}

/// Opens the onboarding window, or brings it forward.
pub fn open(cx: &mut App) -> gpui::Result<AnyWindowHandle> {
    if let Some((window, _)) = onboarding_window(cx) {
        cx.update_window(window, |_, window, _| window.activate_window())?;
        return Ok(window);
    }
    let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Welcome to Glyphvane".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        window_decorations: Some(WindowDecorations::Server),
        window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
        app_id: Some(crate::APP_ID.to_owned()),
        ..WindowOptions::default()
    };
    let (window, view) = gpui::open_window(options, cx, |window, cx| {
        cx.new(|cx| OnboardingWindow::new(window, cx))
    })?;
    cx.set_global(OpenWindow {
        window,
        view: view.downgrade(),
    });
    cx.activate(true);
    Ok(window)
}

/// The onboarding window's view.
pub struct OnboardingWindow {
    focus: FocusHandle,
    /// What the default-app offer answered.
    default_app_status: Option<String>,
}

impl std::fmt::Debug for OnboardingWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnboardingWindow")
            .field("default_app_status", &self.default_app_status)
            .finish_non_exhaustive()
    }
}

impl Focusable for OnboardingWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl OnboardingWindow {
    fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        cx.observe_global::<AppSettings>(|_, cx| cx.notify())
            .detach();
        // However the window goes, the onboarding is done.
        cx.on_release(|_, cx| finish(cx)).detach();
        Self {
            focus,
            default_app_status: None,
        }
    }

    /// What the default-app offer answered, if it was used.
    pub fn default_app_status(&self) -> Option<&str> {
        self.default_app_status.as_deref()
    }

    /// Sets `key` for all vaults, as a control does.
    pub fn choose(&mut self, key: &str, value: Value, cx: &mut Context<'_, Self>) {
        if let Err(err) = AppSettings::set_value(cx, Target::AllVaults, key, &value) {
            tracing::warn!("onboarding: cannot set {key}: {err}");
        }
        cx.notify();
    }

    /// Open folder…: asks for a folder and opens it as a vault.
    pub fn open_folder(&mut self, cx: &mut Context<'_, Self>) {
        let answer = open_dialog::dialog(cx).ask(OpenPrompt::Folder, cx);
        cx.spawn(async move |_, cx| {
            let Ok(Some(paths)) = answer.await else {
                return;
            };
            let Some(folder) = paths.into_iter().next() else {
                return;
            };
            cx.update(|cx| {
                if let Err(err) = crate::open_folder(cx, &folder, crate::Placement::RecentWindow) {
                    tracing::warn!("onboarding: cannot open {}: {err}", folder.display());
                }
            });
        })
        .detach();
    }

    /// Carries out the default-app offer.
    pub fn make_default_app(&mut self, cx: &mut Context<'_, Self>) {
        let view = cx.entity().downgrade();
        default_app::apply(cx, move |result, cx| {
            let message = match result {
                Ok(message) => message,
                Err(err) => format!("Not changed: {err}."),
            };
            let _ = view.update(cx, |this, cx| {
                this.default_app_status = Some(message);
                cx.notify();
            });
        });
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<'_, Self>) {
        finish(cx);
        window.remove_window();
    }

    /// A click handler that runs `f` on this view.
    fn on_click(
        &self,
        cx: &mut Context<'_, Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<'_, Self>) + 'static,
    ) -> OnClick {
        let view = cx.entity().downgrade();
        Box::new(move |_, window, cx| {
            let _ = view.update(cx, |this, cx| f(this, window, cx));
        })
    }

    fn segments(
        &self,
        key: &'static str,
        current: &str,
        options: &[(&'static str, &'static str)],
        cx: &mut Context<'_, Self>,
    ) -> Vec<Segment> {
        options
            .iter()
            .map(|&(value, label)| Segment {
                id: format!("onboarding-{key}-{value}").into(),
                label: label.into(),
                active: current == value,
                on_click: Some(self.on_click(cx, move |this, _, cx| {
                    this.choose(key, Value::String(value.to_owned()), cx)
                })),
            })
            .collect()
    }

    fn switch(
        &self,
        key: &'static str,
        on: bool,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        controls::switch(
            SharedString::from(format!("onboarding-switch-{key}")),
            on,
            true,
            self.on_click(cx, move |this, _, cx| {
                this.choose(key, Value::Boolean(!on), cx)
            }),
            theme,
        )
        .debug_selector(move || format!("onboarding-switch-{key}"))
        .into_any_element()
    }

    fn group(title: &'static str, first: bool, rows: Vec<AnyElement>, theme: &Theme) -> AnyElement {
        div()
            .child(controls::heading(title, first, theme))
            .child(
                div()
                    .border_b(px(HAIRLINE))
                    .border_color(theme.border.default)
                    .children(rows),
            )
            .into_any_element()
    }
}

/// Marks the onboarding as done.
fn finish(cx: &mut App) {
    if !app_state::get(cx).onboarding_done {
        app_state::update(cx, |state| state.onboarding_done = true);
    }
}

/// A button of the window: `primary` on `accent.default`, otherwise the
/// secondary look.
fn button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    on_click: OnClick,
    theme: &Theme,
) -> AnyElement {
    let (hover, press) = (theme.state.hover, theme.state.press);
    let base = div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .h(px(crate::metrics::size::CONTROL))
        .px(px(row_metrics::SEGMENT_PAD_X))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .font_weight(FontWeight(text::weight::ACTIVE))
        .on_click(on_click)
        .child(label);
    if primary {
        base.bg(theme.accent.default)
            .text_color(theme.accent.on)
            .hover(|s| s.opacity(0.9))
            .active(|s| s.opacity(0.8))
            .into_any_element()
    } else {
        base.border_1()
            .border_color(theme.border.strong)
            .bg(theme.surface.raised)
            .hover(move |s| s.bg(hover))
            .active(move |s| s.bg(press))
            .into_any_element()
    }
}

impl Render for OnboardingWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let settings = cx.global::<AppSettings>().settings().clone();

        let start = vec![controls::row(
            "onboarding-open-folder-row",
            "Open a folder of notes",
            Some(
                "A folder, or an Obsidian vault, opens as a vault: links, search and the Files \
                 panel work across all its notes. You can also open single files at any time."
                    .into(),
            ),
            None,
            vec![button(
                "onboarding-open-folder",
                "Open folder…",
                false,
                self.on_click(cx, |this, _, cx| this.open_folder(cx)),
                theme,
            )],
            theme,
        )
        .into_any_element()];

        let theme_segments = self.segments(
            "appearance.theme",
            settings.appearance.theme.as_str(),
            &[
                ("system", "System"),
                ("light", "Light"),
                ("semi-light", "Semi-light"),
                ("dark", "Dark"),
            ],
            cx,
        );
        let look = vec![
            controls::row(
                "onboarding-theme-row",
                "Theme",
                Some("System follows your computer's light or dark appearance.".into()),
                None,
                vec![controls::segmented(theme_segments, true, theme).into_any_element()],
                theme,
            )
            .into_any_element(),
        ];

        let profile = settings.profiles.default_profile.as_str();
        let profile_note = PROFILES
            .iter()
            .find(|(id, _, _)| *id == profile)
            .map_or("", |(_, _, note)| note);
        let profile_segments = self.segments(
            "profiles.default_profile",
            profile,
            &PROFILES
                .iter()
                .map(|&(id, label, _)| (id, label))
                .collect::<Vec<_>>(),
            cx,
        );
        let flavor = vec![
            controls::row(
                "onboarding-profile-row",
                "Default profile",
                Some(
                    format!(
                        "Markdown comes in flavors. Glyphvane picks one per document from its \
                     folder and contents (an Obsidian vault, a GitHub repository), and uses \
                     this one when nothing says. {profile_note}"
                    )
                    .into(),
                ),
                None,
                vec![controls::segmented(profile_segments, true, theme).into_any_element()],
                theme,
            )
            .into_any_element(),
        ];

        let privacy = settings.privacy_and_security.clone();
        let remote_segments = self.segments(
            "privacy_and_security.remote_content",
            privacy.remote_content.as_str(),
            &[("ask", "Ask"), ("allow", "Allow"), ("block", "Block")],
            cx,
        );
        let privacy_rows = vec![
            controls::row(
                "onboarding-updates-row",
                "Check for updates",
                Some(
                    "Once a day, ask GitHub which Glyphvane release is the newest. Nothing \
                     about you or your notes is sent, and nothing is installed."
                        .into(),
                ),
                None,
                vec![self.switch(
                    "general.check_for_updates",
                    settings.general.check_for_updates,
                    theme,
                    cx,
                )],
                theme,
            )
            .into_any_element(),
            controls::row(
                "onboarding-crash-row",
                "Save crash reports",
                Some(
                    "When something goes wrong, write a report on this computer. It is never \
                     sent; you can add it to a bug report yourself."
                        .into(),
                ),
                None,
                vec![self.switch(
                    "privacy_and_security.crash_reports",
                    privacy.crash_reports,
                    theme,
                    cx,
                )],
                theme,
            )
            .into_any_element(),
            controls::row(
                "onboarding-remote-row",
                "Images from the web",
                Some(
                    "Notes can show images from other sites, which then learn that you opened \
                     them. Ask shows them after you agree, per note."
                        .into(),
                ),
                None,
                vec![controls::segmented(remote_segments, true, theme).into_any_element()],
                theme,
            )
            .into_any_element(),
        ];

        let default_label = match default_app::offer() {
            Offer::SetDirectly => Some("Make default"),
            Offer::OpenSystemSettings => Some("Open settings"),
            Offer::Explain => None,
        };
        let default_rows = vec![
            controls::row(
                "onboarding-default-app-row",
                "Open Markdown files with Glyphvane",
                Some(
                    self.default_app_status
                        .clone()
                        .unwrap_or_else(|| default_app::description().to_owned())
                        .into(),
                ),
                None,
                default_label
                    .map(|label| {
                        button(
                            "onboarding-default-app",
                            label,
                            false,
                            self.on_click(cx, |this, _, cx| this.make_default_app(cx)),
                            theme,
                        )
                    })
                    .into_iter()
                    .collect(),
                theme,
            )
            .into_any_element(),
        ];

        div()
            .id("onboarding-window")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::dismiss))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface.page)
            .text_color(theme.text.primary)
            .font_family(Appearance::interface_font(cx))
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(
                div()
                    .id("onboarding-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(PAD))
                    .child(
                        div()
                            .debug_selector(|| "onboarding-title".into())
                            .text_size(px(text::UI_TITLE.size))
                            .line_height(px(text::UI_TITLE.line))
                            .font_weight(FontWeight(text::weight::STRONG))
                            .child("Welcome to Glyphvane"),
                    )
                    .child(
                        div()
                            .mt(px(row_metrics::NOTE_MARGIN_Y))
                            .text_color(theme.text.secondary)
                            .child(
                                "A few choices before you start. Each one applies at once, \
                                 and all of them can be changed later in Settings.",
                            ),
                    )
                    .child(Self::group("Start", true, start, theme))
                    .child(Self::group("Look", false, look, theme))
                    .child(Self::group("Markdown flavor", false, flavor, theme))
                    .child(Self::group(
                        "Privacy: nothing leaves this computer unless you allow it",
                        false,
                        privacy_rows,
                        theme,
                    ))
                    .child(Self::group("Default app", false, default_rows, theme)),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .justify_end()
                    .px(px(PAD))
                    .py(px(row_metrics::HEADING_GAP))
                    .border_t(px(HAIRLINE))
                    .border_color(theme.border.default)
                    .bg(theme.surface.window)
                    .child(button(
                        "onboarding-done",
                        "Get started",
                        true,
                        Box::new(|_, window, cx| {
                            finish(cx);
                            window.remove_window();
                        }),
                        theme,
                    )),
            )
    }
}
