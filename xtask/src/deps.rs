// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `cargo xtask check-deps`: enforces NFR-MAINT-01. The core crates must
//! stay GUI-free, so none of them may reach a GPUI package through normal
//! or build dependencies.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::cargo;

/// Crates that must not depend on GPUI, directly or transitively.
const GUI_FREE: &[&str] = &[
    "gv-text",
    "gv-syntax",
    "gv-model",
    "gv-vault",
    "gv-search",
    "gv-render-tree",
    "gv-html",
    "gv-highlight",
    "gv-math",
    "gv-diagram",
    "gv-media",
    "gv-bases",
    "gv-canvas",
    "gv-export",
    "gv-editor",
    "gv-spell",
    "gv-helper",
    // Vendored Zed code (third_party/zed/): the rope and its test-only shims.
    "rope",
    "zed-test-util",
    "zed-test-zlog",
];

/// Workspace members that may depend on GPUI or are not shipped code.
const GUI_ALLOWED: &[&str] = &["gv-ui", "gv-app", "xtask"];

/// Zed's GUI-free utility crates, published under the GPUI snapshot's name
/// prefix (ADR-014). Their own dependencies are still checked.
const GUI_FREE_GPUI_SNAPSHOT_CRATES: &[&str] = &[
    "gpui-pre-sum-tree",
    "gpui-pre-util",
    "gpui-pre-ztracing",
    "gpui-pre-ztracing-macro",
];

/// GPUI and its companion crates: `gpui`, `gpui-pre`, `gpui-kit`,
/// `gpui-component`, and anything else named `gpui*`, except the GUI-free
/// utility crates above.
fn is_gui_package(name: &str) -> bool {
    name.starts_with("gpui") && !GUI_FREE_GPUI_SNAPSHOT_CRATES.contains(&name)
}

/// A resolved dependency graph, reduced to what the check needs.
#[derive(Debug, Default)]
pub(crate) struct Graph {
    /// Package id -> display name (`name vX.Y.Z`) and plain name.
    packages: HashMap<String, (String, String)>,
    /// Package id -> ids of its normal and build dependencies.
    edges: HashMap<String, Vec<String>>,
    /// Workspace member name -> package id.
    members: BTreeMap<String, String>,
}

impl Graph {
    /// Builds the graph from `cargo metadata --format-version 1` output.
    pub(crate) fn from_metadata(meta: &Value) -> Result<Self> {
        let mut graph = Self::default();
        let packages = meta["packages"]
            .as_array()
            .context("metadata: no `packages`")?;
        for pkg in packages {
            let id = str_field(pkg, "id")?;
            let name = str_field(pkg, "name")?;
            let version = str_field(pkg, "version")?;
            graph.packages.insert(
                id.to_owned(),
                (format!("{name} v{version}"), name.to_owned()),
            );
        }
        let members = meta["workspace_members"]
            .as_array()
            .context("metadata: no `workspace_members`")?;
        for id in members {
            let id = id.as_str().context("metadata: bad workspace member id")?;
            let (_, name) = graph
                .packages
                .get(id)
                .with_context(|| format!("metadata: unknown workspace member {id}"))?;
            graph.members.insert(name.clone(), id.to_owned());
        }
        let nodes = meta["resolve"]["nodes"]
            .as_array()
            .context("metadata: no `resolve.nodes` (was --no-deps used?)")?;
        for node in nodes {
            let id = str_field(node, "id")?;
            let mut deps = Vec::new();
            for dep in node["deps"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let kinds = dep["dep_kinds"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                // `kind` is null for normal deps; "dev" deps are not shipped.
                let shipped = kinds
                    .iter()
                    .any(|k| k["kind"].is_null() || k["kind"] == "build");
                if shipped {
                    deps.push(str_field(dep, "pkg")?.to_owned());
                }
            }
            graph.edges.insert(id.to_owned(), deps);
        }
        Ok(graph)
    }

    /// Adds a package (test helper and building block).
    #[cfg(test)]
    fn add(&mut self, name: &str, deps: &[&str], member: bool) {
        self.packages
            .insert(name.to_owned(), (format!("{name} v0.0.0"), name.to_owned()));
        self.edges.insert(
            name.to_owned(),
            deps.iter().map(|d| (*d).to_owned()).collect(),
        );
        if member {
            self.members.insert(name.to_owned(), name.to_owned());
        }
    }

    /// Shortest dependency chain from `start` to a GPUI package, as display
    /// names, or `None` if no GPUI package is reachable.
    pub(crate) fn gui_path(&self, start: &str) -> Option<Vec<String>> {
        let mut parent: HashMap<&str, &str> = HashMap::new();
        let mut queue = VecDeque::from([start]);
        parent.insert(start, start);
        while let Some(id) = queue.pop_front() {
            let is_gui = self
                .packages
                .get(id)
                .is_some_and(|(_, n)| is_gui_package(n));
            if is_gui && id != start {
                let mut chain = vec![id];
                let mut cur = id;
                while cur != start {
                    cur = parent[cur];
                    chain.push(cur);
                }
                chain.reverse();
                return Some(chain.into_iter().map(|id| self.display(id)).collect());
            }
            for dep in self.edges.get(id).into_iter().flatten() {
                if !parent.contains_key(dep.as_str()) {
                    parent.insert(dep, id);
                    queue.push_back(dep);
                }
            }
        }
        None
    }

    fn display(&self, id: &str) -> String {
        self.packages
            .get(id)
            .map_or_else(|| id.to_owned(), |(display, _)| display.clone())
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("metadata: missing string field `{key}`"))
}

/// Checks the graph; returns human-readable problems (empty if clean).
pub(crate) fn violations(graph: &Graph) -> Vec<String> {
    let mut problems = Vec::new();
    for name in GUI_FREE {
        match graph.members.get(*name) {
            None => problems.push(format!(
                "{name}: listed as GUI-free but not a workspace member (update xtask/src/deps.rs)"
            )),
            Some(id) => {
                if let Some(chain) = graph.gui_path(id) {
                    problems.push(format!("{name} depends on GPUI: {}", chain.join(" -> ")));
                }
            }
        }
    }
    for name in graph.members.keys() {
        if !GUI_FREE.contains(&name.as_str()) && !GUI_ALLOWED.contains(&name.as_str()) {
            problems.push(format!(
                "{name}: workspace member not classified as GUI-free or GUI-allowed \
                 (update xtask/src/deps.rs)"
            ));
        }
    }
    problems
}

pub(crate) fn run(root: &Path) -> Result<bool> {
    let output = Command::new(cargo())
        .args(["metadata", "--format-version", "1", "--all-features"])
        .current_dir(root)
        .output()
        .context("failed to run `cargo metadata`")?;
    if !output.status.success() {
        bail!(
            "`cargo metadata` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let meta: Value =
        serde_json::from_slice(&output.stdout).context("parsing `cargo metadata` output")?;
    let graph = Graph::from_metadata(&meta)?;
    let problems = violations(&graph);
    if problems.is_empty() {
        println!("check-deps: {} GUI-free crates OK", GUI_FREE.len());
        return Ok(true);
    }
    eprintln!("check-deps: NFR-MAINT-01 violated:");
    for p in &problems {
        eprintln!("  {p}");
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_gpui_names() {
        for n in [
            "gpui",
            "gpui-pre",
            "gpui-kit",
            "gpui-component",
            "gpui_macros",
        ] {
            assert!(is_gui_package(n), "{n}");
        }
        for n in [
            "gv-ui",
            "ropey",
            "agpui",
            "gpui-pre-sum-tree",
            "gpui-pre-util",
        ] {
            assert!(!is_gui_package(n), "{n}");
        }
    }

    #[test]
    fn finds_shortest_transitive_path() {
        let mut g = Graph::default();
        g.add("gv-editor", &["gv-model", "long1"], true);
        g.add("gv-model", &["helper"], true);
        g.add("helper", &["gpui-component"], false);
        g.add("long1", &["long2"], false);
        g.add("long2", &["long3"], false);
        g.add("long3", &["gpui"], false);
        g.add("gpui-component", &["gpui"], false);
        g.add("gpui", &[], false);
        let path = g.gui_path("gv-editor");
        assert_eq!(
            path,
            Some(vec![
                "gv-editor v0.0.0".to_owned(),
                "gv-model v0.0.0".to_owned(),
                "helper v0.0.0".to_owned(),
                "gpui-component v0.0.0".to_owned(),
            ])
        );
    }

    #[test]
    fn clean_graph_with_cycle_has_no_path() {
        let mut g = Graph::default();
        g.add("gv-text", &["a"], true);
        g.add("a", &["b"], false);
        g.add("b", &["a"], false);
        assert_eq!(g.gui_path("gv-text"), None);
    }

    fn pkg(name: &str) -> Value {
        json!({ "id": format!("{name}-id"), "name": name, "version": "1.0.0" })
    }

    fn dep(name: &str, kind: Value) -> Value {
        json!({ "name": name, "pkg": format!("{name}-id"), "dep_kinds": [{ "kind": kind, "target": null }] })
    }

    #[test]
    fn metadata_ignores_dev_deps_and_reports_violations() {
        let mut packages = vec![
            pkg("gpui"),
            pkg("shim"),
            pkg("gv-ui"),
            pkg("gv-app"),
            pkg("xtask"),
        ];
        let mut members = vec![json!("gv-ui-id"), json!("gv-app-id"), json!("xtask-id")];
        let mut nodes = vec![
            json!({ "id": "gpui-id", "deps": [] }),
            json!({ "id": "shim-id", "deps": [dep("gpui", Value::Null)] }),
            json!({ "id": "gv-ui-id", "deps": [dep("gpui", Value::Null)] }),
            json!({ "id": "gv-app-id", "deps": [dep("gv-ui", Value::Null)] }),
            json!({ "id": "xtask-id", "deps": [] }),
        ];
        for name in GUI_FREE {
            packages.push(pkg(name));
            members.push(json!(format!("{name}-id")));
            let deps = match *name {
                // Dev-only GPUI use (e.g. test fixtures) is allowed.
                "gv-text" => vec![dep("gpui", json!("dev"))],
                // A build dependency counts.
                "gv-html" => vec![dep("shim", json!("build"))],
                _ => vec![],
            };
            nodes.push(json!({ "id": format!("{name}-id"), "deps": deps }));
        }
        let meta = json!({
            "packages": packages,
            "workspace_members": members,
            "resolve": { "nodes": nodes },
        });
        let graph = Graph::from_metadata(&meta).expect("valid metadata");
        assert_eq!(
            violations(&graph),
            vec![
                "gv-html depends on GPUI: gv-html v1.0.0 -> shim v1.0.0 -> gpui v1.0.0".to_owned()
            ]
        );
    }

    #[test]
    fn reports_missing_and_unclassified_members() {
        let mut g = Graph::default();
        g.add("gv-new", &[], true);
        let problems = violations(&g);
        assert_eq!(problems.len(), GUI_FREE.len() + 1);
        assert!(problems.iter().any(|p| p.starts_with("gv-new:")));
    }
}
