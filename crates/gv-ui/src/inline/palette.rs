// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Colour roles of inline content and their theme colours.
//!
//! `gv-inline` keeps colours opaque: a run, a decoration or a background
//! names a [`ColorRole`]. In Glyphvane a role is a theme token: the role's
//! number is the index of a [`Token`]. A [`Palette`] holds the GPUI colour of
//! every token for one theme, so painting a role is an array lookup.

use std::sync::{Arc, OnceLock};

use gpui::Hsla;
use gv_inline::ColorRole;

use crate::theme::{Color, Theme, ThemeId};

/// The colour tokens of a theme, as colour roles for inline content.
///
/// `TextPrimary` is role 0, the role `gv-inline` gives a run by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Token {
    TextPrimary,
    TextSecondary,
    TextTertiary,
    AccentDefault,
    AccentTint,
    AccentLine,
    AccentOn,
    SurfaceWindow,
    SurfacePanel,
    SurfacePage,
    SurfaceRaised,
    SurfaceSunken,
    StateHover,
    StatePress,
    BorderDefault,
    BorderStrong,
    StatusError,
    StatusErrorTint,
    StatusWarning,
    StatusWarningTint,
    StatusSuccess,
    StatusSuccessTint,
    StatusInfo,
    StatusInfoTint,
    Selection,
    SyntaxKey,
    SyntaxString,
    SyntaxNumber,
    SyntaxTag,
    SyntaxUrl,
    SyntaxCode,
    DiagramCategory1,
    DiagramCategory2,
    DiagramCategory3,
    DiagramCategory4,
    DiagramCategory5,
    DiagramCategory6,
    DiagramCategory7,
    DiagramCategory8,
    DiagramCategory9,
    DiagramCategory10,
    DiagramCategory11,
    DiagramCategory12,
}

impl Token {
    /// Every token, in role order: `ALL[i].role() == ColorRole(i)`.
    pub const ALL: [Self; 43] = [
        Self::TextPrimary,
        Self::TextSecondary,
        Self::TextTertiary,
        Self::AccentDefault,
        Self::AccentTint,
        Self::AccentLine,
        Self::AccentOn,
        Self::SurfaceWindow,
        Self::SurfacePanel,
        Self::SurfacePage,
        Self::SurfaceRaised,
        Self::SurfaceSunken,
        Self::StateHover,
        Self::StatePress,
        Self::BorderDefault,
        Self::BorderStrong,
        Self::StatusError,
        Self::StatusErrorTint,
        Self::StatusWarning,
        Self::StatusWarningTint,
        Self::StatusSuccess,
        Self::StatusSuccessTint,
        Self::StatusInfo,
        Self::StatusInfoTint,
        Self::Selection,
        Self::SyntaxKey,
        Self::SyntaxString,
        Self::SyntaxNumber,
        Self::SyntaxTag,
        Self::SyntaxUrl,
        Self::SyntaxCode,
        Self::DiagramCategory1,
        Self::DiagramCategory2,
        Self::DiagramCategory3,
        Self::DiagramCategory4,
        Self::DiagramCategory5,
        Self::DiagramCategory6,
        Self::DiagramCategory7,
        Self::DiagramCategory8,
        Self::DiagramCategory9,
        Self::DiagramCategory10,
        Self::DiagramCategory11,
        Self::DiagramCategory12,
    ];

    /// The categorical diagram palette, slot 1 first.
    pub const DIAGRAM_CATEGORIES: [Self; 12] = [
        Self::DiagramCategory1,
        Self::DiagramCategory2,
        Self::DiagramCategory3,
        Self::DiagramCategory4,
        Self::DiagramCategory5,
        Self::DiagramCategory6,
        Self::DiagramCategory7,
        Self::DiagramCategory8,
        Self::DiagramCategory9,
        Self::DiagramCategory10,
        Self::DiagramCategory11,
        Self::DiagramCategory12,
    ];

    /// The colour role that stands for this token.
    pub const fn role(self) -> ColorRole {
        ColorRole(self as u32)
    }

    /// The token a colour role stands for; `None` for a role that is not a
    /// token.
    pub fn from_role(role: ColorRole) -> Option<Self> {
        usize::try_from(role.0)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
    }

    /// The token's name in the design tokens (`text.primary`, ...).
    pub fn name(self) -> &'static str {
        match self {
            Self::TextPrimary => "text.primary",
            Self::TextSecondary => "text.secondary",
            Self::TextTertiary => "text.tertiary",
            Self::AccentDefault => "accent.default",
            Self::AccentTint => "accent.tint",
            Self::AccentLine => "accent.line",
            Self::AccentOn => "accent.on",
            Self::SurfaceWindow => "surface.window",
            Self::SurfacePanel => "surface.panel",
            Self::SurfacePage => "surface.page",
            Self::SurfaceRaised => "surface.raised",
            Self::SurfaceSunken => "surface.sunken",
            Self::StateHover => "state.hover",
            Self::StatePress => "state.press",
            Self::BorderDefault => "border.default",
            Self::BorderStrong => "border.strong",
            Self::StatusError => "status.error",
            Self::StatusErrorTint => "status.error_tint",
            Self::StatusWarning => "status.warning",
            Self::StatusWarningTint => "status.warning_tint",
            Self::StatusSuccess => "status.success",
            Self::StatusSuccessTint => "status.success_tint",
            Self::StatusInfo => "status.info",
            Self::StatusInfoTint => "status.info_tint",
            Self::Selection => "selection.text",
            Self::SyntaxKey => "syntax.key",
            Self::SyntaxString => "syntax.string",
            Self::SyntaxNumber => "syntax.number",
            Self::SyntaxTag => "syntax.tag",
            Self::SyntaxUrl => "syntax.url",
            Self::SyntaxCode => "syntax.code",
            Self::DiagramCategory1 => "diagram.category.1",
            Self::DiagramCategory2 => "diagram.category.2",
            Self::DiagramCategory3 => "diagram.category.3",
            Self::DiagramCategory4 => "diagram.category.4",
            Self::DiagramCategory5 => "diagram.category.5",
            Self::DiagramCategory6 => "diagram.category.6",
            Self::DiagramCategory7 => "diagram.category.7",
            Self::DiagramCategory8 => "diagram.category.8",
            Self::DiagramCategory9 => "diagram.category.9",
            Self::DiagramCategory10 => "diagram.category.10",
            Self::DiagramCategory11 => "diagram.category.11",
            Self::DiagramCategory12 => "diagram.category.12",
        }
    }

    /// The token's colour in `theme`.
    pub fn color(self, theme: &Theme) -> Color {
        match self {
            Self::TextPrimary => theme.text.primary,
            Self::TextSecondary => theme.text.secondary,
            Self::TextTertiary => theme.text.tertiary,
            Self::AccentDefault => theme.accent.default,
            Self::AccentTint => theme.accent.tint,
            Self::AccentLine => theme.accent.line,
            Self::AccentOn => theme.accent.on,
            Self::SurfaceWindow => theme.surface.window,
            Self::SurfacePanel => theme.surface.panel,
            Self::SurfacePage => theme.surface.page,
            Self::SurfaceRaised => theme.surface.raised,
            Self::SurfaceSunken => theme.surface.sunken,
            Self::StateHover => theme.state.hover,
            Self::StatePress => theme.state.press,
            Self::BorderDefault => theme.border.default,
            Self::BorderStrong => theme.border.strong,
            Self::StatusError => theme.status.error,
            Self::StatusErrorTint => theme.status.error_tint,
            Self::StatusWarning => theme.status.warning,
            Self::StatusWarningTint => theme.status.warning_tint,
            Self::StatusSuccess => theme.status.success,
            Self::StatusSuccessTint => theme.status.success_tint,
            Self::StatusInfo => theme.status.info,
            Self::StatusInfoTint => theme.status.info_tint,
            Self::Selection => theme.selection.text,
            Self::SyntaxKey => theme.syntax.key,
            Self::SyntaxString => theme.syntax.string,
            Self::SyntaxNumber => theme.syntax.number,
            Self::SyntaxTag => theme.syntax.tag,
            Self::SyntaxUrl => theme.syntax.url,
            Self::SyntaxCode => theme.syntax.code,
            Self::DiagramCategory1 => theme.diagram.category[0],
            Self::DiagramCategory2 => theme.diagram.category[1],
            Self::DiagramCategory3 => theme.diagram.category[2],
            Self::DiagramCategory4 => theme.diagram.category[3],
            Self::DiagramCategory5 => theme.diagram.category[4],
            Self::DiagramCategory6 => theme.diagram.category[5],
            Self::DiagramCategory7 => theme.diagram.category[6],
            Self::DiagramCategory8 => theme.diagram.category[7],
            Self::DiagramCategory9 => theme.diagram.category[8],
            Self::DiagramCategory10 => theme.diagram.category[9],
            Self::DiagramCategory11 => theme.diagram.category[10],
            Self::DiagramCategory12 => theme.diagram.category[11],
        }
    }
}

impl From<Token> for ColorRole {
    fn from(token: Token) -> Self {
        token.role()
    }
}

/// The GPUI colour of every [`Token`] in one theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    colors: [Hsla; Token::ALL.len()],
}

impl Palette {
    pub fn new(theme: &Theme) -> Self {
        Self {
            colors: Token::ALL.map(|token| token.color(theme).into()),
        }
    }

    /// The palette of a built-in theme, made once.
    pub fn of(id: ThemeId) -> Arc<Self> {
        static PALETTES: OnceLock<[Arc<Palette>; 3]> = OnceLock::new();
        let palettes =
            PALETTES.get_or_init(|| ThemeId::ALL.map(|id| Arc::new(Palette::new(id.theme()))));
        palettes[id as usize].clone()
    }

    pub fn color(&self, token: Token) -> Hsla {
        self.colors[token as usize]
    }

    /// The colour of a role. A role that is not a token paints as
    /// `text.primary`, so content stays readable.
    pub fn role(&self, role: ColorRole) -> Hsla {
        self.color(Token::from_role(role).unwrap_or(Token::TextPrimary))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip_and_follow_the_order_of_all() {
        for (i, token) in Token::ALL.into_iter().enumerate() {
            assert_eq!(token.role(), ColorRole(i as u32));
            assert_eq!(Token::from_role(token.role()), Some(token));
        }
        assert_eq!(Token::TextPrimary.role(), ColorRole::default());
        assert_eq!(Token::from_role(ColorRole(Token::ALL.len() as u32)), None);
        assert_eq!(Token::from_role(ColorRole(u32::MAX)), None);
    }

    #[test]
    fn names_are_unique_token_paths() {
        let mut names: Vec<_> = Token::ALL.iter().map(|t| t.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Token::ALL.len());
        assert!(names.iter().all(|n| n.contains('.')));
    }

    #[test]
    fn palettes_map_roles_to_theme_colours() {
        for id in ThemeId::ALL {
            let theme = id.theme();
            let palette = Palette::of(id);
            assert_eq!(
                palette.role(Token::Selection.role()),
                Hsla::from(theme.selection.text)
            );
            assert_eq!(
                palette.color(Token::AccentLine),
                Hsla::from(theme.accent.line)
            );
            assert_eq!(
                palette.role(ColorRole(9999)),
                Hsla::from(theme.text.primary),
                "unknown roles paint as body text"
            );
        }
        assert_ne!(
            Palette::of(ThemeId::Light).color(Token::TextPrimary),
            Palette::of(ThemeId::Dark).color(Token::TextPrimary)
        );
    }
}
