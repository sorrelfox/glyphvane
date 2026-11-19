// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the settings window lists, without GPUI: the sections of the
//! sidebar, the rows generated from the settings schema, their labels and
//! units, search, and turning what is typed into a field into a value.
//!
//! Only the Appearance section is designed; its rows are drawn by
//! [`super::appearance`]. Every other section is generated from the schema
//! here (not designed: label from the key, description from the setting's
//! documentation, a control chosen by the value kind).

use gv_settings::{Kind, Scope, Spec};
use toml::Value;

/// A section of the sidebar, in the design's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionId {
    General,
    Appearance,
    Editor,
    Preview,
    Links,
    Files,
    Vault,
    Profiles,
    Spelling,
    Keymap,
    PrivacyAndSecurity,
    About,
}

impl SectionId {
    /// Every section, in the sidebar's order.
    pub const ALL: [Self; 12] = [
        Self::General,
        Self::Appearance,
        Self::Editor,
        Self::Preview,
        Self::Links,
        Self::Files,
        Self::Vault,
        Self::Profiles,
        Self::Spelling,
        Self::Keymap,
        Self::PrivacyAndSecurity,
        Self::About,
    ];

    /// The sidebar label and page title.
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Editor => "Editor",
            Self::Preview => "Preview",
            Self::Links => "Links",
            Self::Files => "Files",
            Self::Vault => "Vault",
            Self::Profiles => "Profiles",
            Self::Spelling => "Spelling",
            Self::Keymap => "Keymap",
            Self::PrivacyAndSecurity => "Privacy and security",
            Self::About => "About",
        }
    }

    /// A stable name for selectors and tests: `privacy-and-security`.
    pub fn slug(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Editor => "editor",
            Self::Preview => "preview",
            Self::Links => "links",
            Self::Files => "files",
            Self::Vault => "vault",
            Self::Profiles => "profiles",
            Self::Spelling => "spelling",
            Self::Keymap => "keymap",
            Self::PrivacyAndSecurity => "privacy-and-security",
            Self::About => "about",
        }
    }

    /// The settings-file table the section edits; `None` for Keymap and
    /// About, which have their own files or none.
    pub fn table(self) -> Option<&'static str> {
        Some(match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Editor => "editor",
            Self::Preview => "preview",
            Self::Links => "links",
            Self::Files => "files",
            Self::Vault => "vault",
            Self::Profiles => "profiles",
            Self::Spelling => "spelling",
            Self::PrivacyAndSecurity => "privacy_and_security",
            Self::Keymap | Self::About => return None,
        })
    }

    /// Whether the section has a design (only Appearance; NFR-UI-01).
    pub fn is_designed(self) -> bool {
        self == Self::Appearance
    }

    /// The section's settings, in schema order.
    pub fn specs(self) -> Vec<&'static Spec> {
        match self.table() {
            Some(table) => gv_settings::specs()
                .iter()
                .filter(|s| s.section == table && shown(s))
                .collect(),
            None => Vec::new(),
        }
    }

    /// The section of the settings key `path`.
    pub fn of_key(path: &str) -> Option<Self> {
        let table = path.split_once('.')?.0;
        Self::ALL.into_iter().find(|s| s.table() == Some(table))
    }
}

/// Whether the window lists `spec`: not the automatic update check where
/// this copy cannot check (package-manager builds, the Flatpak), as Help ▸
/// Check for updates says.
pub fn shown(spec: &Spec) -> bool {
    spec.path != crate::updates::SETTING || crate::updates::enabled()
}

/// A unit shown after a number field, from the key's suffix.
pub fn unit(spec: &Spec) -> Option<&'static str> {
    let key = spec.key;
    [
        ("_font_size", "px"),
        ("_chars", "ch"),
        ("_percent", "%"),
        ("_ms", "ms"),
        ("_secs", "s"),
        ("_bytes", "bytes"),
    ]
    .into_iter()
    .find(|(suffix, _)| key.ends_with(suffix))
    .map(|(_, unit)| unit)
}

/// The row label: the key as a sentence, without a unit suffix
/// (`hover_preview_delay_ms` → "Hover preview delay"). Not designed.
pub fn label(spec: &Spec) -> String {
    let mut key = spec.key;
    for suffix in ["_chars", "_percent", "_ms", "_secs", "_bytes"] {
        if let Some(stripped) = key.strip_suffix(suffix) {
            key = stripped;
        }
    }
    sentence_case(&key.replace('_', " "))
}

fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The label of an enum value. Profiles use their interface names; other
/// values are the kebab-case word as a sentence (not designed).
pub fn variant_label(spec: &Spec, variant: &str) -> String {
    match (spec.path, variant) {
        ("profiles.default_profile" | "profiles.vault_profile", "auto") => "Detect".to_owned(),
        ("profiles.default_profile" | "profiles.vault_profile", id) => {
            match gv_profile::Requested::from_id(id) {
                Some(profile) => profile.name().to_owned(),
                None => id.to_owned(),
            }
        }
        ("appearance.high_contrast", "follow-system") => "Follow the system".to_owned(),
        ("appearance.theme" | "appearance.preferred_light_theme", "semi-light") => {
            "Semi-light".to_owned()
        }
        (_, other) => sentence_case(&other.replace('-', " ")),
    }
}

/// The control a generated row uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Switch,
    Select,
    Number,
    Text,
    /// Comma-separated strings in a text field.
    List,
}

impl Control {
    pub fn of(spec: &Spec) -> Self {
        match spec.kind {
            Kind::Bool => Self::Switch,
            Kind::Enum(_) => Self::Select,
            Kind::Integer { .. } | Kind::Float { .. } => Self::Number,
            Kind::String => Self::Text,
            Kind::StringList => Self::List,
        }
    }
}

/// A value as a field shows it: numbers without a needless `.0`, lists as
/// comma-separated words.
pub fn field_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        Value::Float(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map_or_else(|| item.to_string(), str::to_owned)
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

/// Turns what was typed into a field into a value of the setting's kind.
/// The range is checked later, by the schema; this only refuses text that
/// is not a number at all, with the schema's own wording.
pub fn parse_field(spec: &Spec, text: &str) -> Result<Value, String> {
    let text = text.trim();
    let not_a_number = || {
        format!(
            "Expected {}, found \u{201c}{text}\u{201d}.",
            spec.kind.expected()
        )
    };
    match spec.kind {
        Kind::Integer { .. } => text
            .parse::<i64>()
            .map(Value::Integer)
            .map_err(|_| not_a_number()),
        Kind::Float { .. } => match text.parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(Value::Float(f)),
            _ => Err(not_a_number()),
        },
        Kind::String => Ok(Value::String(text.to_owned())),
        Kind::StringList => Ok(Value::Array(
            text.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| Value::String(item.to_owned()))
                .collect(),
        )),
        Kind::Bool => match text {
            "true" => Ok(Value::Boolean(true)),
            "false" => Ok(Value::Boolean(false)),
            _ => Err(not_a_number()),
        },
        Kind::Enum(variants) => variants
            .iter()
            .find(|v| **v == text)
            .map(|v| Value::String((*v).to_owned()))
            .ok_or_else(not_a_number),
    }
}

/// Whether the setting can be set for one vault.
pub fn per_vault(spec: &Spec) -> bool {
    spec.scope != Scope::GlobalOnly
}

/// A search hit: the setting and its section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub section: SectionId,
    pub path: &'static str,
}

/// The settings that match `query`: every word must occur in the label,
/// the description, the key or the section name (ignoring case).
pub fn search(query: &str) -> Vec<Hit> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for section in SectionId::ALL {
        for spec in section.specs() {
            let haystack = format!(
                "{} {} {} {} {}",
                label(spec),
                appearance_label(spec.path).unwrap_or_default(),
                spec.doc,
                spec.path,
                section.label()
            )
            .to_lowercase();
            if words.iter().all(|w| haystack.contains(w.as_str())) {
                hits.push(Hit {
                    section,
                    path: spec.path,
                });
            }
        }
    }
    hits
}

/// The sections with hits, in sidebar order.
pub fn sections_of(hits: &[Hit]) -> Vec<SectionId> {
    SectionId::ALL
        .into_iter()
        .filter(|s| hits.iter().any(|h| h.section == *s))
        .collect()
}

/// The designed label of an Appearance setting (SPEC § Settings window),
/// which search also matches.
pub fn appearance_label(path: &str) -> Option<&'static str> {
    Some(match path {
        "appearance.theme" => "Theme",
        "appearance.preferred_light_theme" => "In light appearance, use",
        "appearance.high_contrast" => "High contrast",
        "appearance.interface_font" | "appearance.interface_font_size" => "Interface font",
        "appearance.preview_font" | "appearance.preview_font_size" => "Preview font",
        "appearance.editor_font" | "appearance.editor_font_size" => "Editor font",
        "appearance.readable_line_width" | "appearance.readable_line_width_chars" => {
            "Readable line width"
        }
        "appearance.fill_wide_blocks" | "appearance.wide_block_width_chars" => {
            "Wide tables and diagrams"
        }
        "appearance.zoom_percent" => "Zoom",
        "appearance.code_theme" => "Code theme",
        _ => return None,
    })
}

/// The label a row shows: the designed one for Appearance, else generated.
pub fn row_label(spec: &Spec) -> String {
    match appearance_label(spec.path) {
        // Size fields share their font's row; listed alone (in search
        // results) they say what they are.
        Some(label) if spec.key.ends_with("_size") => format!("{label} size"),
        Some(_) if spec.key == "readable_line_width_chars" => "Readable line width".to_owned(),
        // The width shares the switch's row; alone it says what it is.
        Some(_) if spec.key == "wide_block_width_chars" => {
            "Width of wide tables and diagrams".to_owned()
        }
        Some(label) => label.to_owned(),
        None => label(spec),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(path: &str) -> &'static Spec {
        gv_settings::spec(path).expect("a setting")
    }

    #[test]
    fn every_schema_section_has_a_sidebar_section() {
        for section in gv_settings::sections() {
            assert!(
                SectionId::ALL
                    .iter()
                    .any(|s| s.table() == Some(section.name)),
                "{} has no sidebar section",
                section.name
            );
        }
        for section in SectionId::ALL {
            if let Some(table) = section.table() {
                assert!(!section.specs().is_empty(), "{table} has no settings");
            }
        }
    }

    #[test]
    fn labels_and_units_come_from_the_keys() {
        assert_eq!(
            label(spec("preview.hover_preview_delay_ms")),
            "Hover preview delay"
        );
        assert_eq!(unit(spec("preview.hover_preview_delay_ms")), Some("ms"));
        assert_eq!(label(spec("general.open_files_in")), "Open files in");
        assert_eq!(unit(spec("appearance.interface_font_size")), Some("px"));
        assert_eq!(unit(spec("appearance.zoom_percent")), Some("%"));
        assert_eq!(unit(spec("general.viewer_mode")), None);
        assert_eq!(
            variant_label(spec("profiles.default_profile"), "gfm-file"),
            gv_profile::Requested::from_id("gfm-file")
                .map(|p| p.name())
                .unwrap_or_default()
        );
        assert_eq!(
            variant_label(spec("profiles.vault_profile"), "auto"),
            "Detect"
        );
        assert_eq!(
            variant_label(spec("files.external_files"), "ask"),
            "Ask".to_owned()
        );
    }

    #[test]
    fn fields_parse_by_kind() {
        let number = spec("appearance.interface_font_size");
        assert_eq!(parse_field(number, " 14 "), Ok(Value::Float(14.0)));
        assert!(parse_field(number, "big").is_err());
        let integer = spec("preview.embed_max_depth");
        assert_eq!(parse_field(integer, "7"), Ok(Value::Integer(7)));
        assert!(parse_field(integer, "7.5").is_err());
        let list = spec("spelling.languages");
        assert_eq!(
            parse_field(list, "en_US, de_DE,,"),
            Ok(Value::Array(vec![
                Value::String("en_US".into()),
                Value::String("de_DE".into())
            ]))
        );
        assert_eq!(field_text(&Value::Float(12.5)), "12.5");
        assert_eq!(field_text(&Value::Float(15.0)), "15");
    }

    #[test]
    fn the_flatpak_lists_no_update_check() {
        let paths =
            |section: SectionId| -> Vec<&str> { section.specs().iter().map(|s| s.path).collect() };
        {
            let _flatpak = crate::updates::ForceFlatpak::new(true);
            assert!(!paths(SectionId::General).contains(&crate::updates::SETTING));
            assert!(
                search("check for updates")
                    .iter()
                    .all(|h| h.path != crate::updates::SETTING)
            );
            // The rest of General stays.
            assert!(paths(SectionId::General).contains(&"general.viewer_mode"));
        }
        let _flatpak = crate::updates::ForceFlatpak::new(false);
        assert_eq!(
            paths(SectionId::General).contains(&crate::updates::SETTING),
            crate::updates::enabled_by_build()
        );
    }

    #[test]
    fn search_matches_labels_docs_and_keys() {
        let hits = search("hover delay");
        assert_eq!(
            hits,
            vec![Hit {
                section: SectionId::Preview,
                path: "preview.hover_preview_delay_ms"
            }]
        );
        // Designed labels are searchable too.
        assert!(
            search("light appearance")
                .iter()
                .any(|h| h.path == "appearance.preferred_light_theme")
        );
        // Both wide block settings are found by their row's label.
        let wide: Vec<&str> = search("wide tables").iter().map(|h| h.path).collect();
        assert_eq!(
            wide,
            vec![
                "appearance.fill_wide_blocks",
                "appearance.wide_block_width_chars"
            ]
        );
        assert_eq!(
            row_label(spec("appearance.wide_block_width_chars")),
            "Width of wide tables and diagrams"
        );
        assert_eq!(
            row_label(spec("appearance.fill_wide_blocks")),
            "Wide tables and diagrams"
        );
        assert!(search("   ").is_empty());
        assert!(search("no such setting anywhere").is_empty());
        assert_eq!(sections_of(&search("profile")), vec![SectionId::Profiles]);
    }
}
