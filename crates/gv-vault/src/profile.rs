// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Link-resolution parameters per flavor profile.
//!
//! Each profile resolves links the way its tool does. The differences are
//! data in [`ProfileParams`]; the resolver has no per-profile branches
//! beyond what these fields say.

use std::fmt;
use std::sync::Arc;

/// How a profile names headings in fragments (the document model's enum).
pub use gv_model::Anchoring;
use gv_model::SlugAlgorithm;

use crate::path::VaultPath;
use crate::repo::RepoContext;
use crate::resolve::tiebreak::TieBreak;

/// The 1.0 profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileId {
    /// CommonMark 0.31.2, no extensions.
    CommonMark,
    /// GitHub, file rendering (repository files).
    GfmFile,
    /// GitHub, comment rendering.
    GfmComment,
    /// Obsidian.
    Obsidian,
    /// The Extended superset (the default for loose folders).
    Extended,
}

impl ProfileId {
    /// The profile id string (`gfm-file`, …).
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
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a bare path is looked up, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseStep {
    /// The vault (index) root.
    VaultRoot,
    /// The source file's folder.
    Relative,
    /// Basename / suffix lookup in the path index.
    Basename,
}

/// How good a match is, from best to worst. Combines with `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchQuality {
    /// As the profile's tool would resolve it.
    Exact,
    /// Only by case folding or NFC normalization.
    CaseFold,
    /// With leniency the tool does not have.
    Loose,
}

/// A directory index file pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexFile {
    /// A file name (`index.md`), compared caselessly.
    Name(&'static str),
    /// Any file with this stem and an extension (`README.*`), caselessly,
    /// in lexical order.
    Stem(&'static str),
}

/// When directories count as link targets of Markdown and HTML links.
/// Wikilinks and embeds name notes, so for them folders always come
/// [`AfterFiles`](Self::AfterFiles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryTargets {
    /// Like files, in candidate order (GitHub: a link to a folder opens it).
    InOrder,
    /// Only after every file lookup (including the basename lookup) failed.
    /// Obsidian links only resolve to files; Glyphvane then reveals a
    /// matching folder instead of reporting it unresolved.
    /// ORACLE-PENDING: "files only" in Obsidian (the oracle reports `null`
    /// for folder links, where Glyphvane reveals the folder).
    AfterFiles,
}

/// The order of the target and display slots in a wikilink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeOrder {
    /// `[[target|display]]` (Obsidian, Extended).
    TargetFirst,
    /// `[[display|target]]` (GitLab, Gollum).
    DisplayFirst,
}

/// How wiki page names map to file names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageNameMapping {
    /// The name is the file name (Obsidian).
    Literal,
    /// Spaces become `_`.
    SpacesToUnderscore,
    /// Spaces become `-`.
    SpacesToHyphen,
}

/// Per-vault settings the resolver needs besides the profile.
#[derive(Debug, Clone, Default)]
pub struct ResolveSettings {
    /// The vault name, for `obsidian://open?vault=…` links to this vault.
    pub vault_name: Option<String>,
    /// The git repository the vault is in, for repository URL mapping.
    pub repo: Option<Arc<RepoContext>>,
    /// Map repository URLs (`github.com/o/r/blob/…`) to local files.
    pub map_repo_urls: bool,
    /// Allow embeds of targets outside the root (default: blocked).
    pub outside_root_embeds: bool,
}

/// The link-resolution parameters of one profile.
#[derive(Debug, Clone)]
pub struct ProfileParams {
    /// Which profile these are.
    pub profile: ProfileId,
    /// What `/x` resolves against, relative to the index root.
    pub link_root: VaultPath,
    /// Bare-path order for Markdown and HTML links.
    pub markdown_order: Vec<BaseStep>,
    /// Bare-path order for wikilinks and embeds.
    pub wikilink_order: Vec<BaseStep>,
    /// Extensions appended (never substituted), with the dot, in order.
    pub appended_extensions: Vec<String>,
    /// Quality of a match that needed an appended extension.
    pub appended_extension_quality: MatchQuality,
    /// Quality of a match found only by case folding or NFC.
    pub case_only_quality: MatchQuality,
    /// Directory index files, in order.
    pub index_files: Vec<IndexFile>,
    /// When folders are targets.
    pub directory_targets: DirectoryTargets,
    /// Tie-break order for basename lookups.
    pub tie_break: TieBreak,
    /// Wikilink slot order.
    pub pipe_order: PipeOrder,
    /// Wiki page-name mapping.
    pub page_names: PageNameMapping,
    /// Wikilink subpaths and Markdown fragments are `#A#B` chains.
    pub fragment_chains: bool,
    /// How headings are named in fragments: the primary slug algorithm, or
    /// heading text. Matching itself is done by a
    /// [`FragmentResolver`](crate::FragmentResolver) (the document model's
    /// [`ModelFragments`](crate::model::ModelFragments)).
    pub anchoring: Anchoring,
    /// Vault settings.
    pub settings: ResolveSettings,
}

impl ProfileParams {
    /// The parameters of a built-in profile, with default settings.
    #[must_use]
    pub fn for_profile(profile: ProfileId) -> Self {
        let relative = vec![BaseStep::Relative];
        let md = vec![".md".to_owned()];
        match profile {
            ProfileId::CommonMark => Self {
                profile,
                link_root: VaultPath::root(),
                markdown_order: relative.clone(),
                wikilink_order: relative,
                appended_extensions: md,
                appended_extension_quality: MatchQuality::Loose,
                case_only_quality: MatchQuality::CaseFold,
                index_files: Vec::new(),
                directory_targets: DirectoryTargets::InOrder,
                tie_break: TieBreak::Standard,
                pipe_order: PipeOrder::TargetFirst,
                page_names: PageNameMapping::Literal,
                fragment_chains: false,
                anchoring: Anchoring::Slug(SlugAlgorithm::GitHub),
                settings: ResolveSettings::default(),
            },
            ProfileId::GfmFile | ProfileId::GfmComment => Self {
                profile,
                index_files: vec![IndexFile::Name("README.md"), IndexFile::Stem("README")],
                anchoring: if profile == ProfileId::GfmFile {
                    Anchoring::Slug(SlugAlgorithm::GitHub)
                } else {
                    Anchoring::None
                },
                settings: ResolveSettings {
                    map_repo_urls: true,
                    ..ResolveSettings::default()
                },
                ..Self::for_profile(ProfileId::CommonMark)
            },
            ProfileId::Obsidian => {
                let order = vec![BaseStep::VaultRoot, BaseStep::Relative, BaseStep::Basename];
                Self {
                    profile,
                    link_root: VaultPath::root(),
                    markdown_order: order.clone(),
                    wikilink_order: order,
                    appended_extensions: md,
                    appended_extension_quality: MatchQuality::Exact,
                    case_only_quality: MatchQuality::Exact,
                    index_files: Vec::new(),
                    directory_targets: DirectoryTargets::AfterFiles,
                    tie_break: TieBreak::Standard,
                    pipe_order: PipeOrder::TargetFirst,
                    page_names: PageNameMapping::Literal,
                    // ORACLE-PENDING: Markdown-link fragments chain like
                    // wikilink subpaths (`Note.md#A#B`).
                    fragment_chains: true,
                    anchoring: Anchoring::HeadingText,
                    settings: ResolveSettings::default(),
                }
            }
            ProfileId::Extended => Self {
                profile,
                link_root: VaultPath::root(),
                markdown_order: vec![BaseStep::Relative],
                wikilink_order: vec![BaseStep::Relative, BaseStep::VaultRoot, BaseStep::Basename],
                appended_extensions: md,
                appended_extension_quality: MatchQuality::Exact,
                case_only_quality: MatchQuality::CaseFold,
                index_files: vec![IndexFile::Name("README.md"), IndexFile::Name("index.md")],
                directory_targets: DirectoryTargets::InOrder,
                tie_break: TieBreak::Standard,
                pipe_order: PipeOrder::TargetFirst,
                page_names: PageNameMapping::Literal,
                fragment_chains: false,
                anchoring: Anchoring::Slug(SlugAlgorithm::GitHub),
                settings: ResolveSettings {
                    map_repo_urls: true,
                    ..ResolveSettings::default()
                },
            },
        }
    }

    /// Sets the vault settings.
    #[must_use]
    pub fn with_settings(mut self, settings: ResolveSettings) -> Self {
        self.settings = settings;
        self
    }
}
