// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What a folder brings that workspace trust is about (FR-SET-07): the
//! summary the trust prompt shows the first time a folder is opened.
//!
//! The scan is cheap and reads no file contents: it lists
//! `.obsidian/snippets/*.css`, the folders of `.obsidian/plugins/` and
//! `.obsidian/themes/`, and checks for the shared settings files
//! (`.glyphvane/settings.toml`, `.glyphvane.toml`). Glyphvane runs no
//! plugins and, in restricted mode, ignores CSS snippets and plugin
//! highlighters; remote content and external hand-offs are what any note
//! may trigger, so the prompt always names them.

use std::fs;
use std::path::Path;

/// What a folder holds that trust decides about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustSummary {
    /// CSS snippets in `.obsidian/snippets/`.
    pub css_snippets: usize,
    /// Obsidian community plugins (folders in `.obsidian/plugins/`).
    pub plugins: usize,
    /// Obsidian themes (folders in `.obsidian/themes/`).
    pub themes: usize,
    /// The folder has shared settings (`.glyphvane/settings.toml` or
    /// `.glyphvane.toml`), which can only make security settings stricter.
    pub shared_settings: bool,
}

impl TrustSummary {
    /// Whether the folder holds anything beyond notes.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// One sentence per thing found, for the prompt's details ("3 CSS
    /// snippets", "2 Obsidian plugins (never run)").
    pub fn lines(&self) -> Vec<String> {
        let count = |n: usize, one: &str, many: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {many}")
            }
        };
        let mut out = Vec::new();
        if self.css_snippets > 0 {
            out.push(count(self.css_snippets, "CSS snippet", "CSS snippets"));
        }
        if self.themes > 0 {
            out.push(count(self.themes, "Obsidian theme", "Obsidian themes"));
        }
        if self.plugins > 0 {
            out.push(format!(
                "{} (never run)",
                count(self.plugins, "Obsidian plugin", "Obsidian plugins")
            ));
        }
        if self.shared_settings {
            out.push("shared settings".to_owned());
        }
        out
    }
}

/// Scans the folder at `root`. Unreadable parts count as empty.
pub fn scan_trust(root: &Path) -> TrustSummary {
    let obsidian = root.join(".obsidian");
    let entries = |dir: &Path, keep: &dyn Fn(&fs::DirEntry) -> bool| -> usize {
        fs::read_dir(dir).map_or(0, |it| it.flatten().filter(|e| keep(e)).count())
    };
    let is_dir = |e: &fs::DirEntry| e.file_type().is_ok_and(|t| t.is_dir());
    let is_css = |e: &fs::DirEntry| {
        e.file_type().is_ok_and(|t| t.is_file())
            && Path::new(&e.file_name())
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("css"))
    };
    TrustSummary {
        css_snippets: entries(&obsidian.join("snippets"), &is_css),
        plugins: entries(&obsidian.join("plugins"), &is_dir),
        themes: entries(&obsidian.join("themes"), &is_dir),
        shared_settings: root.join(".glyphvane").join("settings.toml").is_file()
            || root.join(".glyphvane.toml").is_file(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_snippets_plugins_themes_and_settings() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        assert!(scan_trust(root).is_empty());
        let ob = root.join(".obsidian");
        for d in [
            "snippets",
            "plugins/dataview",
            "plugins/calendar",
            "themes/Minimal",
        ] {
            fs::create_dir_all(ob.join(d)).expect("mkdir");
        }
        fs::write(ob.join("snippets/a.css"), "").expect("write");
        fs::write(ob.join("snippets/b.CSS"), "").expect("write");
        fs::write(ob.join("snippets/notes.txt"), "").expect("write");
        fs::write(ob.join("plugins/stray.json"), "").expect("write");
        fs::create_dir(root.join(".glyphvane")).expect("mkdir");
        fs::write(root.join(".glyphvane/settings.toml"), "version = 1").expect("write");
        let s = scan_trust(root);
        assert_eq!(
            s,
            TrustSummary {
                css_snippets: 2,
                plugins: 2,
                themes: 1,
                shared_settings: true,
            }
        );
        assert_eq!(
            s.lines(),
            [
                "2 CSS snippets",
                "1 Obsidian theme",
                "2 Obsidian plugins (never run)",
                "shared settings"
            ]
        );
    }
}
