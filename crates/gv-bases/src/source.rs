// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What a base evaluates over: the files of the vault, as data.
//!
//! [`VaultData`] is the boundary to the content index. The adapter over
//! `gv-vault` is [`crate::IndexData`]; [`MemoryData`] holds files built
//! by hand (tests, previews).

use std::collections::HashMap;
use std::sync::Arc;

use crate::date::Date;
use crate::value::{Link, Url, Value};

/// One link of a file, with where it points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    /// The link text (`Note`, `folder/Note#Heading`).
    pub text: Arc<str>,
    /// The vault path it resolves to, if any.
    pub target: Option<Arc<str>>,
}

/// What a base knows about one file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileInfo {
    /// Vault-relative path with `/` separators (`folder/Note.md`).
    pub path: Arc<str>,
    /// Size in bytes, if known.
    pub size: Option<u64>,
    /// Creation time, milliseconds since the Unix epoch (UTC), if known.
    pub ctime_ms: Option<i64>,
    /// Modification time, milliseconds since the Unix epoch (UTC), if
    /// known.
    pub mtime_ms: Option<i64>,
    /// Front matter properties, in order, already typed (see
    /// [`property_value`]).
    pub properties: Vec<(Arc<str>, Value)>,
    /// Tags of the content and the front matter, without `#`.
    pub tags: Vec<Arc<str>>,
    /// Internal links (not embeds), front matter links included.
    pub links: Vec<LinkInfo>,
    /// Embeds.
    pub embeds: Vec<LinkInfo>,
}

impl FileInfo {
    /// A file at `path` with nothing else known.
    #[must_use]
    pub fn new(path: impl Into<Arc<str>>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }

    /// The file name (`Note.md`).
    #[must_use]
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The file name without its extension (`Note`).
    #[must_use]
    pub fn basename(&self) -> &str {
        let name = self.name();
        match name.rfind('.') {
            Some(i) if i > 0 => &name[..i],
            _ => name,
        }
    }

    /// The extension without the dot (`md`), or `""`.
    #[must_use]
    pub fn ext(&self) -> &str {
        let name = self.name();
        match name.rfind('.') {
            Some(i) if i > 0 => &name[i + 1..],
            _ => "",
        }
    }

    /// The parent folder (`folder/sub`), `/` at the vault root (as
    /// Obsidian reports it).
    #[must_use]
    pub fn folder(&self) -> &str {
        match self.path.rfind('/') {
            Some(i) => &self.path[..i],
            None => "/",
        }
    }

    /// The value of the front matter property `key`.
    #[must_use]
    pub fn property(&self, key: &str) -> Option<&Value> {
        self.properties
            .iter()
            .find(|(k, _)| &**k == key)
            .map(|(_, v)| v)
    }
}

/// The vault as a base sees it. Implementations must be cheap to query
/// per row: [`file`](Self::file) and [`resolve_link`](Self::resolve_link)
/// run for many cells.
pub trait VaultData: Sync {
    /// Every file a base may list, notes and other files. Order does not
    /// matter (results are sorted).
    fn files(&self) -> Vec<Arc<FileInfo>>;

    /// The file at `path`.
    fn file(&self, path: &str) -> Option<Arc<FileInfo>>;

    /// Where the link text `text` (`Note`, `folder/Note`, `Note#Part`)
    /// written in the file `from` points, as Obsidian resolves it.
    fn resolve_link(&self, text: &str, from: &str) -> Option<Arc<str>>;

    /// The files that link to `path`.
    fn backlinks(&self, path: &str) -> Vec<Arc<str>>;
}

/// Files held in memory, with a simple Obsidian-like resolver: the exact
/// path, the path plus `.md`, then the shortest path whose file name
/// (with or without `.md`) matches, ignoring case.
#[derive(Debug, Default, Clone)]
pub struct MemoryData {
    files: HashMap<Arc<str>, Arc<FileInfo>>,
}

impl MemoryData {
    /// Holds `files`. Links whose `target` is `None` are resolved here.
    #[must_use]
    pub fn new(files: impl IntoIterator<Item = FileInfo>) -> Self {
        let mut data = Self {
            files: files
                .into_iter()
                .map(|f| (f.path.clone(), Arc::new(f)))
                .collect(),
        };
        let resolved: Vec<Arc<FileInfo>> = data
            .files
            .values()
            .map(|f| {
                let mut f = (**f).clone();
                for link in f.links.iter_mut().chain(f.embeds.iter_mut()) {
                    if link.target.is_none() {
                        link.target = data.resolve_link(&link.text, &f.path);
                    }
                }
                let props = std::mem::take(&mut f.properties);
                f.properties = props
                    .into_iter()
                    .map(|(k, v)| (k, resolve_value(&data, v, &f.path)))
                    .collect();
                Arc::new(f)
            })
            .collect();
        data.files = resolved.into_iter().map(|f| (f.path.clone(), f)).collect();
        data
    }
}

fn resolve_value(data: &MemoryData, v: Value, from: &str) -> Value {
    match v {
        Value::Link(l) if l.path.is_none() => Value::Link(Arc::new(Link {
            path: data.resolve_link(&l.text, from),
            ..(*l).clone()
        })),
        Value::List(items) => Value::List(
            items
                .iter()
                .cloned()
                .map(|i| resolve_value(data, i, from))
                .collect(),
        ),
        other => other,
    }
}

impl VaultData for MemoryData {
    fn files(&self) -> Vec<Arc<FileInfo>> {
        self.files.values().cloned().collect()
    }

    fn file(&self, path: &str) -> Option<Arc<FileInfo>> {
        self.files.get(path).cloned()
    }

    fn resolve_link(&self, text: &str, _from: &str) -> Option<Arc<str>> {
        let target = text.split(['#', '|']).next().unwrap_or(text).trim();
        if target.is_empty() {
            return None;
        }
        for candidate in [target.to_owned(), format!("{target}.md")] {
            if let Some((k, _)) = self.files.get_key_value(candidate.as_str()) {
                return Some(k.clone());
            }
        }
        let want = target.to_lowercase();
        let want_md = format!("{want}.md");
        self.files
            .keys()
            .filter(|p| {
                let lower = p.to_lowercase();
                let name = lower.rsplit('/').next().unwrap_or(&lower).to_owned();
                name == want || name == want_md || lower.ends_with(&format!("/{want_md}"))
            })
            .min_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
            .cloned()
    }

    fn backlinks(&self, path: &str) -> Vec<Arc<str>> {
        let mut out: Vec<Arc<str>> = self
            .files
            .values()
            .filter(|f| {
                f.links
                    .iter()
                    .chain(f.embeds.iter())
                    .any(|l| l.target.as_deref() == Some(path))
            })
            .map(|f| f.path.clone())
            .collect();
        out.sort();
        out
    }
}

/// A front matter string as a typed value: a whole `[[wikilink]]` becomes
/// a [`Value::Link`] (unresolved: `path` is `None`), an `http(s)://` or
/// `mailto:` URL a [`Value::Url`], an ISO date (`2025-01-31`,
/// `2025-01-31T10:00`) a [`Value::Date`], anything else a string.
///
/// Obsidian types properties per name, vault-wide (`types.json`); until
/// those types are available, the value's shape decides.
#[must_use]
pub fn text_value(text: &str, local_offset_minutes: i32) -> Value {
    let t = text.trim();
    if let Some(inner) = t.strip_prefix("[[").and_then(|r| r.strip_suffix("]]"))
        && !inner.contains("]]")
        && !inner.contains("[[")
    {
        let (target, display) = match inner.split_once('|') {
            Some((a, b)) => (a, Some(Arc::from(b))),
            None => (inner, None),
        };
        return Value::Link(Arc::new(Link {
            text: target.into(),
            path: None,
            display,
        }));
    }
    if t.starts_with("http://") || t.starts_with("https://") || t.starts_with("mailto:") {
        return Value::Url(Arc::new(Url {
            url: t.into(),
            display: None,
        }));
    }
    if t.len() >= 10
        && t.as_bytes()[4] == b'-'
        && let Some(d) = Date::parse(t, local_offset_minutes)
    {
        return Value::Date(d);
    }
    Value::string(text)
}

/// A front matter number as written: a number if it parses, else text.
#[must_use]
pub fn number_value(text: &str) -> Value {
    match text.trim().parse::<f64>() {
        Ok(n) => Value::Number(n),
        Err(_) => Value::string(text),
    }
}

/// Builds a property [`Value`] from YAML (nested front matter): strings
/// through [`text_value`], maps as objects.
#[must_use]
pub fn property_value(yaml: &crate::yaml::YamlValue, local_offset_minutes: i32) -> Value {
    use crate::yaml::YamlValue as Y;
    match yaml {
        Y::Null => Value::Null,
        Y::Bool(b) => Value::Bool(*b),
        #[allow(clippy::cast_precision_loss)]
        Y::Int(i) => Value::Number(*i as f64),
        Y::Float(f) => Value::Number(*f),
        Y::String(s) => text_value(s, local_offset_minutes),
        Y::Seq(items) => Value::list(
            items
                .iter()
                .map(|i| property_value(i, local_offset_minutes))
                .collect(),
        ),
        Y::Map(entries) => Value::Object(
            entries
                .iter()
                .map(|(k, v)| {
                    (
                        Arc::from(k.as_str()),
                        property_value(v, local_offset_minutes),
                    )
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_parts() {
        let f = FileInfo::new("a/b/Note.v2.md");
        assert_eq!(f.name(), "Note.v2.md");
        assert_eq!(f.basename(), "Note.v2");
        assert_eq!(f.ext(), "md");
        assert_eq!(f.folder(), "a/b");
        let root = FileInfo::new(".hidden");
        assert_eq!(root.ext(), "");
        assert_eq!(root.basename(), ".hidden");
        assert_eq!(root.folder(), "/");
    }

    #[test]
    fn text_values() {
        assert!(
            matches!(text_value("[[Note|Shown]]", 0), Value::Link(l) if &*l.text == "Note" && l.display.as_deref() == Some("Shown"))
        );
        assert!(matches!(
            text_value("https://example.org", 0),
            Value::Url(_)
        ));
        assert!(matches!(text_value("2025-01-31", 0), Value::Date(d) if !d.has_time));
        assert!(matches!(text_value("2025-01-31T10:00", 0), Value::Date(d) if d.has_time));
        assert_eq!(text_value("2025 plans", 0), Value::string("2025 plans"));
        assert_eq!(
            text_value("[[a]] and [[b]]", 0),
            Value::string("[[a]] and [[b]]")
        );
        assert_eq!(number_value("2.5"), Value::Number(2.5));
        assert_eq!(number_value("0x1"), Value::string("0x1"));
    }

    #[test]
    fn memory_resolver() {
        let mut a = FileInfo::new("notes/A.md");
        a.links.push(LinkInfo {
            text: "b".into(),
            target: None,
        });
        a.properties.push(("up".into(), text_value("[[B]]", 0)));
        let data = MemoryData::new([a, FileInfo::new("deep/x/B.md"), FileInfo::new("B.md")]);
        assert_eq!(data.resolve_link("B", "x").as_deref(), Some("B.md"));
        assert_eq!(
            data.resolve_link("deep/x/B", "x").as_deref(),
            Some("deep/x/B.md")
        );
        assert_eq!(
            data.resolve_link("A#Heading", "x").as_deref(),
            Some("notes/A.md")
        );
        assert_eq!(data.resolve_link("missing", "x"), None);
        let a = data.file("notes/A.md").expect("a");
        assert_eq!(a.links[0].target.as_deref(), Some("B.md"));
        assert!(
            matches!(a.property("up"), Some(Value::Link(l)) if l.path.as_deref() == Some("B.md"))
        );
        assert_eq!(data.backlinks("B.md"), [Arc::<str>::from("notes/A.md")]);
    }
}
