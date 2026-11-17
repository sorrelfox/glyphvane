// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A base evaluated over the real content index.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gv_bases::{CompiledBase, EvalContext, IndexData, Value, VaultData};
use gv_vault::content::{ContentIndexer, ContentOptions, FileSource};
use gv_vault::{EntryKind, FileMeta, IndexConfig, IndexState, PathIndex, ProfileId, VaultPath};

struct Mem(HashMap<String, String>);

impl FileSource for Mem {
    fn is_placeholder(&self, _: &VaultPath) -> io::Result<bool> {
        Ok(false)
    }
    fn read(&self, path: &VaultPath) -> io::Result<Vec<u8>> {
        self.0
            .get(path.as_str())
            .map(|s| s.clone().into_bytes())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
}

const FILES: &[(&str, &str)] = &[
    (
        "Projects/Alpha.md",
        "---\nstatus: active\npriority: 2\ndue: 2025-04-01\nowner: \"[[Ada]]\"\ntags: [project]\n---\n# Alpha\n\nSee [[Beta]] and ![[diagram.png]].\n",
    ),
    (
        "Projects/Beta.md",
        "---\nstatus: done\npriority: 1\ntags:\n  - project\n  - project/old\n---\n# Beta\n\nBack to [[Alpha]]. #urgent\n",
    ),
    ("People/Ada.md", "---\nrole: lead\n---\n# Ada\n"),
    ("Dashboard.md", "# Dashboard\n\n[[Alpha]] [[Beta]]\n"),
    ("diagram.png", ""),
    ("Projects.base", "views:\n  - type: table\n"),
];

fn snapshot_data() -> IndexData {
    let files: HashMap<String, String> = FILES
        .iter()
        .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
        .collect();
    let entries = FILES.iter().enumerate().map(|(i, (p, t))| {
        (
            VaultPath::new(p).expect("path"),
            EntryKind::File,
            FileMeta {
                size: t.len() as u64,
                mtime: Some(
                    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + i as u64 * 60),
                ),
                identity: None,
            },
        )
    });
    let index = PathIndex::from_entries(
        "/vault",
        IndexConfig::obsidian(Vec::new()),
        IndexState::Complete,
        entries,
    )
    .expect("index");
    let mut options = ContentOptions::for_profile(ProfileId::Obsidian);
    options.threads = 1;
    let indexer = ContentIndexer::start_with_source(index, options, Arc::new(Mem(files)), |_| {})
        .expect("indexer");
    assert!(indexer.flush(Duration::from_secs(20)), "index flush");
    let snapshot = indexer.snapshot().expect("snapshot");
    indexer.stop();
    IndexData::new(snapshot, 0)
}

#[test]
fn files_and_properties_from_the_index() {
    let data = snapshot_data();
    assert_eq!(data.files().len(), FILES.len());
    let alpha = data.file("Projects/Alpha.md").expect("alpha");
    assert_eq!(alpha.property("status"), Some(&Value::string("active")));
    assert_eq!(alpha.property("priority"), Some(&Value::Number(2.0)));
    assert!(matches!(alpha.property("due"), Some(Value::Date(_))));
    assert!(
        matches!(alpha.property("owner"), Some(Value::Link(l)) if l.path.as_deref() == Some("People/Ada.md")),
        "{:?}",
        alpha.property("owner")
    );
    assert_eq!(alpha.links.len(), 2, "{:?}", alpha.links);
    assert_eq!(alpha.embeds.len(), 1);
    assert_eq!(alpha.embeds[0].target.as_deref(), Some("diagram.png"));
    assert!(alpha.mtime_ms.is_some());
    assert_eq!(data.backlinks("Projects/Alpha.md").len(), 2);
    assert_eq!(
        data.resolve_link("Beta", "Dashboard.md").as_deref(),
        Some("Projects/Beta.md")
    );
}

#[test]
fn view_over_the_index() {
    let data = snapshot_data();
    let base = CompiledBase::parse(
        "filters:\n  and:\n    - file.hasTag('project')\nformulas:\n  owner_role: 'owner.asFile().role'\nviews:\n  - type: table\n    order: [file.basename, status, formula.owner_role, file.backlinks]\n    sort:\n      - property: priority\n  - type: table\n    filters: 'file.hasLink(this)'\n",
    )
    .expect("base");
    let ctx = EvalContext::new(1_741_608_000_000, 0).with_this("Projects.base");
    let r = base.evaluate(0, &data, &ctx);
    assert_eq!(r.matched, 2);
    let names: Vec<String> = r
        .rows()
        .map(|row| row.cells[0].clone().expect("name").display())
        .collect();
    assert_eq!(names, ["Beta", "Alpha"]);
    let alpha = r.rows().nth(1).expect("alpha");
    assert_eq!(alpha.cells[2], Ok(Value::string("lead")));
    assert_eq!(
        alpha.cells[3].as_ref().map(Value::display).as_deref(),
        Ok("Dashboard, Beta")
    );
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);

    // An embedded base: `this` is the embedding note.
    let r = base.evaluate(1, &data, &ctx.clone().with_this("Dashboard.md"));
    assert_eq!(r.matched, 0);
    let r = base.evaluate(1, &data, &ctx.with_this("Projects/Alpha.md"));
    let paths: Vec<&str> = r.rows().map(|row| &*row.path).collect();
    assert_eq!(paths, ["Projects/Beta.md"]);
}
