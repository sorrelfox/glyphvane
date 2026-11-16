// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Flavor profiles in the windows: which profile a tab's document uses and
//! why ([`TabProfile`]), the overrides set from the status bar's profile
//! chip ([`ProfileOverrides`]), and what the compiled profile means for
//! the parser.
//!
//! Detection follows [`gv_profile::detect`]. The settings give the default
//! profile (`profiles.default_profile`) and a vault override
//! (`profiles.vault_profile`). Overrides picked from the chip menu for a
//! file, a folder or a vault are kept for the session: per-file and
//! per-folder state in app settings needs the vault settings layers, which
//! the windows do not load yet.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{App, Global};
use gv_profile::detect::{DetectOptions, Overrides};
use gv_profile::switches::TildeSingle;
use gv_profile::{CompiledProfile, Detection, ProfileId, Reason, Requested};
use gv_syntax::{FrontMatterOptions, ParseOptions};

use crate::AppSettings;

/// Where an override from the chip menu applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Scope {
    /// This file.
    #[default]
    File,
    /// This file's folder and the folders below it.
    Folder,
    /// The vault the file is in.
    Vault,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::File, Scope::Folder, Scope::Vault];

    /// The label in the chip menu.
    pub fn label(self) -> &'static str {
        match self {
            Self::File => "This file",
            Self::Folder => "This folder",
            Self::Vault => "This vault",
        }
    }
}

/// The overrides picked in the chip menu, and the folders where a dialect
/// hint was dismissed. They last for the session.
#[derive(Debug, Default)]
pub struct ProfileOverrides {
    files: HashMap<PathBuf, ProfileId>,
    folders: HashMap<PathBuf, ProfileId>,
    vaults: HashMap<PathBuf, ProfileId>,
    dismissed_hints: HashSet<PathBuf>,
}

impl Global for ProfileOverrides {}

impl ProfileOverrides {
    /// The override for `file` at `scope`; `None` when it is detected.
    pub fn get(cx: &App, file: &Path, scope: Scope) -> Option<ProfileId> {
        let this = cx.try_global::<Self>()?;
        let key = scope_key(file, scope)?;
        match scope {
            Scope::File => this.files.get(&key),
            Scope::Folder => this.folders.get(&key),
            Scope::Vault => this.vaults.get(&key),
        }
        .copied()
    }

    /// Sets (or, with `None`, removes) the override for `file` at `scope`.
    pub fn set(cx: &mut App, file: &Path, scope: Scope, profile: Option<ProfileId>) {
        let Some(key) = scope_key(file, scope) else {
            return;
        };
        let this = cx.default_global::<Self>();
        let map = match scope {
            Scope::File => &mut this.files,
            Scope::Folder => &mut this.folders,
            Scope::Vault => &mut this.vaults,
        };
        match profile {
            Some(profile) => map.insert(key, profile),
            None => map.remove(&key),
        };
    }

    /// Whether a dialect hint was dismissed in `folder`.
    pub fn hint_dismissed(cx: &App, folder: &Path) -> bool {
        cx.try_global::<Self>()
            .is_some_and(|this| this.dismissed_hints.contains(folder))
    }

    /// Remembers that a dialect hint was dismissed in `folder`.
    pub fn dismiss_hint(cx: &mut App, folder: &Path) {
        cx.default_global::<Self>()
            .dismissed_hints
            .insert(folder.to_path_buf());
    }
}

/// The path an override at `scope` is keyed by: the file, its folder, or
/// its vault root.
fn scope_key(file: &Path, scope: Scope) -> Option<PathBuf> {
    let file = std::path::absolute(file).ok()?;
    Some(match scope {
        Scope::File => file,
        Scope::Folder => file.parent()?.to_path_buf(),
        Scope::Vault => gv_vault::detect_root(&file).root,
    })
}

/// The profile of a tab's document.
#[derive(Debug, Clone)]
pub struct TabProfile {
    /// The file it was detected for; `None` for an untitled document.
    pub path: Option<PathBuf>,
    pub detection: Detection,
    pub compiled: Arc<CompiledProfile>,
}

impl TabProfile {
    /// The profile the document uses.
    pub fn id(&self) -> ProfileId {
        self.compiled.id
    }

    /// The chip's tooltip: the reason, and what is still to ship.
    pub fn tooltip(&self) -> String {
        let home = gv_vault::root::home_dir();
        let mut text = self.detection.reason.describe(home.as_deref());
        if let Some(note) = self.detection.pending_note() {
            text.push_str(" · ");
            text.push_str(&note);
        }
        if !self.detection.config_diagnostics().is_empty() {
            text.push_str(" · `.glyphvane.toml` has problems");
        }
        text
    }

    /// The parse options of the compiled profile.
    pub fn parse_options(&self) -> ParseOptions {
        parse_options(&self.compiled)
    }
}

/// The options the parser takes today, from a compiled profile.
///
/// The parser keeps its own per-profile table
/// ([`ParseOptions::for_profile`]) of the constructs it implements so
/// far; that is the base, so a profile never turns on a construct the
/// parser would read with another profile's meaning (Extended's `~x~` is
/// subscript, not GFM strikethrough). Switches changed by tuning or
/// `.glyphvane.toml` are applied on top where the parser has an option
/// for them. Once the parser takes a [`CompiledProfile`] itself, this
/// mapping goes.
pub fn parse_options(profile: &CompiledProfile) -> ParseOptions {
    let mut options = ParseOptions::for_profile(profile.id.as_str()).unwrap_or_default();
    let s = &profile.switches;
    for key in &profile.changed {
        match *key {
            "front_matter_yaml" | "front_matter_toml" => {
                options.front_matter = FrontMatterOptions {
                    yaml: s.front_matter.yaml,
                    toml: s.front_matter.toml,
                };
            }
            "tables" => options.tables = s.tables,
            "tilde_single" | "tilde_double" => {
                options.strikethrough = s.tilde_single == TildeSingle::Strike;
            }
            "extended_autolinks" => options.autolinks = s.extended_autolinks,
            _ => {}
        }
    }
    options
}

/// The detection settings: the default profile and the vault override
/// from the settings in effect.
fn settings(cx: &App) -> (DetectOptions, Option<ProfileId>) {
    let defaults = gv_settings::Settings::default();
    let settings = cx
        .try_global::<AppSettings>()
        .map_or(&defaults, AppSettings::settings);
    let profiles = &settings.profiles;
    let default =
        ProfileId::from_id(profiles.default_profile.as_str()).unwrap_or(ProfileId::Extended);
    let vault = ProfileId::from_id(profiles.vault_profile.as_str());
    (
        DetectOptions {
            default,
            ..DetectOptions::default()
        },
        vault,
    )
}

/// Detects the profile of the document at `path` (`None`: an untitled
/// document, which uses the default profile).
pub fn detect(path: Option<&Path>, cx: &App) -> TabProfile {
    let (options, settings_vault) = settings(cx);
    let detection = match path {
        Some(path) => {
            let overrides = overrides_for(path, settings_vault, cx);
            gv_profile::detect(path, &overrides, &options)
        }
        None => Detection {
            requested: Requested::BuiltIn(options.default),
            profile: Requested::BuiltIn(options.default).effective(),
            reason: Reason::Default,
            config: None,
            switches_apply: false,
        },
    };
    for d in detection.config_diagnostics() {
        if let Some(file) = &detection.config {
            tracing::warn!("{}:{d}", file.path.display());
        }
    }
    let compiled = Arc::new(detection.compile());
    TabProfile {
        path: path.map(Path::to_path_buf),
        detection,
        compiled,
    }
}

fn overrides_for(path: &Path, settings_vault: Option<ProfileId>, cx: &App) -> Overrides {
    let Some(this) = cx.try_global::<ProfileOverrides>() else {
        return Overrides {
            vault: settings_vault,
            ..Overrides::default()
        };
    };
    Overrides {
        file: ProfileOverrides::get(cx, path, Scope::File),
        folders: this
            .folders
            .iter()
            .map(|(folder, id)| (folder.clone(), *id))
            .collect(),
        vault: ProfileOverrides::get(cx, path, Scope::Vault).or(settings_vault),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_options_follow_the_front_matter_switches() {
        for id in ProfileId::ALL {
            let compiled = gv_profile::compile_builtin(id);
            assert_eq!(
                Some(parse_options(&compiled)),
                ParseOptions::for_profile(id.as_str()),
                "{id}"
            );
        }
    }

    #[test]
    fn switch_overrides_reach_the_parser_options() {
        use gv_profile::{SwitchOverrides, SwitchValue, Tuning};
        let mut overrides = SwitchOverrides::new();
        overrides
            .insert("front_matter_toml", SwitchValue::Bool(true))
            .expect("valid");
        overrides
            .insert("tables", SwitchValue::Bool(true))
            .expect("valid");
        overrides
            .insert("tilde_single", SwitchValue::Word("strike".into()))
            .expect("valid");
        let options = parse_options(&gv_profile::compile(
            ProfileId::CommonMark,
            Tuning::default(),
            &overrides,
        ));
        assert!(options.front_matter.toml && !options.front_matter.yaml);
        assert!(options.tables && options.strikethrough && !options.autolinks);
    }
}
