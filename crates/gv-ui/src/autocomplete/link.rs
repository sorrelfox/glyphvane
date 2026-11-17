// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The links an accepted completion writes, in the vault's link style
//! (09 § Link generation).
//!
//! - **Style.** A vault with `.obsidian/app.json` takes `useMarkdownLinks`
//!   and `newLinkFormat` from it (a missing key is Obsidian's default:
//!   wikilinks, shortest); otherwise the `links.use_markdown_links` and
//!   `links.new_link_format` settings. `links.markdown_link_encoding`
//!   chooses `%20` or the `<…>` form for Markdown destinations.
//! - **Paths.** Wikilinks leave `.md` off notes; Markdown links keep the
//!   extension. The candidates of the chosen form are tried in order and
//!   the first that resolves to exactly the target, letter case included,
//!   is written (`resolve(generate(target)) == target`); an ambiguous
//!   shortest form lengthens to the vault path.
//! - **Fragments.** Wikilinks write the heading text (without the
//!   characters a wikilink cannot hold) and `#^id`; Markdown links write
//!   the percent-encoded heading text in Obsidian and GitHub slugs
//!   elsewhere.

use std::path::Path;

use gv_settings::{LinkEncoding, NewLinkFormat};
use gv_vault::resolve::{Note, Status, resolve};
use gv_vault::{LinkKind, LinkRef, PathIndex, ProfileId, ProfileParams, VaultPath};

/// How generated links are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LinkStyle {
    /// Markdown links instead of wikilinks.
    pub markdown: bool,
    pub format: NewLinkFormat,
    /// Markdown destinations in `<…>` instead of percent-encoded.
    pub angle: bool,
}

impl Default for LinkStyle {
    fn default() -> Self {
        Self {
            markdown: false,
            format: NewLinkFormat::Shortest,
            angle: false,
        }
    }
}

impl LinkStyle {
    /// The style of the vault at `root`: its Obsidian settings when it has
    /// `.obsidian/app.json`, else `settings`.
    pub fn for_vault(root: Option<&Path>, settings: Option<&gv_settings::Links>) -> Self {
        let angle =
            settings.is_some_and(|s| s.markdown_link_encoding == LinkEncoding::AngleBrackets);
        let app =
            root.and_then(|r| std::fs::read_to_string(r.join(".obsidian").join("app.json")).ok());
        if let Some(app) = app {
            let format = match json_string(&app, "newLinkFormat").as_deref() {
                Some("relative") => NewLinkFormat::Relative,
                Some("absolute") => NewLinkFormat::Absolute,
                _ => NewLinkFormat::Shortest,
            };
            return Self {
                markdown: json_bool(&app, "useMarkdownLinks").unwrap_or(false),
                format,
                angle,
            };
        }
        match settings {
            Some(s) => Self {
                markdown: s.use_markdown_links,
                format: s.new_link_format,
                angle,
            },
            None => Self::default(),
        }
    }
}

/// The raw value after `"key":` in a flat JSON object.
fn json_raw<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let quoted = format!("\"{key}\"");
    let at = text.find(&quoted)? + quoted.len();
    Some(text[at..].trim_start().strip_prefix(':')?.trim_start())
}

/// The boolean value of `"key"` in a flat JSON object.
fn json_bool(text: &str, key: &str) -> Option<bool> {
    let rest = json_raw(text, key)?;
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// The string value of `"key"` in a flat JSON object (`\"` and `\\`
/// unescaped).
fn json_string(text: &str, key: &str) -> Option<String> {
    let rest = json_raw(text, key)?.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

/// Writes links from one document.
#[derive(Debug, Clone)]
pub(crate) struct Linker<'a> {
    pub index: Option<&'a PathIndex>,
    pub params: &'a ProfileParams,
    /// The document, vault-relative.
    pub source: &'a VaultPath,
    pub profile: ProfileId,
    pub style: LinkStyle,
}

impl Linker<'_> {
    /// Whether a link of `kind` written `text` resolves to exactly
    /// `target`. Without an index nothing can be checked: every text is
    /// taken.
    fn resolves(&self, kind: LinkKind, text: &str, target: &VaultPath) -> bool {
        let Some(index) = self.index else {
            return true;
        };
        let link = LinkRef::new(kind, text, self.source.clone(), self.profile);
        let r = resolve(index, &link, self.params);
        r.status == Status::Exact
            && !r.notes.contains(&Note::CaseDiffers)
            && r.target_path.as_ref() == Some(target)
    }

    /// The first candidate that resolves to `target`, else the last one.
    fn pick(&self, kind: LinkKind, candidates: Vec<String>, target: &VaultPath) -> String {
        let mut last = String::new();
        for candidate in candidates {
            // The resolver percent-decodes Markdown destinations itself.
            if self.resolves(kind, &candidate, target) {
                return candidate;
            }
            last = candidate;
        }
        last
    }

    /// The path part of a wikilink to `target`: no `.md` on notes.
    pub fn wiki_path(&self, target: &VaultPath) -> String {
        let segments: Vec<&str> = target.segments().collect();
        let strip = |text: String| match text.strip_suffix(".md") {
            Some(stem) if is_note(target) => stem.to_owned(),
            _ => text,
        };
        let full = strip(segments.join("/"));
        let mut candidates = Vec::new();
        match self.style.format {
            NewLinkFormat::Shortest => {
                for n in 1..=segments.len() {
                    candidates.push(strip(segments[segments.len() - n..].join("/")));
                }
            }
            NewLinkFormat::Relative => {
                candidates.push(strip(relative_parts(self.source, target).join("/")));
                candidates.push(full.clone());
            }
            NewLinkFormat::Absolute => candidates.push(full.clone()),
        }
        candidates.push(full);
        self.pick(LinkKind::Wikilink, candidates, target)
    }

    /// The destination of a Markdown link to `target`, encoded per the
    /// style (`%20`, or `<…>` when it needs it).
    pub fn markdown_dest(&self, target: &VaultPath) -> String {
        let segments: Vec<String> = target.segments().map(str::to_owned).collect();
        let relative = relative_parts(self.source, target);
        let suffix = |n: usize| segments[segments.len() - n..].to_vec();
        let mut parts: Vec<Vec<String>> = Vec::new();
        match self.style.format {
            NewLinkFormat::Shortest => {
                for n in 1..=segments.len() {
                    parts.push(suffix(n));
                }
                parts.push(relative);
            }
            NewLinkFormat::Relative => parts.push(relative),
            NewLinkFormat::Absolute => parts.push(segments.clone()),
        }
        let mut candidates: Vec<String> = parts.iter().map(|p| self.join(p, false)).collect();
        candidates.push(self.join(&segments, true));
        let raw = self.pick(LinkKind::Inline, candidates, target);
        if self.style.angle && raw.contains([' ', '(', ')']) {
            format!("<{raw}>")
        } else {
            raw
        }
    }

    /// Path segments joined with `/` (a leading `/` when `rooted`),
    /// percent-encoded unless the style writes `<…>`.
    fn join(&self, parts: &[String], rooted: bool) -> String {
        let joined = parts
            .iter()
            .map(|s| {
                if self.style.angle {
                    s.clone()
                } else {
                    encode(s)
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        if rooted { format!("/{joined}") } else { joined }
    }
}

/// Whether `path` is a Markdown note (its wikilinks leave `.md` off).
pub(crate) fn is_note(path: &VaultPath) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

/// Percent-encodes what a Markdown destination cannot hold as is.
pub(crate) fn encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for c in segment.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            '%' => out.push_str("%25"),
            '[' => out.push_str("%5B"),
            ']' => out.push_str("%5D"),
            _ => out.push(c),
        }
    }
    out
}

/// The segments of `to` relative to the folder of `from` (`..` first).
fn relative_parts(from: &VaultPath, to: &VaultPath) -> Vec<String> {
    let mut from_dir: Vec<&str> = from.segments().collect();
    from_dir.pop();
    let to_segments: Vec<&str> = to.segments().collect();
    let common = from_dir
        .iter()
        .zip(&to_segments)
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = vec!["..".into(); from_dir.len() - common];
    parts.extend(to_segments[common..].iter().map(|s| (*s).to_owned()));
    parts
}

/// A heading as a wikilink writes it: without `[`, `]`, `|`, `#`, `^`.
pub(crate) fn wiki_heading(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if matches!(c, '[' | ']' | '|' | '#' | '^') {
                ' '
            } else {
                c
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A heading as a Markdown link's fragment: the percent-encoded text in
/// Obsidian, the GitHub slug elsewhere.
pub(crate) fn heading_fragment(profile: ProfileId, text: &str) -> String {
    if profile == ProfileId::Obsidian {
        encode(&wiki_heading(text))
    } else {
        gv_model::slug::SlugAlgorithm::GitHub.slug(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(s: &str) -> VaultPath {
        VaultPath::new(s).expect("path")
    }

    fn linker<'a>(
        index: &'a PathIndex,
        params: &'a ProfileParams,
        source: &'a VaultPath,
        style: LinkStyle,
    ) -> Linker<'a> {
        Linker {
            index: Some(index),
            params,
            source,
            profile: params.profile,
            style,
        }
    }

    #[test]
    fn wikilinks_are_shortest_unless_ambiguous() {
        let index = PathIndex::from_paths([
            "Home.md",
            "a/Plan.md",
            "b/Plan.md",
            "a/Only.md",
            "img/p q.png",
        ])
        .expect("index");
        let params = ProfileParams::for_profile(ProfileId::Obsidian);
        let source = vp("Home.md");
        let l = linker(&index, &params, &source, LinkStyle::default());
        assert_eq!(l.wiki_path(&vp("a/Only.md")), "Only");
        assert_eq!(l.wiki_path(&vp("img/p q.png")), "p q.png");
        // Two notes named Plan: lengthened until it is exact.
        assert_eq!(l.wiki_path(&vp("b/Plan.md")), "b/Plan");
        let absolute = LinkStyle {
            format: NewLinkFormat::Absolute,
            ..LinkStyle::default()
        };
        let l = linker(&index, &params, &source, absolute);
        assert_eq!(l.wiki_path(&vp("a/Only.md")), "a/Only");
    }

    #[test]
    fn markdown_destinations_are_encoded_and_checked() {
        let index = PathIndex::from_paths(["notes/Home.md", "notes/My Note.md", "x/y/Far.md"])
            .expect("index");
        let params = ProfileParams::for_profile(ProfileId::GfmFile);
        let source = vp("notes/Home.md");
        let relative = LinkStyle {
            markdown: true,
            format: NewLinkFormat::Relative,
            angle: false,
        };
        let l = linker(&index, &params, &source, relative);
        assert_eq!(l.markdown_dest(&vp("notes/My Note.md")), "My%20Note.md");
        assert_eq!(l.markdown_dest(&vp("x/y/Far.md")), "../x/y/Far.md");
        let angle = LinkStyle {
            angle: true,
            ..relative
        };
        let l = linker(&index, &params, &source, angle);
        assert_eq!(l.markdown_dest(&vp("notes/My Note.md")), "<My Note.md>");
        // Shortest in GitHub's relative resolution is the relative path.
        let shortest = LinkStyle {
            format: NewLinkFormat::Shortest,
            ..relative
        };
        let l = linker(&index, &params, &source, shortest);
        assert_eq!(l.markdown_dest(&vp("x/y/Far.md")), "../x/y/Far.md");
    }

    #[test]
    fn obsidian_settings_win_over_glyphvane_settings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = gv_settings::Links {
            use_markdown_links: false,
            ..gv_settings::Links::default()
        };
        let style = LinkStyle::for_vault(Some(dir.path()), Some(&settings));
        assert!(!style.markdown);
        std::fs::create_dir_all(dir.path().join(".obsidian")).expect("mkdir");
        std::fs::write(
            dir.path().join(".obsidian/app.json"),
            r#"{ "useMarkdownLinks": true, "newLinkFormat": "relative" }"#,
        )
        .expect("write");
        let style = LinkStyle::for_vault(Some(dir.path()), Some(&settings));
        assert!(style.markdown);
        assert_eq!(style.format, NewLinkFormat::Relative);
    }

    #[test]
    fn heading_fragments_follow_the_profile() {
        assert_eq!(wiki_heading("A [b] | c"), "A b c");
        assert_eq!(
            heading_fragment(ProfileId::Obsidian, "Two Words"),
            "Two%20Words"
        );
        assert_eq!(
            heading_fragment(ProfileId::GfmFile, "Two Words!"),
            "two-words"
        );
    }
}
