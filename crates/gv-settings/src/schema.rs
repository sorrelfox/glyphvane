// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The settings schema: every section and setting, with its default.
//!
//! Each setting is declared once. The declaration produces the typed struct
//! field (with serde), the default, and the registry entry used for
//! validation, layering and the JSON Schema. The `@…` markers before a field
//! narrow its range or mark it security-relevant.

use serde::{Deserialize, Serialize};

use crate::spec::{Kind, Section, SettingType, Spec, Tighten};

/// Declares an enum whose values are kebab-case strings in TOML.
macro_rules! setting_enum {
    (
        $(#[doc = $doc:literal])*
        $Name:ident {
            $(
                $(#[doc = $vdoc:literal])*
                $Variant:ident = $text:literal,
            )+
        }
    ) => {
        $(#[doc = $doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $Name {
            $(
                $(#[doc = $vdoc])*
                #[serde(rename = $text)]
                $Variant,
            )+
        }

        impl $Name {
            /// The TOML strings, in declaration order. For security-relevant
            /// settings this runs from the least to the most strict value.
            pub const VARIANTS: &'static [&'static str] = &[$($text),+];

            /// The value as written in TOML.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$Variant => $text,)+
                }
            }
        }

        impl SettingType for $Name {
            const KIND: Kind = Kind::Enum(Self::VARIANTS);
        }
    };
}

/// Declares the sections, their structs and the registry.
macro_rules! define_settings {
    (
        $(
            $(#[doc = $sdoc:literal])*
            $section:ident: $Section:ident {
                $(
                    $(#[doc = $doc:literal])*
                    $(@$meta:ident($($arg:expr),*))*
                    $field:ident: $ty:ty = $default:expr,
                )*
            }
        )*
    ) => {
        $(
            $(#[doc = $sdoc])*
            #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
            #[serde(default)]
            pub struct $Section {
                $(
                    $(#[doc = $doc])*
                    pub $field: $ty,
                )*
            }

            impl Default for $Section {
                fn default() -> Self {
                    Self {
                        $($field: $default,)*
                    }
                }
            }
        )*

        /// The effective settings, typed. Built from the layers by
        /// [`crate::resolve`]; every value has been validated.
        #[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
        #[serde(default)]
        pub struct Settings {
            $(
                $(#[doc = $sdoc])*
                pub $section: $Section,
            )*
        }

        pub(crate) fn build_registry() -> (Vec<Section>, Vec<Spec>) {
            let mut sections = Vec::new();
            let mut specs = Vec::new();
            $(
                sections.push(Section {
                    name: stringify!($section),
                    doc: join_doc(&[$($sdoc),*]),
                });
                let defaults = $Section::default();
                $(
                    specs.push(
                        Spec::new(
                            stringify!($section),
                            stringify!($field),
                            concat!(stringify!($section), ".", stringify!($field)),
                            &[$($doc),*],
                            <$ty as SettingType>::KIND,
                            default_value(&defaults.$field),
                        )
                        $(.$meta($($arg),*))*
                    );
                )*
            )*
            (sections, specs)
        }
    };
}

fn join_doc(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn default_value<T: Serialize>(value: &T) -> toml::Value {
    // The defaults are plain scalars, strings and string lists, which always
    // serialize; the schema test checks every one.
    toml::Value::try_from(value).unwrap_or(toml::Value::Boolean(false))
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

setting_enum! {
    /// Which view a file opened from the operating system starts in.
    OpenFilesIn {
        /// The rendered Reading view.
        Reading = "reading",
        /// The Source editor.
        Source = "source",
        /// Whichever view was used last.
        LastUsed = "last-used",
    }
}

setting_enum! {
    /// The theme setting. `System` follows the operating system's appearance.
    ThemeChoice {
        /// Dark in dark appearance, the preferred light theme otherwise.
        System = "system",
        /// Always Light.
        Light = "light",
        /// Always Semi-light, the sand-tinted light theme.
        SemiLight = "semi-light",
        /// Always Dark.
        Dark = "dark",
    }
}

setting_enum! {
    /// The light theme used by [`ThemeChoice::System`] in light appearance.
    PreferredLightTheme {
        /// Light.
        Light = "light",
        /// Semi-light.
        SemiLight = "semi-light",
    }
}

setting_enum! {
    /// When the high-contrast themes are used.
    HighContrast {
        /// When the operating system asks for high contrast.
        FollowSystem = "follow-system",
        /// Always.
        Always = "always",
        /// Never.
        Never = "never",
    }
}

setting_enum! {
    /// What the `:` emoji completion inserts.
    EmojiInsert {
        /// The shortcode, such as `:smile:`.
        Shortcode = "shortcode",
        /// The Unicode character.
        Character = "character",
    }
}

setting_enum! {
    /// Where "Insert footnote" puts the definition.
    FootnotePlacement {
        /// At the end of the document.
        EndOfDocument = "end-of-document",
        /// After the current paragraph.
        AfterParagraph = "after-paragraph",
    }
}

setting_enum! {
    /// The path form of generated links.
    NewLinkFormat {
        /// The shortest unambiguous path.
        Shortest = "shortest",
        /// Relative to the linking note.
        Relative = "relative",
        /// Absolute from the vault root.
        Absolute = "absolute",
    }
}

setting_enum! {
    /// How generated Markdown links escape spaces and reserved characters.
    LinkEncoding {
        /// Percent-encoding, such as `%20`.
        Percent = "percent",
        /// The `<…>` destination form.
        AngleBrackets = "angle-brackets",
    }
}

setting_enum! {
    /// What happens to files dropped or pasted from outside the vault.
    ExternalFiles {
        /// Copy into the attachment folder.
        Copy = "copy",
        /// Link to the file where it is.
        Link = "link",
        /// Ask each time.
        Ask = "ask",
    }
}

setting_enum! {
    /// A built-in flavor profile.
    ProfileId {
        /// CommonMark 0.31.2 without extensions.
        Commonmark = "commonmark",
        /// GitHub Flavored Markdown as github.com renders files.
        GfmFile = "gfm-file",
        /// GitHub Flavored Markdown as github.com renders comments.
        GfmComment = "gfm-comment",
        /// Obsidian Flavored Markdown.
        Obsidian = "obsidian",
        /// The Extended superset.
        Extended = "extended",
    }
}

setting_enum! {
    /// A profile override: `auto` keeps detection.
    ProfileOverride {
        /// Detect the profile.
        Auto = "auto",
        /// CommonMark.
        Commonmark = "commonmark",
        /// GFM (files).
        GfmFile = "gfm-file",
        /// GFM (comments).
        GfmComment = "gfm-comment",
        /// Obsidian.
        Obsidian = "obsidian",
        /// Extended.
        Extended = "extended",
    }
}

setting_enum! {
    /// The remote-content policy. Declared from the least to the most strict.
    RemoteContent {
        /// Load remote images and media, except blocked sites and private or
        /// loopback hosts, which always ask.
        Allow = "allow",
        /// Load nothing remote until the user approves.
        Ask = "ask",
        /// Never load anything remote.
        Block = "block",
    }
}

setting_enum! {
    /// Workspace trust. Declared from the least to the most strict.
    WorkspaceTrust {
        /// The vault is trusted and uses its remote-content mode.
        Trusted = "trusted",
        /// Show the trust prompt (the default for newly opened folders).
        Ask = "ask",
        /// Restricted mode: remote content blocked, every external hand-off
        /// confirmed, plugin highlighters and CSS snippets ignored.
        Restricted = "restricted",
    }
}

define_settings! {
    /// Start-up, view modes and updates.
    general: General {
        /// The view that files opened from the operating system (file manager,
        /// `open`, the command line) start in. `--wait` always opens Source.
        open_files_in: OpenFilesIn = OpenFilesIn::Reading,
        /// Viewer mode: documents are read-only until editing is switched on.
        viewer_mode: bool = false,
        /// Check GitHub Releases for a new version, at most once a day. Opt-in.
        @global_only()
        check_for_updates: bool = false,
    }

    /// Theme, fonts, line width and zoom.
    appearance: Appearance {
        /// The theme. `system` uses Dark in dark appearance and the preferred
        /// light theme otherwise.
        theme: ThemeChoice = ThemeChoice::System,
        /// The light theme that `system` uses in light appearance.
        preferred_light_theme: PreferredLightTheme = PreferredLightTheme::Light,
        /// When the high-contrast themes are used.
        high_contrast: HighContrast = HighContrast::FollowSystem,
        /// The interface font family.
        interface_font: String = "Inter".to_owned(),
        /// The interface font size in pixels.
        @range(8, 32)
        interface_font_size: f32 = 12.0,
        /// The Reading view font family.
        preview_font: String = "Inter".to_owned(),
        /// The Reading view font size in pixels.
        @range(8, 48)
        preview_font_size: f32 = 15.0,
        /// The editor font family. A monospace font keeps Markdown columns and
        /// tables aligned.
        editor_font: String = "JetBrains Mono".to_owned(),
        /// The editor font size in pixels.
        @range(8, 48)
        editor_font_size: f32 = 12.5,
        /// Keep text in a centred column in the Reading view and the editor.
        readable_line_width: bool = true,
        /// The readable column width in characters.
        @range(40, 240)
        readable_line_width_chars: u32 = 72,
        /// With a readable line width, tables and diagrams too wide for the
        /// text column use all the space beside it in the Reading view.
        fill_wide_blocks: bool = true,
        /// How wide, in characters, tables and diagrams too wide for the text
        /// column get when they do not use all the space beside it.
        @range(40, 400)
        wide_block_width_chars: u32 = 120,
        /// The zoom level in percent.
        @range(50, 300)
        zoom_percent: u32 = 100,
        /// The code-block theme: `auto` matches the app theme, otherwise the
        /// name of a bundled code theme. The editor and the Reading view use
        /// the same colours.
        code_theme: String = "auto".to_owned(),
    }

    /// Editing behaviour.
    editor: Editor {
        /// Use the proportional prose font in the editor. Table alignment is
        /// then approximate.
        prose_font: bool = false,
        /// Render heading lines larger in the editor.
        larger_headings: bool = false,
        /// Seconds of idle time after which the caret stops blinking.
        @range(1, 3600)
        caret_blink_idle_secs: u32 = 10,
        /// Wrap a pasted URL as `<url>` when nothing is selected.
        wrap_pasted_urls: bool = false,
        /// What the `:` emoji completion inserts.
        emoji: EmojiInsert = EmojiInsert::Shortcode,
        /// Where "Insert footnote" puts the definition.
        footnote_definitions: FootnotePlacement = FootnotePlacement::EndOfDocument,
        /// Clicking in the Reading view moves the Source caret to the matching
        /// place.
        sync_caret_from_reading: bool = true,
        /// Leave front matter out of the word and character counts.
        statistics_exclude_front_matter: bool = false,
        /// Leave code out of the word and character counts.
        statistics_exclude_code: bool = false,
        /// Leave comments out of the word and character counts.
        statistics_exclude_comments: bool = false,
        /// Large-file mode starts above this file size in bytes.
        @range(1024, 1_073_741_824)
        large_file_bytes: u64 = 5 * 1024 * 1024,
        /// Large-file mode starts when a line is longer than this many
        /// characters.
        @range(1000, 100_000_000)
        large_file_line_chars: u32 = 100_000,
        /// Large-file mode starts above this many lines.
        @range(1000, 100_000_000)
        large_file_lines: u32 = 200_000,
    }

    /// The Reading view.
    preview: Preview {
        /// Delay before a link's hover preview opens, in milliseconds.
        @range(0, 5000)
        hover_preview_delay_ms: u32 = 300,
        /// The deepest embed nesting rendered.
        @range(1, 20)
        embed_max_depth: u32 = 5,
        /// The most rendered nodes in one top-level embed tree.
        @range(100, 1_000_000)
        embed_max_nodes: u32 = 10_000,
        /// The most source bytes in one top-level embed tree.
        @range(1024, 268_435_456)
        embed_max_bytes: u64 = 2 * 1024 * 1024,
    }

    /// Link generation and following. Vaults with an Obsidian config use its
    /// link style instead.
    links: Links {
        /// Generate Markdown links instead of wikilinks.
        use_markdown_links: bool = false,
        /// The path form of generated links.
        new_link_format: NewLinkFormat = NewLinkFormat::Shortest,
        /// How generated Markdown links escape spaces and reserved characters.
        markdown_link_encoding: LinkEncoding = LinkEncoding::Percent,
        /// Map web URLs that point into the vault's repository to local files.
        /// Nothing is fetched either way.
        map_web_urls_to_files: bool = true,
        /// Ask once per site before opening a web link in the browser.
        @security(Tighten::TrueIsStricter)
        confirm_web_links: bool = false,
        /// Back and forward history entries kept per tab.
        @range(1, 1000)
        history_size: u32 = 100,
    }

    /// Saving and attachments.
    files: Files {
        /// Save automatically after 1 s idle, on focus loss, on tab switch and
        /// on quit.
        autosave: bool = true,
        /// Where new attachments go: `/` (vault root), `./` (the note's
        /// folder), `./sub` (a subfolder of the note's folder) or a
        /// vault-relative folder. An Obsidian config's value takes precedence.
        attachment_folder: String = "/".to_owned(),
        /// What happens to files dropped or pasted from outside the vault.
        external_files: ExternalFiles = ExternalFiles::Ask,
    }

    /// Vault detection and watching.
    vault: Vault {
        /// Extra Obsidian config-folder names, besides `.obsidian`.
        obsidian_config_folders: Vec<String> = Vec::new(),
        /// The polling interval for network, WSL and SMB paths, in seconds.
        @range(1, 3600)
        network_poll_interval_secs: u32 = 5,
    }

    /// Flavor profiles.
    profiles: Profiles {
        /// The profile used when detection finds nothing.
        default_profile: ProfileId = ProfileId::Extended,
        /// A profile for every document, overriding detection. Usually set for
        /// one vault. `auto` keeps detection.
        vault_profile: ProfileOverride = ProfileOverride::Auto,
    }

    /// Spell check.
    spelling: Spelling {
        /// Check spelling in prose.
        enabled: bool = true,
        /// The default dictionaries, such as `en_US`. A word is correct if any
        /// of them accepts it. Front-matter `lang` overrides this per document.
        languages: Vec<String> = strings(&["en_US"]),
        /// Skip words that contain digits.
        skip_words_with_digits: bool = true,
        /// Skip words in all capitals.
        skip_all_caps: bool = true,
    }

    /// Remote content, workspace trust and local crash reports. Vault settings
    /// files can make these stricter, never looser.
    privacy_and_security: PrivacyAndSecurity {
        /// Remote images and media: `allow`, `ask` (load after approval) or
        /// `block` (never load). A global `block` is absolute: no vault can
        /// change it.
        @security(Tighten::LaterVariantIsStricter)
        @absolute_in_global("block")
        remote_content: RemoteContent = RemoteContent::Ask,
        /// Sites whose remote content loads without asking in `ask` mode.
        /// Lists from all layers are combined.
        @security(Tighten::Subset)
        @union()
        remote_allow_sites: Vec<String> = Vec::new(),
        /// Sites whose remote content never loads. Lists from all layers are
        /// combined.
        @security(Tighten::Superset)
        @union()
        remote_block_sites: Vec<String> = Vec::new(),
        /// Workspace trust. Globally: what newly opened folders get (`ask`
        /// shows the trust prompt). Per vault: the stored decision.
        @security(Tighten::LaterVariantIsStricter)
        workspace_trust: WorkspaceTrust = WorkspaceTrust::Ask,
        /// Confirm every external hand-off, not only the risky ones.
        @security(Tighten::TrueIsStricter)
        confirm_every_handoff: bool = false,
        /// Allow embeds of files outside the vault root.
        @security(Tighten::FalseIsStricter)
        allow_outside_root_embeds: bool = false,
        /// Write local crash reports. Nothing is ever sent. Opt-in.
        @global_only()
        crash_reports: bool = false,
    }
}
