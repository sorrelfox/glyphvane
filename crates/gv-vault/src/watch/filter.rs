// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Which paths the watcher ignores: the same rules as the walk
//! ([`IndexConfig`]), applied to single paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ignore::gitignore::Gitignore;

use crate::index::IndexConfig;
use crate::path::VaultPath;

/// The marker in the temporary file names of the save pipeline
/// (`.<name>.gv-tmp-<random>`), never indexed.
const OWN_TEMP_MARKER: &str = ".gv-tmp-";

/// The walk's exclusion rules for single paths.
#[derive(Debug)]
pub(crate) struct PathFilter {
    root: PathBuf,
    exclude_names: Vec<String>,
    include_hidden: bool,
    respect_gitignore: bool,
    /// `.gitignore` of each directory asked about (empty when it has none).
    gitignores: HashMap<PathBuf, Gitignore>,
    /// `.git/info/exclude` of the root, loaded on first use.
    git_exclude: Option<Gitignore>,
}

impl PathFilter {
    pub(crate) fn new(root: &Path, config: &IndexConfig) -> Self {
        Self {
            root: root.to_path_buf(),
            exclude_names: config.exclude_names.clone(),
            include_hidden: config.include_hidden,
            respect_gitignore: config.respect_gitignore,
            gitignores: HashMap::new(),
            git_exclude: None,
        }
    }

    /// Whether ignore files take part (a change to one changes the rules).
    pub(crate) fn uses_gitignore(&self) -> bool {
        self.respect_gitignore
    }

    /// Forgets the cached ignore files (one of them changed).
    pub(crate) fn reload_rules(&mut self) {
        self.gitignores.clear();
        self.git_exclude = None;
    }

    /// Whether the walk would skip `path`, which is a directory if `is_dir`.
    /// A path below a skipped directory is skipped too.
    pub(crate) fn excluded(&mut self, path: &VaultPath, is_dir: bool) -> bool {
        if path.is_root() {
            return false;
        }
        let segments: Vec<&str> = path.segments().collect();
        let last = segments.len() - 1;
        for (i, seg) in segments.iter().enumerate() {
            let seg_is_dir = i < last || is_dir;
            if seg.starts_with('.') && seg.contains(OWN_TEMP_MARKER) {
                return true;
            }
            if !self.include_hidden && seg.starts_with('.') {
                return true;
            }
            if self.exclude_names.iter().any(|x| x == seg) && (seg_is_dir || *seg == ".git") {
                return true;
            }
        }
        self.respect_gitignore && self.git_ignored(&segments, is_dir)
    }

    /// `.gitignore` rules: the deepest file that matches decides, and a
    /// path inside an ignored directory is ignored.
    fn git_ignored(&mut self, segments: &[&str], is_dir: bool) -> bool {
        let last = segments.len() - 1;
        for i in 0..segments.len() {
            let prefix: PathBuf = segments[..=i].iter().collect();
            let abs = self.root.join(&prefix);
            let prefix_is_dir = i < last || is_dir;
            let mut decided = false;
            for j in (0..=i).rev() {
                let dir: PathBuf = self.root.join(segments[..j].iter().collect::<PathBuf>());
                let m = self.gitignore(&dir).matched(&abs, prefix_is_dir);
                if m.is_ignore() {
                    return true;
                }
                if m.is_whitelist() {
                    decided = true;
                    break;
                }
            }
            if !decided && self.exclude_file().matched(&abs, prefix_is_dir).is_ignore() {
                return true;
            }
        }
        false
    }

    fn gitignore(&mut self, dir: &Path) -> &Gitignore {
        self.gitignores
            .entry(dir.to_path_buf())
            .or_insert_with(|| load(&dir.join(".gitignore")))
    }

    fn exclude_file(&mut self) -> &Gitignore {
        let root = &self.root;
        self.git_exclude
            .get_or_insert_with(|| load(&root.join(".git").join("info").join("exclude")))
    }
}

/// An ignore file; empty when it is missing or unreadable. Lines that do
/// not parse are skipped, as the walk does.
fn load(file: &Path) -> Gitignore {
    if !file.is_file() {
        return Gitignore::empty();
    }
    Gitignore::new(file).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(s: &str) -> VaultPath {
        VaultPath::new(s).expect("valid path")
    }

    #[test]
    fn default_rules() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut f = PathFilter::new(dir.path(), &IndexConfig::default());
        assert!(!f.excluded(&vp("a/b.md"), false));
        assert!(f.excluded(&vp("node_modules"), true));
        assert!(f.excluded(&vp("node_modules/x.md"), false));
        assert!(f.excluded(&vp("a/.git/HEAD"), false));
        assert!(f.excluded(&vp(".hidden.md"), false));
        assert!(f.excluded(&vp("a/.Note.md.gv-tmp-12ab"), false));
        // A file named like an excluded folder is kept (except `.git`).
        let mut repo = PathFilter::new(dir.path(), &IndexConfig::git_repo());
        assert!(!repo.excluded(&vp("target"), false));
        assert!(repo.excluded(&vp("target"), true));
        assert!(repo.excluded(&vp(".git"), false));
        assert!(!repo.excluded(&vp(".github/x.md"), false));
        assert!(repo.excluded(&vp(".x.md.gv-tmp-1"), false));
    }

    #[test]
    fn gitignore_rules() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join(".gitignore"), "build/\n*.log\n").expect("write");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub/.gitignore"), "!keep.log\n").expect("write");
        let mut f = PathFilter::new(dir.path(), &IndexConfig::git_repo());
        assert!(f.excluded(&vp("build"), true));
        assert!(f.excluded(&vp("build/a.md"), false));
        assert!(f.excluded(&vp("x.log"), false));
        assert!(f.excluded(&vp("sub/x.log"), false));
        assert!(!f.excluded(&vp("sub/keep.log"), false));
        assert!(!f.excluded(&vp("sub/a.md"), false));
    }
}
