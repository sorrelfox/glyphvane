// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Text styles of the Reading view: colour tokens, the type ramp, style
//! ids and the link decorations of each link status.
//!
//! - A [`ColorToken`] is a theme token by role name; its
//!   [`ColorToken::role`] is the opaque [`ColorRole`] that `gv-inline`
//!   carries. The numbering is the UI palette's: role `n` is the `n`-th
//!   token of [`ColorToken::ALL`].
//! - A [`StyleId`] packs a [`TextRole`] (a step of the type ramp: body,
//!   headings, code, ...) and a [`Variant`] (italic, strong, monospace).
//!   The [`StyleTable`] describes every id: family, size, line height,
//!   weight, slant and tracking, from the design's numbers. The UI maps the
//!   family, weight and slant of each id to a font.
//! - [`link_decor`] gives the preview decoration of a [`LinkStatus`]:
//!   colour, underline style, strikethrough and what trails the link.

use gv_inline::{
    Background, ColorRole, LineHeight, ParagraphStyle, RunStyle, Strikethrough, StyleId, Underline,
    UnderlineStyle,
};

use crate::status::LinkStatus;

/// A colour token of the design, by role.
///
/// `TextPrimary` is role 0, the role `gv-inline` gives a run by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u32)]
pub enum ColorToken {
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
}

impl ColorToken {
    /// Every token, in role order: `ALL[i].role() == ColorRole(i)`.
    pub const ALL: [Self; 31] = [
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
        }
    }
}

impl From<ColorToken> for ColorRole {
    fn from(token: ColorToken) -> Self {
        token.role()
    }
}

/// The font family of a style, by role: the UI maps it to a face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    /// The interface and preview family (Inter in the design).
    Text,
    /// The monospace family (JetBrains Mono in the design).
    Mono,
}

/// A step of the preview's type ramp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum TextRole {
    /// Body text: 15/24.
    Body,
    /// Level-1 heading: 26/32, 700, -0.02em.
    H1,
    /// Level-2 heading: 18/24, 600, -0.01em.
    H2,
    /// Level-3 heading (not designed; between H2 and the body).
    H3,
    /// Level-4 heading (not designed).
    H4,
    /// Level-5 heading (not designed).
    H5,
    /// Level-6 heading (not designed).
    H6,
    /// Code blocks: mono 12.5/20.
    Code,
    /// The language or title in a code block's head: mono 11.
    CodeLabel,
    /// Front matter properties: 12.5/22.
    Properties,
    /// Counts next to inline status icons: 11.5, 600.
    Badge,
    /// Table header cells: 12, 600, in `text.secondary` (line height not
    /// designed: 16).
    TableHeader,
    /// Table body cells: 13.5/20.
    TableCell,
}

impl TextRole {
    /// Every role, in id order.
    pub const ALL: [Self; 13] = [
        Self::Body,
        Self::H1,
        Self::H2,
        Self::H3,
        Self::H4,
        Self::H5,
        Self::H6,
        Self::Code,
        Self::CodeLabel,
        Self::Properties,
        Self::Badge,
        Self::TableHeader,
        Self::TableCell,
    ];

    /// The role of a heading level (clamped to 1..=6).
    pub fn heading(level: u8) -> Self {
        match level {
            0 | 1 => Self::H1,
            2 => Self::H2,
            3 => Self::H3,
            4 => Self::H4,
            5 => Self::H5,
            _ => Self::H6,
        }
    }

    /// The role's name, for dumps and debugging.
    pub fn name(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::H1 => "h1",
            Self::H2 => "h2",
            Self::H3 => "h3",
            Self::H4 => "h4",
            Self::H5 => "h5",
            Self::H6 => "h6",
            Self::Code => "code",
            Self::CodeLabel => "code-label",
            Self::Properties => "properties",
            Self::Badge => "badge",
            Self::TableHeader => "table-header",
            Self::TableCell => "table-cell",
        }
    }

    /// The colour of plain text in this role.
    pub fn color(self) -> ColorToken {
        match self {
            Self::CodeLabel | Self::TableHeader => ColorToken::TextSecondary,
            Self::Badge => ColorToken::StatusWarning,
            _ => ColorToken::TextPrimary,
        }
    }
}

/// Inline variations of a role: italic, strong and monospace (inline
/// code), as bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Variant(pub u8);

impl Variant {
    /// No variation.
    pub const PLAIN: Variant = Variant(0);
    /// Emphasis.
    pub const ITALIC: Variant = Variant(1);
    /// Strong emphasis.
    pub const STRONG: Variant = Variant(2);
    /// Inline code.
    pub const MONO: Variant = Variant(4);
    /// All bits.
    const ALL_BITS: u8 = 7;

    /// Whether every bit of `other` is set.
    pub const fn contains(self, other: Variant) -> bool {
        self.0 & other.0 == other.0
    }

    /// The union of both.
    #[must_use]
    pub const fn with(self, other: Variant) -> Variant {
        Variant(self.0 | other.0)
    }
}

/// Bits of a [`StyleId`] that hold the variant.
const VARIANT_BITS: u32 = 3;

/// The style id of `role` with `variant`.
pub fn style_id(role: TextRole, variant: Variant) -> StyleId {
    StyleId(((role as u32) << VARIANT_BITS) | u32::from(variant.0 & Variant::ALL_BITS))
}

/// The role and variant of a style id made by [`style_id`].
pub fn decode_style_id(id: StyleId) -> Option<(TextRole, Variant)> {
    let role = TextRole::ALL.get(usize::try_from(id.0 >> VARIANT_BITS).ok()?)?;
    let bits = u8::try_from(id.0 & ((1 << VARIANT_BITS) - 1)).ok()?;
    Some((*role, Variant(bits)))
}

/// Everything about a style the UI needs to pick a font and the layout
/// needs to size it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyleSpec {
    pub family: Family,
    /// Font size in px.
    pub size: f32,
    /// Line height in px.
    pub line_height: f32,
    /// CSS weight (400 regular, 600 semibold, 700 bold).
    pub weight: u16,
    pub italic: bool,
    /// Letter spacing in em (CSS `letter-spacing`).
    pub tracking_em: f32,
}

impl StyleSpec {
    const fn new(family: Family, size: f32, line_height: f32, weight: u16) -> Self {
        Self {
            family,
            size,
            line_height,
            weight,
            italic: false,
            tracking_em: 0.0,
        }
    }

    const fn tracking(mut self, em: f32) -> Self {
        self.tracking_em = em;
        self
    }

    /// Letter spacing in px.
    pub fn letter_spacing(&self) -> f32 {
        self.tracking_em * self.size
    }
}

/// Inline code size relative to the text around it (13 in a 15 px body).
const INLINE_CODE_RATIO: f32 = 13.0 / 15.0;

/// The styles of the preview: one [`StyleSpec`] per role, the inline-code
/// chip and the zoom factor. Built from the design's numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct StyleTable {
    roles: [StyleSpec; TextRole::ALL.len()],
    zoom: f32,
}

impl Default for StyleTable {
    fn default() -> Self {
        Self::preview()
    }
}

impl StyleTable {
    /// The Preview mode's type: body 15/24, H1 26/32 700 -0.02em, H2 18/24
    /// 600 -0.01em, code 12.5/20, properties 12.5/22, table header cells
    /// 12/16 600, table body cells 13.5/20. H3 to H6 are not designed; they
    /// step down from H2 to the body size.
    pub fn preview() -> Self {
        use Family::{Mono, Text};
        Self {
            roles: [
                StyleSpec::new(Text, 15.0, 24.0, 400),
                StyleSpec::new(Text, 26.0, 32.0, 700).tracking(-0.02),
                StyleSpec::new(Text, 18.0, 24.0, 600).tracking(-0.01),
                StyleSpec::new(Text, 16.0, 24.0, 600),
                StyleSpec::new(Text, 15.0, 24.0, 600),
                StyleSpec::new(Text, 14.0, 22.0, 600),
                StyleSpec::new(Text, 13.5, 20.0, 600),
                StyleSpec::new(Mono, 12.5, 20.0, 400),
                StyleSpec::new(Mono, 11.0, 16.0, 400),
                StyleSpec::new(Text, 12.5, 22.0, 400),
                StyleSpec::new(Text, 11.5, 16.0, 600),
                StyleSpec::new(Text, 12.0, 16.0, 600),
                StyleSpec::new(Text, 13.5, 20.0, 400),
            ],
            zoom: 1.0,
        }
    }

    /// The preview pane of Split mode, one step smaller: body 14.5/23, H1
    /// 24/30; the other headings step down in proportion.
    pub fn split() -> Self {
        let mut t = Self::preview();
        let r = &mut t.roles;
        r[TextRole::Body as usize].size = 14.5;
        r[TextRole::Body as usize].line_height = 23.0;
        r[TextRole::H1 as usize].size = 24.0;
        r[TextRole::H1 as usize].line_height = 30.0;
        r[TextRole::H2 as usize].size = 17.0;
        r[TextRole::H3 as usize].size = 15.5;
        r[TextRole::H4 as usize].size = 14.5;
        r[TextRole::H4 as usize].line_height = 23.0;
        t
    }

    /// The same table with every length multiplied by `zoom` (view zoom).
    #[must_use]
    pub fn with_zoom(mut self, zoom: f32) -> Self {
        self.zoom = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            1.0
        };
        self
    }

    /// The zoom factor.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// The spec of a role, zoomed, without variation.
    pub fn role(&self, role: TextRole) -> StyleSpec {
        let mut s = self.roles[role as usize];
        s.size *= self.zoom;
        s.line_height *= self.zoom;
        s
    }

    /// The spec of `role` with `variant`: italic slants; strong is at
    /// least 700; monospace switches to the mono family at the inline-code
    /// size (13 in a 15 px body) and drops the tracking.
    pub fn variant(&self, role: TextRole, variant: Variant) -> StyleSpec {
        let mut s = self.role(role);
        if variant.contains(Variant::ITALIC) {
            s.italic = true;
        }
        if variant.contains(Variant::STRONG) {
            s.weight = s.weight.max(700);
        }
        if variant.contains(Variant::MONO) && s.family != Family::Mono {
            s.family = Family::Mono;
            s.size = (s.size * INLINE_CODE_RATIO * 2.0).round() / 2.0;
            s.tracking_em = 0.0;
        }
        s
    }

    /// The spec of a style id; `None` for an id [`style_id`] does not make.
    pub fn spec(&self, id: StyleId) -> Option<StyleSpec> {
        decode_style_id(id).map(|(role, variant)| self.variant(role, variant))
    }

    /// Every style id with its spec: what the UI's font table needs.
    pub fn ids(&self) -> impl Iterator<Item = (StyleId, StyleSpec)> + '_ {
        TextRole::ALL.into_iter().flat_map(move |role| {
            (0..=Variant::ALL_BITS).map(move |bits| {
                let v = Variant(bits);
                (style_id(role, v), self.variant(role, v))
            })
        })
    }

    /// The paragraph style (strut) of a role.
    pub fn paragraph_style(&self, role: TextRole) -> ParagraphStyle {
        let s = self.role(role);
        ParagraphStyle::new(
            style_id(role, Variant::PLAIN),
            s.size,
            LineHeight::Px(s.line_height),
        )
    }

    /// The run style of `role` with `variant`: font, size, tracking and the
    /// role's text colour; inline code gets its chip (mono 13, padding 1 5,
    /// radius 4, `surface.sunken`).
    pub fn run_style(&self, role: TextRole, variant: Variant) -> RunStyle {
        let s = self.variant(role, variant);
        let mut run = RunStyle::new(style_id(role, variant), s.size)
            .with_color(role.color().role())
            .with_letter_spacing(s.letter_spacing());
        if variant.contains(Variant::MONO) && role != TextRole::Code {
            run = run.with_background(Background {
                role: ColorToken::SurfaceSunken.role(),
                padding_x: 5.0 * self.zoom,
                padding_y: 1.0 * self.zoom,
                radius: 4.0 * self.zoom,
            });
        }
        run
    }

    /// `run` decorated as a link with `status` (see [`link_decor`]).
    pub fn link_style(&self, run: RunStyle, status: LinkStatus) -> RunStyle {
        let d = link_decor(status);
        let mut run = run.with_color(d.color.role());
        if let Some((style, color)) = d.underline {
            run = run.with_underline(Underline::new(
                style,
                color.role(),
                3.0 * self.zoom,
                self.zoom,
            ));
        }
        if d.strike {
            run = run.with_strikethrough(Strikethrough {
                color: d.color.role(),
                thickness: self.zoom,
            });
        }
        run
    }
}

/// An icon drawn after a link to show its status (Lucide ids).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusIcon {
    /// `triangle-alert`: ambiguous.
    TriangleAlert,
    /// `file-plus` with a dashed outline: unresolved.
    FilePlusDashed,
    /// `log-out`: outside the root.
    LogOut,
    /// `arrow-up-right`: external.
    ArrowUpRight,
    /// `lock`: blocked.
    Lock,
}

impl StatusIcon {
    /// The Lucide id.
    pub fn lucide_id(self) -> &'static str {
        match self {
            Self::TriangleAlert => "triangle-alert",
            Self::FilePlusDashed => "file-plus",
            Self::LogOut => "log-out",
            Self::ArrowUpRight => "arrow-up-right",
            Self::Lock => "lock",
        }
    }

    /// Whether the outline is drawn dashed (3 on, 2.6 off).
    pub fn dashed(self) -> bool {
        self == Self::FilePlusDashed
    }

    /// Size in px at zoom 1: 12 for external links, 13 for the others.
    pub fn size(self) -> f32 {
        match self {
            Self::ArrowUpRight => 12.0,
            _ => 13.0,
        }
    }
}

/// What trails a link in the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkSuffix {
    /// `≈` in `text.secondary` (case-only and loose matches).
    Approx,
    /// An icon, with the candidate count for ambiguous links.
    Icon {
        icon: StatusIcon,
        color: ColorToken,
        count: Option<u32>,
    },
}

/// The preview decoration of a link status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkDecor {
    /// Text colour.
    pub color: ColorToken,
    /// Underline style and colour.
    pub underline: Option<(UnderlineStyle, ColorToken)>,
    /// Struck through (blocked links).
    pub strike: bool,
    pub suffix: Option<LinkSuffix>,
}

/// The preview decoration of `status`, as the design's link status table
/// gives it: resolved links are `accent.default` with a solid 1 px
/// `accent.line` underline 3 px below the baseline; case-only and loose
/// matches add `≈`; ambiguous ones a warning icon and the candidate count;
/// unresolved links are dashed with a dashed `file-plus`; outside-root and
/// external links add `log-out` and `arrow-up-right`; blocked links are
/// `text.secondary`, struck through, with a lock.
pub fn link_decor(status: LinkStatus) -> LinkDecor {
    let solid = Some((UnderlineStyle::Solid, ColorToken::AccentLine));
    let resolved = LinkDecor {
        color: ColorToken::AccentDefault,
        underline: solid,
        strike: false,
        suffix: None,
    };
    let icon = |icon, color, count| Some(LinkSuffix::Icon { icon, color, count });
    match status {
        LinkStatus::Exact | LinkStatus::Pending => resolved,
        LinkStatus::CaseFold | LinkStatus::Loose => LinkDecor {
            suffix: Some(LinkSuffix::Approx),
            ..resolved
        },
        LinkStatus::Ambiguous(n) => LinkDecor {
            suffix: icon(
                StatusIcon::TriangleAlert,
                ColorToken::StatusWarning,
                Some(n),
            ),
            ..resolved
        },
        LinkStatus::Unresolved => LinkDecor {
            underline: Some((UnderlineStyle::Dashed, ColorToken::AccentDefault)),
            suffix: icon(StatusIcon::FilePlusDashed, ColorToken::TextSecondary, None),
            ..resolved
        },
        LinkStatus::OutsideRoot => LinkDecor {
            suffix: icon(StatusIcon::LogOut, ColorToken::TextSecondary, None),
            ..resolved
        },
        LinkStatus::External => LinkDecor {
            suffix: icon(StatusIcon::ArrowUpRight, ColorToken::AccentDefault, None),
            ..resolved
        },
        LinkStatus::Blocked => LinkDecor {
            color: ColorToken::TextSecondary,
            underline: None,
            strike: true,
            suffix: icon(StatusIcon::Lock, ColorToken::TextSecondary, None),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_numbered_in_order() {
        for (i, token) in ColorToken::ALL.iter().enumerate() {
            assert_eq!(token.role(), ColorRole(i as u32));
            assert_eq!(ColorToken::from_role(token.role()), Some(*token));
        }
        assert_eq!(ColorToken::TextPrimary.role(), ColorRole::default());
        assert_eq!(ColorToken::from_role(ColorRole(31)), None);
    }

    #[test]
    fn style_ids_round_trip() {
        let table = StyleTable::preview();
        let mut seen = std::collections::HashSet::new();
        for (id, spec) in table.ids() {
            assert!(seen.insert(id), "duplicate {id:?}");
            let (role, variant) = decode_style_id(id).expect("decodes");
            assert_eq!(style_id(role, variant), id);
            assert_eq!(table.spec(id), Some(spec));
        }
        assert_eq!(seen.len(), TextRole::ALL.len() * 8);
        assert_eq!(table.spec(StyleId(u32::MAX)), None);
    }

    #[test]
    fn design_numbers() {
        let t = StyleTable::preview();
        let body = t.role(TextRole::Body);
        assert_eq!(
            (body.size, body.line_height, body.weight),
            (15.0, 24.0, 400)
        );
        let h1 = t.role(TextRole::H1);
        assert_eq!((h1.size, h1.line_height, h1.weight), (26.0, 32.0, 700));
        assert!((h1.letter_spacing() + 0.52).abs() < 1e-6);
        let h2 = t.role(TextRole::H2);
        assert_eq!((h2.size, h2.line_height, h2.weight), (18.0, 24.0, 600));
        let code = t.variant(TextRole::Body, Variant::MONO);
        assert_eq!((code.family, code.size), (Family::Mono, 13.0));
        let strong = t.variant(TextRole::H2, Variant::STRONG.with(Variant::ITALIC));
        assert_eq!((strong.weight, strong.italic), (700, true));
        let zoomed = StyleTable::preview().with_zoom(2.0).role(TextRole::Body);
        assert_eq!((zoomed.size, zoomed.line_height), (30.0, 48.0));
        let split = StyleTable::split();
        assert_eq!(split.role(TextRole::Body).line_height, 23.0);
        assert_eq!(split.role(TextRole::H1).size, 24.0);
    }

    #[test]
    fn link_statuses_follow_the_design() {
        let exact = link_decor(LinkStatus::Exact);
        assert_eq!(exact.color, ColorToken::AccentDefault);
        assert_eq!(
            exact.underline,
            Some((UnderlineStyle::Solid, ColorToken::AccentLine))
        );
        assert_eq!(link_decor(LinkStatus::Pending), exact);
        assert_eq!(
            link_decor(LinkStatus::Loose).suffix,
            Some(LinkSuffix::Approx)
        );
        let unresolved = link_decor(LinkStatus::Unresolved);
        assert_eq!(
            unresolved.underline.map(|u| u.0),
            Some(UnderlineStyle::Dashed)
        );
        let blocked = link_decor(LinkStatus::Blocked);
        assert!(blocked.strike && blocked.underline.is_none());
        assert_eq!(blocked.color, ColorToken::TextSecondary);
        assert_eq!(
            link_decor(LinkStatus::Ambiguous(3)).suffix,
            Some(LinkSuffix::Icon {
                icon: StatusIcon::TriangleAlert,
                color: ColorToken::StatusWarning,
                count: Some(3)
            })
        );
    }
}
