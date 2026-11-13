// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Vault-relative paths and lexical path normalization.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// A normalized path relative to the vault (index) root.
///
/// Invariants, checked by every constructor:
/// - segments are separated by `/` (on every OS);
/// - no leading or trailing `/`, no empty segments;
/// - no `.` or `..` segments;
/// - no NUL character.
///
/// The root itself is the empty path. Segments keep their bytes exactly as
/// on disk (no normalization), so a path round-trips to the file system.
/// Cloning is O(1).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VaultPath(Arc<str>);

/// Why a string is not a valid [`VaultPath`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// A segment is empty (`a//b`, a leading or trailing `/`).
    EmptySegment,
    /// A `.` or `..` segment.
    DotSegment,
    /// A NUL character.
    Nul,
    /// A file-system path component that is not valid UTF-8.
    NotUtf8,
    /// A file-system path that is absolute or has a prefix (`C:`).
    NotRelative,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptySegment => "empty path segment",
            Self::DotSegment => "`.` or `..` segment",
            Self::Nul => "NUL character in path",
            Self::NotUtf8 => "path is not valid UTF-8",
            Self::NotRelative => "path is not relative",
        })
    }
}

impl std::error::Error for PathError {}

impl VaultPath {
    /// The vault root (the empty path).
    #[must_use]
    pub fn root() -> Self {
        Self(Arc::from(""))
    }

    /// Parses a `/`-separated relative path. The empty string is the root.
    pub fn new(s: &str) -> Result<Self, PathError> {
        if !s.is_empty() {
            for seg in s.split('/') {
                check_segment(seg)?;
            }
        }
        Ok(Self(Arc::from(s)))
    }

    /// Builds a path from segments. No segments gives the root.
    pub fn from_segments<I, S>(segments: I) -> Result<Self, PathError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = String::new();
        for (i, seg) in segments.into_iter().enumerate() {
            let seg = seg.as_ref();
            check_segment(seg)?;
            if i > 0 {
                out.push('/');
            }
            out.push_str(seg);
        }
        Ok(Self(Arc::from(out)))
    }

    /// Converts a relative file-system path (as produced by stripping the
    /// root prefix from a walked path) into a vault path.
    pub fn from_relative_fs_path(p: &Path) -> Result<Self, PathError> {
        let mut segs: Vec<&str> = Vec::new();
        for c in p.components() {
            match c {
                Component::Normal(s) => segs.push(s.to_str().ok_or(PathError::NotUtf8)?),
                Component::CurDir => {}
                Component::ParentDir => return Err(PathError::DotSegment),
                Component::RootDir | Component::Prefix(_) => return Err(PathError::NotRelative),
            }
        }
        Self::from_segments(segs)
    }

    /// The path as a `/`-separated string (empty for the root).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The shared string (for map keys without another allocation).
    pub(crate) fn shared_str(&self) -> Arc<str> {
        self.0.clone()
    }

    /// `true` for the root.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The segments, outermost first. Empty for the root.
    pub fn segments(&self) -> impl DoubleEndedIterator<Item = &str> + '_ {
        let s: &str = &self.0;
        s.split('/').filter(|seg| !seg.is_empty())
    }

    /// Number of segments (0 for the root).
    #[must_use]
    pub fn depth(&self) -> usize {
        if self.is_root() {
            0
        } else {
            self.0.bytes().filter(|&b| b == b'/').count() + 1
        }
    }

    /// The last segment, or `None` for the root.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        if self.is_root() {
            None
        } else {
            Some(self.0.rsplit('/').next().unwrap_or(&self.0))
        }
    }

    /// The parent directory, or `None` for the root.
    #[must_use]
    pub fn parent(&self) -> Option<VaultPath> {
        if self.is_root() {
            return None;
        }
        Some(match self.0.rfind('/') {
            Some(i) => Self(Arc::from(&self.0[..i])),
            None => Self::root(),
        })
    }

    /// Appends one segment.
    pub fn join(&self, segment: &str) -> Result<VaultPath, PathError> {
        check_segment(segment)?;
        if self.is_root() {
            Ok(Self(Arc::from(segment)))
        } else {
            Ok(Self(Arc::from(format!("{}/{segment}", self.0))))
        }
    }

    /// The extension of the file name (after its last `.`), without the dot.
    /// A leading dot (`.gitignore`) does not start an extension.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        extension_of(self.file_name()?)
    }

    /// `true` if `prefix` is this path or one of its ancestors (segment
    /// boundaries; the root is a prefix of everything).
    #[must_use]
    pub fn starts_with(&self, prefix: &VaultPath) -> bool {
        if prefix.is_root() {
            return true;
        }
        match self.0.strip_prefix(&*prefix.0) {
            Some("") => true,
            Some(rest) => rest.starts_with('/'),
            None => false,
        }
    }

    /// The rest of the path after `prefix` (an ancestor), without the
    /// separating `/`. `None` if `prefix` is not an ancestor or equal.
    #[must_use]
    pub fn strip_prefix(&self, prefix: &VaultPath) -> Option<&str> {
        if prefix.is_root() {
            return Some(&self.0);
        }
        match self.0.strip_prefix(&*prefix.0)? {
            "" => Some(""),
            rest => rest.strip_prefix('/'),
        }
    }

    /// The file-system path under `root`.
    #[must_use]
    pub fn to_fs_path(&self, root: &Path) -> PathBuf {
        let mut p = root.to_path_buf();
        for seg in self.segments() {
            p.push(seg);
        }
        p
    }
}

impl Default for VaultPath {
    fn default() -> Self {
        Self::root()
    }
}

impl fmt::Debug for VaultPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VaultPath({:?})", &*self.0)
    }
}

impl fmt::Display for VaultPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_root() {
            f.write_str("/")
        } else {
            f.write_str(&self.0)
        }
    }
}

impl serde::Serialize for VaultPath {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

fn check_segment(seg: &str) -> Result<(), PathError> {
    if seg.is_empty() {
        Err(PathError::EmptySegment)
    } else if seg == "." || seg == ".." {
        Err(PathError::DotSegment)
    } else if seg.contains('\0') {
        Err(PathError::Nul)
    } else {
        Ok(())
    }
}

/// The extension of a file name (after its last `.`), without the dot.
/// Names that start with their only dot (`.gitignore`) have none.
#[must_use]
pub fn extension_of(name: &str) -> Option<&str> {
    let dot = name.rfind('.')?;
    if dot == 0 {
        None
    } else {
        Some(&name[dot + 1..])
    }
}

/// The file name without its extension (see [`extension_of`]).
#[must_use]
pub fn stem_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// The outcome of [`normalize_lexically`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lexical {
    /// The path stays inside the root: its segments.
    Inside(Vec<String>),
    /// The path climbs `above` levels over the root; `rest` are the
    /// segments that follow at that level (`../../x` gives `above = 2`,
    /// `rest = ["x"]`, possibly after more `..` were absorbed).
    Escapes {
        /// Levels above the root.
        above: usize,
        /// Segments after the escape.
        rest: Vec<String>,
    },
}

/// Applies `rel` to the directory `base` and resolves `.` and `..`
/// **lexically** (symlinks are never consulted). Empty segments are
/// dropped.
#[must_use]
pub fn normalize_lexically<B, R>(base: B, rel: R) -> Lexical
where
    B: IntoIterator,
    B::Item: AsRef<str>,
    R: IntoIterator,
    R::Item: AsRef<str>,
{
    let mut stack: Vec<String> = base.into_iter().map(|s| s.as_ref().to_owned()).collect();
    let mut above = 0usize;
    for seg in rel {
        match seg.as_ref() {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() {
                    above += 1;
                }
            }
            s => stack.push(s.to_owned()),
        }
    }
    if above == 0 {
        Lexical::Inside(stack)
    } else {
        Lexical::Escapes { above, rest: stack }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construct_and_inspect() {
        let p = VaultPath::new("a/b/c.md").unwrap();
        assert_eq!(p.depth(), 3);
        assert_eq!(p.file_name(), Some("c.md"));
        assert_eq!(p.extension(), Some("md"));
        assert_eq!(p.parent().unwrap().as_str(), "a/b");
        assert_eq!(p.segments().collect::<Vec<_>>(), ["a", "b", "c.md"]);
        assert!(p.starts_with(&VaultPath::new("a/b").unwrap()));
        assert!(!p.starts_with(&VaultPath::new("a/b/c").unwrap()));
        assert!(
            !VaultPath::new("ab/c")
                .unwrap()
                .starts_with(&VaultPath::new("a").unwrap())
        );
        assert_eq!(
            p.strip_prefix(&VaultPath::new("a").unwrap()),
            Some("b/c.md")
        );
        let root = VaultPath::root();
        assert_eq!(root.depth(), 0);
        assert_eq!(root.file_name(), None);
        assert_eq!(root.parent(), None);
        assert_eq!(root.join("x").unwrap().as_str(), "x");
        assert_eq!(
            VaultPath::new("x").unwrap().parent(),
            Some(VaultPath::root())
        );
    }

    #[test]
    fn rejects_invalid() {
        assert_eq!(VaultPath::new("/a"), Err(PathError::EmptySegment));
        assert_eq!(VaultPath::new("a/"), Err(PathError::EmptySegment));
        assert_eq!(VaultPath::new("a/../b"), Err(PathError::DotSegment));
        assert_eq!(VaultPath::new("a\0"), Err(PathError::Nul));
    }

    #[test]
    fn extensions() {
        assert_eq!(extension_of(".gitignore"), None);
        assert_eq!(extension_of("v1.2 notes"), Some("2 notes"));
        assert_eq!(stem_of("Note.1.md"), "Note.1");
        assert_eq!(stem_of(".hidden"), ".hidden");
    }

    #[test]
    fn lexical() {
        assert_eq!(
            normalize_lexically(["a", "b"], ["..", "c", ".", "d"]),
            Lexical::Inside(vec!["a".into(), "c".into(), "d".into()])
        );
        assert_eq!(
            normalize_lexically(["a"], ["..", "..", "x"]),
            Lexical::Escapes {
                above: 1,
                rest: vec!["x".into()]
            }
        );
        assert_eq!(
            normalize_lexically(Vec::<String>::new(), ["..", "v", "x"]),
            Lexical::Escapes {
                above: 1,
                rest: vec!["v".into(), "x".into()]
            }
        );
    }
}
