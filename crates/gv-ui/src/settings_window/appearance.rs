// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Appearance section, as designed (SPEC § Settings window, the
//! `settings.*` screens): the theme picker, the light theme System uses,
//! high contrast; the Text group (fonts and sizes, readable line width,
//! zoom); the Code blocks group.
//!
//! The theme cards' previews are tiny windows drawn in each theme's own
//! colours, taken from that theme's roles.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, relative,
};
use toml::Value;

use super::SettingsWindow;
use super::controls::{self, Segment};
use super::metrics::{field, row, theme_card};
use crate::appearance::Appearance;
use crate::metrics::size::THEME_CARD_PREVIEW;
use crate::metrics::{DISABLED_OPACITY, HAIRLINE, text};
use crate::settings::AppSettings;
use crate::theme::{Theme, ThemeChoice, ThemeId};

const THEME: &str = "appearance.theme";
const PREFERRED_LIGHT: &str = "appearance.preferred_light_theme";
const HIGH_CONTRAST: &str = "appearance.high_contrast";
const READABLE: &str = "appearance.readable_line_width";
const READABLE_CHARS: &str = "appearance.readable_line_width_chars";
const ZOOM: &str = "appearance.zoom_percent";
const CODE_THEME: &str = "appearance.code_theme";
/// The zoom stepper's step, in percent.
const ZOOM_STEP: i64 = 10;

/// The font rows: label, description, family key, size key.
const FONTS: [(&str, Option<&str>, &str, &str); 3] = [
    (
        "Interface font",
        None,
        "appearance.interface_font",
        "appearance.interface_font_size",
    ),
    (
        "Preview font",
        None,
        "appearance.preview_font",
        "appearance.preview_font_size",
    ),
    (
        "Editor font",
        Some("A monospace font keeps Markdown columns and tables aligned."),
        "appearance.editor_font",
        "appearance.editor_font_size",
    ),
];

fn choice_name(choice: ThemeChoice) -> &'static str {
    match choice {
        ThemeChoice::System => "System",
        ThemeChoice::Light => "Light",
        ThemeChoice::SemiLight => "Semi-light",
        ThemeChoice::Dark => "Dark",
    }
}

fn theme_name(id: ThemeId) -> &'static str {
    match id {
        ThemeId::Light => "Light",
        ThemeId::SemiLight => "Semi-light",
        ThemeId::Dark => "Dark",
    }
}

fn choice_value(choice: ThemeChoice) -> &'static str {
    gv_settings::ThemeChoice::from(choice).as_str()
}

/// The operating system's name in the sentence under the cards.
fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "The system"
    }
}

impl SettingsWindow {
    pub(super) fn appearance(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let settings = cx
            .global::<AppSettings>()
            .effective_for(self.target)
            .settings()
            .appearance
            .clone();
        let choice: ThemeChoice = settings.theme.into();
        let preferred_light = settings.preferred_light_theme;

        let mut out = vec![controls::heading("Theme", true, theme).into_any_element()];
        out.push(self.theme_cards(choice, preferred_light.into(), theme, cx));
        out.push(
            div()
                .debug_selector(|| "settings-theme-note".into())
                .my(px(row::NOTE_MARGIN_Y))
                .text_size(px(text::UI_SECTION.size))
                .text_color(theme.text.secondary)
                .child(self.showing_sentence(choice, cx))
                .into_any_element(),
        );

        // In light appearance, use: only for System.
        let system = choice == ThemeChoice::System;
        let segments = [
            (gv_settings::PreferredLightTheme::Light, "Light"),
            (gv_settings::PreferredLightTheme::SemiLight, "Semi-light"),
        ]
        .into_iter()
        .map(|(value, label)| Segment {
            id: format!("settings-preferred-{}", value.as_str()).into(),
            label: label.into(),
            active: preferred_light == value,
            on_click: Some(self.set_on_click(
                PREFERRED_LIGHT,
                Value::String(value.as_str().to_owned()),
                cx,
            )),
        })
        .collect();
        out.push(
            controls::row(
                "preferred-light",
                "In light appearance, use",
                Some("Applies to System. Dark appearance always uses Dark.".into()),
                self.error(PREFERRED_LIGHT, cx).map(SharedString::from),
                vec![controls::segmented(segments, system, theme).into_any_element()],
                theme,
            )
            .debug_selector(|| format!("settings-row-{PREFERRED_LIGHT}"))
            .when(!system, |d| d.opacity(DISABLED_OPACITY))
            .into_any_element(),
        );

        // High contrast (the themes themselves are not designed yet).
        let spec = gv_settings::spec(HIGH_CONTRAST);
        let current = settings.high_contrast.as_str();
        let options = gv_settings::HighContrast::VARIANTS
            .iter()
            .map(|v| {
                let label =
                    spec.map_or_else(|| (*v).to_owned(), |s| super::model::variant_label(s, v));
                ((*v).to_owned(), label)
            })
            .collect();
        let select = self.select(HIGH_CONTRAST, current, options, true, theme, cx);
        out.push(
            controls::row(
                "high-contrast",
                "High contrast",
                Some("Switches to the high-contrast theme when the system asks for it.".into()),
                self.error(HIGH_CONTRAST, cx).map(SharedString::from),
                vec![select],
                theme,
            )
            .debug_selector(|| format!("settings-row-{HIGH_CONTRAST}"))
            .into_any_element(),
        );

        // Text.
        out.push(controls::heading("Text", false, theme).into_any_element());
        for (label, description, family_key, size_key) in FONTS {
            let family = self
                .value(family_key, cx)
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let mut fonts: Vec<(String, String)> = self
                .fonts
                .iter()
                .map(|f| (f.to_string(), f.to_string()))
                .collect();
            if !fonts.iter().any(|(f, _)| *f == family) {
                fonts.insert(0, (family.clone(), family.clone()));
            }
            let select = self.select(family_key, &family, fonts, true, theme, cx);
            let size_field = self.text_field(
                size_key,
                Some(field::NUMBER_WIDTH),
                Some("px"),
                true,
                self.error(size_key, cx).is_some(),
                theme,
                window,
                cx,
            );
            let error = self
                .error(family_key, cx)
                .or_else(|| self.error(size_key, cx));
            out.push(
                controls::row(
                    super::eid("font", family_key),
                    label,
                    description.map(SharedString::from),
                    error.map(SharedString::from),
                    vec![control_pair(select, size_field)],
                    theme,
                )
                .debug_selector(move || format!("settings-row-{family_key}"))
                .into_any_element(),
            );
        }

        let readable = settings.readable_line_width;
        let chars = self.text_field(
            READABLE_CHARS,
            Some(field::NUMBER_WIDTH),
            Some("ch"),
            true,
            self.error(READABLE_CHARS, cx).is_some(),
            theme,
            window,
            cx,
        );
        let switch = controls::switch(
            "readable-switch",
            readable,
            true,
            self.set_on_click(READABLE, Value::Boolean(!readable), cx),
            theme,
        )
        .debug_selector(|| format!("settings-switch-{READABLE}"))
        .into_any_element();
        let error = self
            .error(READABLE_CHARS, cx)
            .or_else(|| self.error(READABLE, cx));
        out.push(
            controls::row(
                "readable",
                "Readable line width",
                Some("Keeps text in a centred column in preview and in the editor.".into()),
                error.map(SharedString::from),
                vec![control_pair(chars, switch)],
                theme,
            )
            .debug_selector(|| format!("settings-row-{READABLE}"))
            .into_any_element(),
        );

        let zoom = i64::from(settings.zoom_percent);
        let (min, max) = match gv_settings::spec(ZOOM).map(|s| s.kind) {
            Some(gv_settings::Kind::Integer { min, max }) => (min, max),
            _ => (zoom, zoom),
        };
        let minus = (zoom > min)
            .then(|| self.set_on_click(ZOOM, Value::Integer((zoom - ZOOM_STEP).max(min)), cx));
        let plus = (zoom < max)
            .then(|| self.set_on_click(ZOOM, Value::Integer((zoom + ZOOM_STEP).min(max)), cx));
        out.push(
            controls::row(
                "zoom",
                "Zoom",
                None,
                self.error(ZOOM, cx).map(SharedString::from),
                vec![
                    controls::stepper("settings-zoom", format!("{zoom}%"), minus, plus, theme)
                        .into_any_element(),
                ],
                theme,
            )
            .debug_selector(|| format!("settings-row-{ZOOM}"))
            .into_any_element(),
        );

        // Code blocks. No bundled code theme is listed yet: only "Match the
        // app theme" and the value in the file.
        out.push(controls::heading("Code blocks", false, theme).into_any_element());
        let code_theme = settings.code_theme.clone();
        let mut options = vec![("auto".to_owned(), "Match the app theme".to_owned())];
        if code_theme != "auto" {
            options.push((code_theme.clone(), code_theme.clone()));
        }
        let select = self.select(CODE_THEME, &code_theme, options, true, theme, cx);
        out.push(
            div()
                .border_b(px(HAIRLINE))
                .border_color(theme.border.default)
                .child(
                    controls::row(
                        "code-theme",
                        "Code theme",
                        Some("The same colours are used in the editor and in preview.".into()),
                        self.error(CODE_THEME, cx).map(SharedString::from),
                        vec![select],
                        theme,
                    )
                    .debug_selector(|| format!("settings-row-{CODE_THEME}")),
                )
                .into_any_element(),
        );
        out
    }

    /// One sentence on which theme is showing and why.
    fn showing_sentence(&self, choice: ThemeChoice, cx: &Context<'_, Self>) -> String {
        let shown = theme_name(Appearance::current_id(cx));
        let settings = cx.global::<AppSettings>();
        if settings.theme_override().is_some() {
            return format!(
                "The --theme option is showing {shown} until the theme is changed here."
            );
        }
        if settings.safe_mode() {
            return format!("Safe mode is showing {shown}.");
        }
        match choice {
            ThemeChoice::System => {
                let appearance = if Appearance::current_id(cx) == ThemeId::Dark {
                    "dark"
                } else {
                    "light"
                };
                format!(
                    "{} is in {appearance} appearance, so Glyphvane is showing {shown}.",
                    os_name()
                )
            }
            _ => format!("Glyphvane is showing {shown}."),
        }
    }

    /// The four theme cards: System, Light, Semi-light, Dark.
    fn theme_cards(
        &self,
        choice: ThemeChoice,
        preferred_light: crate::theme::PreferredLight,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let light_for_system =
            ThemeChoice::System.resolve(crate::theme::OsAppearance::Light, preferred_light);
        div()
            .flex()
            .flex_wrap()
            .gap(px(theme_card::GAP))
            .children(ThemeChoice::ALL.into_iter().map(|card| {
                let selected = card == choice;
                let (left, right) = match card {
                    ThemeChoice::System => (light_for_system, ThemeId::Dark),
                    ThemeChoice::Light => (ThemeId::Light, ThemeId::Light),
                    ThemeChoice::SemiLight => (ThemeId::SemiLight, ThemeId::SemiLight),
                    ThemeChoice::Dark => (ThemeId::Dark, ThemeId::Dark),
                };
                let value = choice_value(card);
                div()
                    .id(("theme-card", card as usize))
                    .debug_selector(move || format!("settings-theme-{value}"))
                    .flex_1()
                    .flex_basis(px(theme_card::BASIS))
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(theme_card::NAME_GAP))
                    .on_click(self.set_on_click(THEME, Value::String(value.to_owned()), cx))
                    .child(
                        div()
                            .h(px(THEME_CARD_PREVIEW))
                            .rounded(px(theme_card::PREVIEW_RADIUS))
                            .overflow_hidden()
                            .flex()
                            .map(|d| {
                                if selected {
                                    d.border(px(theme_card::RING_SELECTED))
                                        .border_color(theme.accent.default)
                                } else {
                                    d.border(px(HAIRLINE)).border_color(theme.border.strong)
                                }
                            })
                            .child(preview_half(left.theme(), true))
                            .child(preview_half(right.theme(), false)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(theme_card::NAME_GAP))
                            .when(selected, |d| {
                                d.font_weight(FontWeight(text::weight::STRONG))
                            })
                            .child(
                                div()
                                    .size(px(theme_card::DOT))
                                    .flex_none()
                                    .rounded_full()
                                    .map(|d| {
                                        if selected {
                                            d.border(px(theme_card::DOT_RING_SELECTED))
                                                .border_color(theme.accent.default)
                                        } else {
                                            d.border(px(HAIRLINE)).border_color(theme.border.strong)
                                        }
                                    }),
                            )
                            .child(choice_name(card)),
                    )
            }))
            .into_any_element()
    }
}

/// Two controls with the design's 8 between them.
fn control_pair(a: AnyElement, b: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(row::CONTROL_GAP))
        .child(a)
        .child(b)
        .into_any_element()
}

/// One half of a theme card's preview, in `theme`'s own colours: a title
/// bar, then (in the left half) a sidebar and a heading, two lines of text
/// and a link; the right half continues the lines.
fn preview_half(theme: &Theme, left: bool) -> Div {
    let line = |height: f32, width: Option<f32>, color: crate::theme::Color| {
        div()
            .h(px(height))
            .flex_none()
            .map(|d| match width {
                Some(w) => d.w(relative(w)),
                None => d.w_full(),
            })
            .bg(color)
            .map(|d| {
                // Full lines run on into the other half, so only their outer
                // ends are round.
                let r = px(theme_card::LINE_RADIUS);
                match (width, left) {
                    (Some(_), _) => d.rounded(r),
                    (None, true) => d.rounded_l(r),
                    (None, false) => d.rounded_r(r),
                }
            })
    };
    let lines = if left {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(theme_card::LINES_GAP))
            .pt(px(theme_card::LINES_PAD_TOP))
            .pl(px(theme_card::LINES_PAD_SIDE))
            .child(line(
                theme_card::HEADING_LINE,
                Some(theme_card::HEADING_FRACTION),
                theme.text.primary,
            ))
            .child(line(theme_card::TEXT_LINE, None, theme.border.strong))
            .child(line(theme_card::TEXT_LINE, None, theme.border.strong))
            .child(line(
                theme_card::TEXT_LINE,
                Some(theme_card::LINK_FRACTION),
                theme.accent.default,
            ))
    } else {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(theme_card::LINES_GAP))
            .pt(px(theme_card::LINES_PAD_TOP))
            .pr(px(theme_card::LINES_PAD_RIGHT))
            .child(div().h(px(theme_card::HEADING_LINE)))
            .child(line(theme_card::TEXT_LINE, None, theme.border.strong))
            .child(line(
                theme_card::TEXT_LINE,
                Some(theme_card::SHORT_FRACTION),
                theme.border.strong,
            ))
    };
    div()
        .flex_1()
        .flex()
        .flex_col()
        .bg(theme.surface.page)
        .child(
            div()
                .h(px(theme_card::TITLE_BAR))
                .flex_none()
                .bg(theme.surface.window),
        )
        .child(if left {
            div()
                .flex_1()
                .flex()
                .child(
                    div()
                        .w(px(theme_card::SIDEBAR))
                        .flex_none()
                        .bg(theme.surface.panel),
                )
                .child(lines)
        } else {
            lines
        })
}
