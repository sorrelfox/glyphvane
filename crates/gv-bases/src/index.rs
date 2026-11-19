// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! [`VaultData`] over the content index of `gv-vault`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use gv_vault::content::{ContentSnapshot, NoteRecord, PropertyValue};
use gv_vault::{EntryKind, LinkKind, LinkRef, VaultPath, resolve};

use crate::source::{FileInfo, LinkInfo, VaultData, number_value, property_value, text_value};
use crate::value::{Link, Value};

/// The files of a content-index snapshot, as a base sees them: every
/// file of the path index (notes and attachments), with the note
/// records' properties, tags and resolved links.
///
/// Built once per snapshot (one pass over the vault); queries are map
/// lookups. Rebuild it when the index publishes a new snapshot.
#[derive(Debug, Clone)]
pub struct IndexData {
    snapshot: ContentSnapshot,
    files: HashMap<Arc<str>, Arc<FileInfo>>,
}

impl IndexData {
    /// Reads `snapshot`. Front matter dates are read in a zone
    /// `utc_offset_minutes` east of UTC.
    #[must_use]
    pub fn new(snapshot: ContentSnapshot, utc_offset_minutes: i32) -> Self {
        let mut files: HashMap<Arc<str>, Arc<FileInfo>> = HashMap::new();
        let mut paths: Vec<(VaultPath, u64, Option<i64>, Option<i64>)> = Vec::new();
        if let Some(index) = snapshot.path_index() {
            for entry in index.iter() {
                if entry.kind == EntryKind::File {
                    let ms = |t: std::time::SystemTime| {
                        t.duration_since(UNIX_EPOCH)
                            .ok()
                            .and_then(|d| i64::try_from(d.as_millis()).ok())
                    };
                    let mtime = entry.meta.mtime.and_then(ms);
                    let ctime = entry.meta.created.and_then(ms);
                    paths.push((entry.path.clone(), entry.meta.size, mtime, ctime));
                }
            }
        }
        // Notes the path index does not list yet (unsaved new buffers).
        let listed: std::collections::HashSet<VaultPath> =
            paths.iter().map(|(p, _, _, _)| p.clone()).collect();
        for note in snapshot.notes() {
            if !listed.contains(&note.path) {
                paths.push((
                    note.path.clone(),
                    note.stamp.size,
                    note.stamp.mtime_ns.map(|ns| ns / 1_000_000),
                    None,
                ));
            }
        }
        let mut data = Self {
            snapshot,
            files: HashMap::new(),
        };
        for (path, size, mtime, ctime) in paths {
            let mut info = FileInfo::new(path.as_str());
            info.size = Some(size);
            info.mtime_ms = mtime;
            info.ctime_ms = ctime;
            if let Some(note) = data.snapshot.note(&path) {
                data.fill_note(&mut info, note, utc_offset_minutes);
            }
            files.insert(info.path.clone(), Arc::new(info));
        }
        data.files = files;
        data
    }

    /// The snapshot read.
    #[must_use]
    pub fn snapshot(&self) -> &ContentSnapshot {
        &self.snapshot
    }

    fn fill_note(&self, info: &mut FileInfo, note: &NoteRecord, offset: i32) {
        info.size = Some(note.stamp.size);
        if let Some(ns) = note.stamp.mtime_ns {
            info.mtime_ms = Some(ns / 1_000_000);
        }
        info.tags = note.data.tags.iter().map(|t| t.name.clone()).collect();
        for out in self.snapshot.outgoing(&note.path) {
            if out.target.status == gv_vault::Status::External {
                continue;
            }
            let link = LinkInfo {
                text: out.link.raw.clone(),
                target: out.target.target.as_ref().map(|p| Arc::from(p.as_str())),
            };
            if out.link.is_embed() {
                info.embeds.push(link);
            } else {
                info.links.push(link);
            }
        }
        info.properties = note
            .data
            .properties
            .iter()
            .map(|p| {
                let v = self.typed(&p.value, &note.path, offset);
                (p.key.clone(), v)
            })
            .collect();
    }

    fn typed(&self, value: &PropertyValue, from: &VaultPath, offset: i32) -> Value {
        let v = match value {
            PropertyValue::Null => Value::Null,
            PropertyValue::Bool(b) => Value::Bool(*b),
            PropertyValue::Number(n) => number_value(n),
            PropertyValue::Text(t) => text_value(t, offset),
            PropertyValue::List(items) => {
                Value::list(items.iter().map(|t| text_value(t, offset)).collect())
            }
            PropertyValue::Other(text) => match crate::yaml::parse_yaml(text) {
                Ok(yaml) => property_value(&yaml, offset),
                Err(_) => Value::String(text.clone()),
            },
        };
        self.resolve_links(v, from)
    }

    fn resolve_links(&self, v: Value, from: &VaultPath) -> Value {
        match v {
            Value::Link(l) if l.path.is_none() => {
                let path = self.resolve_from(&l.text, from);
                Value::Link(Arc::new(Link {
                    path,
                    ..(*l).clone()
                }))
            }
            Value::List(items) => Value::List(
                items
                    .iter()
                    .cloned()
                    .map(|i| self.resolve_links(i, from))
                    .collect(),
            ),
            Value::Object(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), self.resolve_links(v.clone(), from)))
                    .collect(),
            ),
            other => other,
        }
    }

    fn resolve_from(&self, text: &str, from: &VaultPath) -> Option<Arc<str>> {
        let index = self.snapshot.path_index()?;
        let options = self.snapshot.options();
        let link = LinkRef::new(LinkKind::Wikilink, text, from.clone(), options.profile);
        let res = resolve(index, &link, &options.params);
        if !res.status.is_resolved() {
            return None;
        }
        res.target_path.map(|p| Arc::from(p.as_str()))
    }
}

impl VaultData for IndexData {
    fn files(&self) -> Vec<Arc<FileInfo>> {
        self.files.values().cloned().collect()
    }

    fn file(&self, path: &str) -> Option<Arc<FileInfo>> {
        self.files.get(path).cloned()
    }

    fn resolve_link(&self, text: &str, from: &str) -> Option<Arc<str>> {
        let from = VaultPath::new(from).unwrap_or_else(|_| VaultPath::root());
        self.resolve_from(text, &from)
    }

    fn backlinks(&self, path: &str) -> Vec<Arc<str>> {
        let Ok(target) = VaultPath::new(path) else {
            return Vec::new();
        };
        self.snapshot
            .backlink_sources(&target)
            .into_iter()
            .map(|p| Arc::from(p.as_str()))
            .collect()
    }
}
