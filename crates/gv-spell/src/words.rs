// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! User word lists: plain text, one word per line.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// A word list kept in a file (the global user dictionary, or a vault's).
/// Lines are trimmed; empty lines and lines starting with `#` are skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WordList {
    path: Option<PathBuf>,
    words: BTreeSet<String>,
}

impl WordList {
    /// A list kept in memory only.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// The list in `path`; empty when the file does not exist yet. Other
    /// read errors are returned, so a list that cannot be read is never
    /// overwritten.
    pub fn load(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let words = match std::fs::read(&path) {
            Ok(bytes) => parse(&String::from_utf8_lossy(&bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            path: Some(path),
            words,
        })
    }

    /// Where the list is kept.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn contains(&self, word: &str) -> bool {
        self.words.contains(word)
    }

    pub fn words(&self) -> impl Iterator<Item = &str> {
        self.words.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Adds `word` and writes the list (atomically: a temporary file
    /// renamed over it). Returns whether the word was new.
    pub fn add(&mut self, word: &str) -> std::io::Result<bool> {
        let word = word.trim();
        if word.is_empty() || word.contains(['\n', '\r']) || !self.words.insert(word.to_owned()) {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    /// Removes `word` and writes the list. Returns whether it was there.
    pub fn remove(&mut self, word: &str) -> std::io::Result<bool> {
        if !self.words.remove(word) {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let mut text = String::new();
        for word in &self.words {
            text.push_str(word);
            text.push('\n');
        }
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        file.write_all(text.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}

fn parse(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_persist() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sub").join("words.txt");
        let mut list = WordList::load(&path).expect("missing is empty");
        assert!(list.is_empty());
        assert!(list.add("Glyphvane").expect("saved"));
        assert!(!list.add("Glyphvane").expect("no change"));
        assert!(!list.add("  ").expect("no change"));
        assert!(list.add("zettel").expect("saved"));
        let again = WordList::load(&path).expect("reads");
        assert_eq!(again.words().collect::<Vec<_>>(), ["Glyphvane", "zettel"]);
        assert_eq!(
            std::fs::read_to_string(&path).expect("file"),
            "Glyphvane\nzettel\n"
        );
        let mut again = again;
        assert!(again.remove("zettel").expect("saved"));
        assert_eq!(WordList::load(&path).expect("reads").len(), 1);
        std::fs::write(&path, "# mine\n\n  spaced  \r\nword\n").expect("write");
        let edited = WordList::load(&path).expect("reads");
        assert_eq!(edited.words().collect::<Vec<_>>(), ["spaced", "word"]);
    }
}
