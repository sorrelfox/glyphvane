// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Profile names: the built-in profiles of 1.0 ([`ProfileId`]) and the
//! dialects detection can recognize before their profiles ship
//! ([`PlannedProfile`]).

use std::fmt;

/// A built-in profile of 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProfileId {
    /// CommonMark 0.31.2, no extensions.
    CommonMark,
    /// GitHub Flavored Markdown as github.com renders repository files.
    GfmFile,
    /// GitHub Flavored Markdown as github.com renders comments.
    GfmComment,
    /// Obsidian Flavored Markdown.
    Obsidian,
    /// The Extended superset: the default when nothing is detected.
    Extended,
}

impl ProfileId {
    /// Every built-in profile, in the order menus list them.
    pub const ALL: [ProfileId; 5] = [
        ProfileId::CommonMark,
        ProfileId::GfmFile,
        ProfileId::GfmComment,
        ProfileId::Obsidian,
        ProfileId::Extended,
    ];

    /// The profile id string (`gfm-file`, ...), as settings files and
    /// `.glyphvane.toml` spell it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CommonMark => "commonmark",
            Self::GfmFile => "gfm-file",
            Self::GfmComment => "gfm-comment",
            Self::Obsidian => "obsidian",
            Self::Extended => "extended",
        }
    }

    /// The profile with id `id`; `None` for anything else.
    #[must_use]
    pub fn from_id(id: &str) -> Option<ProfileId> {
        Self::ALL.into_iter().find(|p| p.as_str() == id)
    }

    /// Whether this build parses and renders the profile. A detected
    /// profile that has not shipped falls back to [`ProfileId::Extended`]
    /// with a hint. Every built-in profile has shipped (Obsidian with the
    /// vault milestone, tuned from `app.json` by
    /// [`crate::compile::Tuning`]).
    #[must_use]
    pub fn is_shipped(self) -> bool {
        true
    }

    /// The release that brings full support, for a profile that has not
    /// shipped.
    #[must_use]
    pub fn ships_in(self) -> Option<&'static str> {
        (!self.is_shipped()).then_some("0.5")
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A dialect detection recognizes whose profile ships after 1.0. Until it
/// ships, the document uses [`ProfileId::Extended`] and the status bar
/// says what was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlannedProfile {
    /// Pandoc `markdown`.
    Pandoc,
    /// Quarto (`.qmd`, `_quarto.yml`).
    Quarto,
    /// R Markdown (`.rmd`).
    RMarkdown,
    /// PHP Markdown Extra.
    PhpExtra,
    /// MkDocs with Python-Markdown and PyMdown (`mkdocs.yml`).
    MkDocs,
    /// Docusaurus (`docusaurus.config.*`, `.mdx`).
    Docusaurus,
    /// VitePress (`.vitepress/`).
    VitePress,
    /// GitLab Flavored Markdown (`.gitlab-ci.yml`, a GitLab remote).
    GitLab,
    /// kramdown, as Jekyll uses it (`_config.yml`).
    Kramdown,
    /// Hugo's Goldmark (`hugo.toml`, ...).
    Hugo,
}

impl PlannedProfile {
    /// Every planned profile.
    pub const ALL: [PlannedProfile; 10] = [
        Self::Pandoc,
        Self::Quarto,
        Self::RMarkdown,
        Self::PhpExtra,
        Self::MkDocs,
        Self::Docusaurus,
        Self::VitePress,
        Self::GitLab,
        Self::Kramdown,
        Self::Hugo,
    ];

    /// The id the profile will have.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pandoc => "pandoc",
            Self::Quarto => "quarto",
            Self::RMarkdown => "rmarkdown",
            Self::PhpExtra => "php-extra",
            Self::MkDocs => "mkdocs",
            Self::Docusaurus => "docusaurus",
            Self::VitePress => "vitepress",
            Self::GitLab => "gitlab",
            Self::Kramdown => "kramdown",
            Self::Hugo => "hugo",
        }
    }

    /// The planned profile with id `id`.
    #[must_use]
    pub fn from_id(id: &str) -> Option<PlannedProfile> {
        Self::ALL.into_iter().find(|p| p.as_str() == id)
    }

    /// The name the interface uses.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Pandoc => "Pandoc",
            Self::Quarto => "Quarto",
            Self::RMarkdown => "R Markdown",
            Self::PhpExtra => "PHP Markdown Extra",
            Self::MkDocs => "MkDocs",
            Self::Docusaurus => "Docusaurus",
            Self::VitePress => "VitePress",
            Self::GitLab => "GitLab",
            Self::Kramdown => "kramdown",
            Self::Hugo => "Hugo",
        }
    }

    /// The release planned to bring the profile.
    #[must_use]
    pub fn ships_in(self) -> &'static str {
        match self {
            Self::Pandoc | Self::Quarto | Self::RMarkdown | Self::PhpExtra => "1.1",
            Self::MkDocs | Self::Docusaurus | Self::VitePress => "1.2",
            Self::GitLab | Self::Kramdown => "1.3",
            Self::Hugo => "a later release",
        }
    }
}

impl fmt::Display for PlannedProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a detection step or a `.glyphvane.toml` asks for: a built-in
/// profile, or one that ships later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Requested {
    BuiltIn(ProfileId),
    Planned(PlannedProfile),
}

impl Requested {
    /// The built-in or planned profile with id `id`.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Requested> {
        ProfileId::from_id(id)
            .map(Self::BuiltIn)
            .or_else(|| PlannedProfile::from_id(id).map(Self::Planned))
    }

    /// The profile this build uses for the request: the built-in profile
    /// when it has shipped, otherwise [`ProfileId::Extended`].
    #[must_use]
    pub fn effective(self) -> ProfileId {
        match self {
            Self::BuiltIn(id) if id.is_shipped() => id,
            Self::BuiltIn(_) | Self::Planned(_) => ProfileId::Extended,
        }
    }

    /// The interface name of what was asked for.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::BuiltIn(id) => crate::builtin::definition(id).name,
            Self::Planned(p) => p.name(),
        }
    }

    /// When the request cannot be met yet: "MkDocs detected — full support
    /// in 1.2".
    #[must_use]
    pub fn pending_note(self) -> Option<String> {
        let release = match self {
            Self::BuiltIn(id) => id.ships_in()?,
            Self::Planned(p) => p.ships_in(),
        };
        Some(format!(
            "{} detected — full support in {release}",
            self.name()
        ))
    }
}

impl From<ProfileId> for Requested {
    fn from(id: ProfileId) -> Self {
        Self::BuiltIn(id)
    }
}

impl fmt::Display for Requested {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BuiltIn(id) => id.fmt(f),
            Self::Planned(p) => p.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for id in ProfileId::ALL {
            assert_eq!(ProfileId::from_id(id.as_str()), Some(id));
            assert_eq!(
                Requested::from_id(id.as_str()),
                Some(Requested::BuiltIn(id))
            );
        }
        for p in PlannedProfile::ALL {
            assert_eq!(Requested::from_id(p.as_str()), Some(Requested::Planned(p)));
            assert_eq!(ProfileId::from_id(p.as_str()), None);
        }
        assert_eq!(Requested::from_id("nope"), None);
    }

    #[test]
    fn unshipped_profiles_fall_back_to_extended() {
        assert_eq!(
            Requested::BuiltIn(ProfileId::GfmFile).effective(),
            ProfileId::GfmFile
        );
        // Every built-in profile has shipped.
        assert_eq!(
            Requested::BuiltIn(ProfileId::Obsidian).effective(),
            ProfileId::Obsidian
        );
        assert!(ProfileId::ALL.into_iter().all(ProfileId::is_shipped));
        assert_eq!(
            Requested::Planned(PlannedProfile::MkDocs).effective(),
            ProfileId::Extended
        );
        assert_eq!(
            Requested::Planned(PlannedProfile::MkDocs)
                .pending_note()
                .as_deref(),
            Some("MkDocs detected — full support in 1.2")
        );
        assert_eq!(Requested::BuiltIn(ProfileId::GfmFile).pending_note(), None);
    }
}
