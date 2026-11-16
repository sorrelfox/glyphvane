// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Theme-agnostic colour roles.
//!
//! merman is configured with one fixed set of *marker* colours, one per
//! [`Role`], instead of real theme colours. After rendering, every colour in
//! the output that matches a marker becomes that role, so one cached diagram
//! paints in every theme: the host resolves roles to its theme tokens at
//! paint time. Colours that match no marker (author `classDef` and `style`
//! colours, and the few that merman derives itself) stay literal.

use serde_json::{Map, Value, json};

/// Number of categorical palette slots ([`Role::Category`]).
pub const CATEGORY_COUNT: u8 = 12;

/// What a colour in a diagram stands for. The host maps each role to a theme
/// token; the doc comment of each variant names the suggested token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// The diagram ground and boxes that must look like it (edge label
    /// backgrounds, alternate gantt sections). `surface.page`.
    Background,
    /// Node fills (flowchart nodes, actors, classes, states, tasks).
    /// `accent.tint`.
    NodeFill,
    /// Node borders. `accent.default`.
    NodeBorder,
    /// Secondary fills (activations, done tasks, excluded days).
    /// `surface.sunken`.
    SecondaryFill,
    /// Containers: clusters, subgraphs, composite states, gantt sections.
    /// `surface.panel`.
    TertiaryFill,
    /// Container and secondary borders. `border.strong`.
    Border,
    /// Grid lines and axes of charts. `border.default`.
    Grid,
    /// All text. `text.primary`.
    Text,
    /// Edges, arrows, lifelines and signals. `text.secondary`.
    Line,
    /// Note fills. `status.warning_tint`.
    NoteFill,
    /// Note borders. `status.warning`.
    NoteBorder,
    /// Critical tasks and the gantt "today" line. `status.error`.
    Critical,
    /// Critical task fills. `status.error_tint`.
    CriticalFill,
    /// Categorical palette slot `0..CATEGORY_COUNT` (pie slices, chart
    /// series, git branches, mindmap and timeline sections, journey
    /// actors). New tokens `diagram.category.1` to `.12`.
    Category(u8),
}

impl Role {
    /// Every role, categories included, in a fixed order.
    pub fn all() -> impl Iterator<Item = Role> {
        const BASE: [Role; 13] = [
            Role::Background,
            Role::NodeFill,
            Role::NodeBorder,
            Role::SecondaryFill,
            Role::TertiaryFill,
            Role::Border,
            Role::Grid,
            Role::Text,
            Role::Line,
            Role::NoteFill,
            Role::NoteBorder,
            Role::Critical,
            Role::CriticalFill,
        ];
        BASE.into_iter()
            .chain((0..CATEGORY_COUNT).map(Role::Category))
    }

    /// The marker colour merman is configured with for this role.
    ///
    /// The markers are light-scheme colours (so merman's own light/dark
    /// decisions, such as label contrast, come out as for a light theme)
    /// and are at least 8 apart in some channel, so a marker that merman
    /// rounds through HSL still matches.
    pub(crate) fn marker(self) -> [u8; 3] {
        match self {
            Role::Background => [0xFD, 0xFD, 0xF6],
            Role::NodeFill => [0xCC, 0xEE, 0xE7],
            Role::NodeBorder => [0x00, 0x73, 0x70],
            Role::SecondaryFill => [0xEF, 0xF2, 0xE4],
            Role::TertiaryFill => [0xF3, 0xF5, 0xEF],
            Role::Border => [0xC0, 0xC5, 0xB8],
            Role::Grid => [0xD9, 0xDD, 0xCF],
            Role::Text => [0x19, 0x1F, 0x1B],
            Role::Line => [0x4A, 0x51, 0x4E],
            Role::NoteFill => [0xFD, 0xEF, 0xC7],
            Role::NoteBorder => [0x8F, 0x53, 0x08],
            Role::Critical => [0xB5, 0x1F, 0x17],
            Role::CriticalFill => [0xFF, 0xE7, 0xDC],
            Role::Category(i) => CATEGORY_MARKERS[usize::from(i % CATEGORY_COUNT)],
        }
    }

    /// The role whose marker `rgb` is, within merman's rounding.
    pub(crate) fn from_marker(rgb: [u8; 3]) -> Option<Role> {
        Role::all().find(|r| {
            let m = r.marker();
            (0..3).all(|i| m[i].abs_diff(rgb[i]) <= MARKER_TOLERANCE)
        })
    }

    /// The category whose marker has the hue of `rgb`: merman lightens or
    /// darkens category colours for some families (kanban sections,
    /// timeline peers), which keeps the hue.
    pub(crate) fn category_by_hue(rgb: [u8; 3]) -> Option<Role> {
        let (h, s, _) = hsl(rgb);
        if s < 0.15 {
            return None;
        }
        (0..CATEGORY_COUNT).map(Role::Category).find(|r| {
            let (mh, _, _) = hsl(r.marker());
            let d = (h - mh).abs();
            d.min(360.0 - d) <= 1.0
        })
    }
}

/// Hue in degrees, saturation and lightness in `0..=1`.
fn hsl([r, g, b]: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if (max - r).abs() <= f32::EPSILON {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if (max - g).abs() <= f32::EPSILON {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s, l)
}

/// Colours merman writes as fixed presentation attributes (`fill="white"`,
/// `stroke="#666"`), not from the theme: the role each stands for. Author
/// colours never come out as presentation attributes (they are CSS).
pub(crate) fn hardcoded(value: &str, stroke: bool) -> Option<Role> {
    match value.to_ascii_lowercase().as_str() {
        "white" | "#fff" | "#ffffff" => Some(Role::Background),
        "black" | "#000" | "#000000" | "#333" | "#333333" => {
            Some(if stroke { Role::Line } else { Role::Text })
        }
        "#666" | "#666666" | "#999" | "#999999" | "grey" | "gray" | "lightgrey" | "lightgray" => {
            Some(Role::Line)
        }
        // Journey's actor colours (merman ignores `journey.actorColours`).
        "#8fbc8f" => Some(Role::Category(0)),
        "#7cfc00" => Some(Role::Category(1)),
        "#00ffff" => Some(Role::Category(2)),
        "#20b2aa" => Some(Role::Category(3)),
        "#b0e0e6" => Some(Role::Category(4)),
        "#ffffe0" => Some(Role::Category(5)),
        _ => None,
    }
}

/// The marker colour of `role` as `#rrggbb`.
pub(crate) fn marker_hex(role: Role) -> String {
    hex(role)
}

/// Largest per-channel difference still taken as a marker (merman converts
/// some colours through HSL with rounding).
const MARKER_TOLERANCE: u8 = 2;

/// Categorical markers: mid-light hues, distinct from each other and from
/// the base roles.
const CATEGORY_MARKERS: [[u8; 3]; CATEGORY_COUNT as usize] = [
    [0x7A, 0xA6, 0xDA],
    [0xE6, 0xA1, 0x57],
    [0x7B, 0xC0, 0x8A],
    [0xE0, 0x7B, 0x7B],
    [0xA9, 0x8B, 0xD4],
    [0xC9, 0xA2, 0x7A],
    [0xE5, 0x9B, 0xC8],
    [0x9D, 0xA3, 0xA8],
    [0xC7, 0xC2, 0x5A],
    [0x6C, 0xC2, 0xC5],
    [0xF0, 0xC7, 0x5E],
    [0x8F, 0xB0, 0xE8],
];

fn hex(role: Role) -> String {
    let [r, g, b] = role.marker();
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Mermaid `themeVariables` that name a colour, with the role each gets.
/// Everything merman would otherwise derive is set explicitly, so derived
/// colours (which match no marker) stay rare.
const VARIABLES: &[(&str, Role)] = &[
    ("background", Role::Background),
    ("primaryColor", Role::NodeFill),
    ("primaryBorderColor", Role::NodeBorder),
    ("primaryTextColor", Role::Text),
    ("secondaryColor", Role::SecondaryFill),
    ("secondaryBorderColor", Role::Border),
    ("secondaryTextColor", Role::Text),
    ("tertiaryColor", Role::TertiaryFill),
    ("tertiaryBorderColor", Role::Border),
    ("tertiaryTextColor", Role::Text),
    ("lineColor", Role::Line),
    ("arrowheadColor", Role::Line),
    ("textColor", Role::Text),
    ("mainBkg", Role::NodeFill),
    ("nodeBkg", Role::NodeFill),
    ("nodeBorder", Role::NodeBorder),
    ("nodeTextColor", Role::Text),
    ("border1", Role::NodeBorder),
    ("border2", Role::Border),
    ("clusterBkg", Role::TertiaryFill),
    ("clusterBorder", Role::Border),
    ("defaultLinkColor", Role::Line),
    ("titleColor", Role::Text),
    ("edgeLabelBackground", Role::Background),
    ("flowContainerStroke", Role::Border),
    ("noteBkgColor", Role::NoteFill),
    ("noteBorderColor", Role::NoteBorder),
    ("noteTextColor", Role::Text),
    // Sequence.
    ("actorBkg", Role::NodeFill),
    ("actorBorder", Role::NodeBorder),
    ("actorTextColor", Role::Text),
    ("actorLineColor", Role::Line),
    ("signalColor", Role::Line),
    ("signalTextColor", Role::Text),
    ("labelBoxBkgColor", Role::NodeFill),
    ("labelBoxBorderColor", Role::NodeBorder),
    ("labelTextColor", Role::Text),
    ("loopTextColor", Role::Text),
    ("activationBkgColor", Role::SecondaryFill),
    ("activationBorderColor", Role::Border),
    ("sequenceNumberColor", Role::Background),
    // Gantt.
    ("sectionBkgColor", Role::TertiaryFill),
    ("altSectionBkgColor", Role::Background),
    ("sectionBkgColor2", Role::SecondaryFill),
    ("excludeBkgColor", Role::SecondaryFill),
    ("taskBorderColor", Role::NodeBorder),
    ("taskBkgColor", Role::NodeFill),
    ("taskTextColor", Role::Text),
    ("taskTextOutsideColor", Role::Text),
    ("taskTextLightColor", Role::Text),
    ("taskTextDarkColor", Role::Text),
    ("taskTextClickableColor", Role::NodeBorder),
    ("activeTaskBorderColor", Role::NodeBorder),
    ("activeTaskBkgColor", Role::SecondaryFill),
    ("gridColor", Role::Grid),
    ("doneTaskBkgColor", Role::SecondaryFill),
    ("doneTaskBorderColor", Role::Border),
    ("critBorderColor", Role::Critical),
    ("critBkgColor", Role::CriticalFill),
    ("todayLineColor", Role::Critical),
    ("vertLineColor", Role::Line),
    // State.
    ("stateBkg", Role::NodeFill),
    ("stateLabelColor", Role::Text),
    ("stateBorder", Role::NodeBorder),
    ("transitionColor", Role::Line),
    ("transitionLabelColor", Role::Text),
    ("labelColor", Role::Text),
    ("labelBackgroundColor", Role::Background),
    ("compositeBackground", Role::Background),
    ("compositeTitleBackground", Role::TertiaryFill),
    ("compositeBorder", Role::Border),
    ("altBackground", Role::TertiaryFill),
    ("innerEndBackground", Role::NodeBorder),
    ("specialStateColor", Role::Line),
    ("errorBkgColor", Role::CriticalFill),
    ("errorTextColor", Role::Critical),
    // Class and ER.
    ("classText", Role::Text),
    ("rowOdd", Role::Background),
    ("rowEven", Role::TertiaryFill),
    ("attributeBackgroundColorOdd", Role::Background),
    ("attributeBackgroundColorEven", Role::TertiaryFill),
    // Requirement.
    ("requirementBackground", Role::NodeFill),
    ("requirementBorderColor", Role::NodeBorder),
    ("requirementTextColor", Role::Text),
    ("relationColor", Role::Line),
    ("relationLabelBackground", Role::Background),
    ("relationLabelColor", Role::Text),
    // C4.
    ("personBkg", Role::NodeFill),
    ("personBorder", Role::NodeBorder),
    // Architecture.
    ("archEdgeColor", Role::Line),
    ("archEdgeArrowColor", Role::Line),
    ("archGroupBorderColor", Role::Border),
    // Git graph labels.
    ("commitLabelColor", Role::Text),
    ("commitLabelBackground", Role::SecondaryFill),
    ("tagLabelColor", Role::Text),
    ("tagLabelBackground", Role::NoteFill),
    ("tagLabelBorder", Role::NoteBorder),
    // Pie.
    ("pieTitleTextColor", Role::Text),
    ("pieSectionTextColor", Role::Text),
    ("pieLegendTextColor", Role::Text),
    ("pieStrokeColor", Role::Background),
    ("pieOuterStrokeColor", Role::Border),
    // Quadrant chart.
    ("quadrant1Fill", Role::Category(0)),
    ("quadrant2Fill", Role::Category(1)),
    ("quadrant3Fill", Role::Category(2)),
    ("quadrant4Fill", Role::Category(3)),
    ("quadrant1TextFill", Role::Text),
    ("quadrant2TextFill", Role::Text),
    ("quadrant3TextFill", Role::Text),
    ("quadrant4TextFill", Role::Text),
    ("quadrantPointFill", Role::NodeBorder),
    ("quadrantPointTextFill", Role::Text),
    ("quadrantXAxisTextFill", Role::Text),
    ("quadrantYAxisTextFill", Role::Text),
    ("quadrantInternalBorderStrokeFill", Role::Border),
    ("quadrantExternalBorderStrokeFill", Role::Border),
    ("quadrantTitleFill", Role::Text),
    // Journey, venn and others.
    ("faceColor", Role::NoteFill),
    ("vennTitleTextColor", Role::Text),
    ("vennSetTextColor", Role::Text),
    ("legendTextColor", Role::Text),
    ("scaleLabelColor", Role::Text),
    ("branchLabelColor", Role::Text),
    ("boundaryColor", Role::Border),
];

/// The site configuration merman renders with: the base theme over marker
/// colours, strict labels and the caps. Documents cannot override any of
/// it (`secure`).
pub(crate) fn site_config(font_family: &str, max_edges: usize, max_text: usize) -> Value {
    let mut vars = Map::new();
    vars.insert("darkMode".into(), Value::Bool(false));
    vars.insert("fontFamily".into(), Value::String(font_family.into()));
    for (name, role) in VARIABLES {
        vars.insert((*name).into(), Value::String(hex(*role)));
    }
    for i in 0..CATEGORY_COUNT {
        let cat = hex(Role::Category(i));
        let n = usize::from(i);
        vars.insert(format!("cScale{n}"), Value::String(cat.clone()));
        vars.insert(format!("cScalePeer{n}"), Value::String(hex(Role::Border)));
        vars.insert(format!("cScaleLabel{n}"), Value::String(hex(Role::Text)));
        vars.insert(
            format!("cScaleInv{n}"),
            Value::String(hex(Role::Background)),
        );
        vars.insert(format!("pie{}", n + 1), Value::String(cat.clone()));
        vars.insert(
            format!("surface{n}"),
            Value::String(hex(Role::TertiaryFill)),
        );
        vars.insert(format!("surfacePeer{n}"), Value::String(hex(Role::Border)));
        if n < 8 {
            vars.insert(format!("git{n}"), Value::String(cat.clone()));
            vars.insert(format!("gitInv{n}"), Value::String(hex(Role::Background)));
            vars.insert(format!("gitBranchLabel{n}"), Value::String(hex(Role::Text)));
            vars.insert(format!("fillType{n}"), Value::String(cat));
        }
    }
    let palette: Vec<String> = (0..CATEGORY_COUNT)
        .map(|i| hex(Role::Category(i)))
        .collect();
    vars.insert(
        "packet".into(),
        json!({
            "startByteColor": hex(Role::Text),
            "endByteColor": hex(Role::Text),
            "labelColor": hex(Role::Text),
            "titleColor": hex(Role::Text),
            "blockStrokeColor": hex(Role::Line),
            "blockFillColor": hex(Role::TertiaryFill),
        }),
    );
    vars.insert(
        "xyChart".into(),
        json!({
            "backgroundColor": hex(Role::Background),
            "titleColor": hex(Role::Text),
            "xAxisLabelColor": hex(Role::Text),
            "xAxisTitleColor": hex(Role::Text),
            "xAxisTickColor": hex(Role::Grid),
            "xAxisLineColor": hex(Role::Grid),
            "yAxisLabelColor": hex(Role::Text),
            "yAxisTitleColor": hex(Role::Text),
            "yAxisTickColor": hex(Role::Grid),
            "yAxisLineColor": hex(Role::Grid),
            "plotColorPalette": palette.join(","),
        }),
    );
    vars.insert(
        "radar".into(),
        json!({
            "axisColor": hex(Role::Grid),
            "graticuleColor": hex(Role::Grid),
        }),
    );
    let secure = [
        "secure",
        "securityLevel",
        "startOnLoad",
        "maxTextSize",
        "suppressErrorRendering",
        "maxEdges",
        "theme",
        "themeVariables",
        "themeCSS",
        "fontFamily",
        "darkMode",
        "look",
        "htmlLabels",
        "flowchart",
    ];
    json!({
        "secure": secure,
        "theme": "base",
        "look": "classic",
        "fontFamily": font_family,
        "htmlLabels": false,
        "flowchart": { "htmlLabels": false },
        "maxEdges": max_edges,
        "maxTextSize": max_text,
        "themeVariables": Value::Object(vars),
    })
}

/// Concrete colours for every role, filled by the host from its theme
/// tokens. Straight (not premultiplied) sRGB with alpha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    base: [[u8; 4]; 13],
    categories: [[u8; 4]; CATEGORY_COUNT as usize],
}

impl Palette {
    /// A palette that gives every role the colour `f` returns.
    pub fn from_fn(mut f: impl FnMut(Role) -> [u8; 4]) -> Self {
        let mut base = [[0; 4]; 13];
        for (slot, role) in base.iter_mut().zip(Role::all()) {
            *slot = f(role);
        }
        let mut categories = [[0; 4]; CATEGORY_COUNT as usize];
        for (i, slot) in (0..CATEGORY_COUNT).zip(categories.iter_mut()) {
            *slot = f(Role::Category(i));
        }
        Self { base, categories }
    }

    /// The marker colours themselves (a light look, for tests and tools).
    pub fn markers() -> Self {
        Self::from_fn(|r| {
            let [red, g, b] = r.marker();
            [red, g, b, 0xFF]
        })
    }

    /// The colour of `role`.
    pub fn get(&self, role: Role) -> [u8; 4] {
        match role {
            Role::Category(i) => self.categories[usize::from(i % CATEGORY_COUNT)],
            other => {
                let index = Role::all().position(|r| r == other).unwrap_or(0);
                self.base[index]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_distinct_beyond_tolerance() {
        let roles: Vec<Role> = Role::all().collect();
        for (i, a) in roles.iter().enumerate() {
            assert_eq!(Role::from_marker(a.marker()), Some(*a));
            for b in &roles[i + 1..] {
                let (ma, mb) = (a.marker(), b.marker());
                let far = (0..3).any(|c| ma[c].abs_diff(mb[c]) > 2 * MARKER_TOLERANCE + 2);
                assert!(far, "{a:?} and {b:?} markers are too close");
            }
        }
    }

    #[test]
    fn palette_round_trips() {
        let p = Palette::from_fn(|r| match r {
            Role::Category(i) => [i, 0, 0, 255],
            Role::Text => [1, 2, 3, 255],
            _ => [9, 9, 9, 255],
        });
        assert_eq!(p.get(Role::Text), [1, 2, 3, 255]);
        assert_eq!(p.get(Role::Category(5)), [5, 0, 0, 255]);
        assert_eq!(p.get(Role::Background), [9, 9, 9, 255]);
    }
}
