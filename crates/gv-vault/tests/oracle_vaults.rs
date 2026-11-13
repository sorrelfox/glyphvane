// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Obsidian oracle fixture vaults through the path index and the resolver,
//! emitted as `glyphvane-expect/1`.
//!
//! - `mini_vault_round_trip` always runs: a small vault in a temp dir.
//! - `oracle_fixture_vaults` runs when `GLYPHVANE_ORACLE_VAULTS` names the
//!   directory holding the generated fixture vaults (`obsidian-links`,
//!   `obsidian-tiebreak`, …); otherwise it only says it was skipped.
//!   `GLYPHVANE_EXPECT_OUT` keeps the emitted files in that directory.
//!   `GLYPHVANE_ORACLE_COMPARE` names the oracle comparator script: the
//!   emitted files are then also checked with it (`python3 -I`), against a
//!   synthetic oracle built from them and through its `--skeleton` output.
//!
//! The link extractor and the heading matcher here are deliberately small
//! stand-ins for the parser and the document model; they cover the syntax
//! the fixtures use, not all of Markdown.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gv_vault::expect::{
    EXPECT_SCHEMA, ExpectFile, ExpectKind, ExpectLink, ExpectProbe, is_valid_status,
};
use gv_vault::{
    Fragment, FragmentKind, FragmentResolver, FragmentTarget, IndexConfig, LinkKind, LinkRef,
    PathIndex, ProfileId, ProfileParams, Status, VaultPath, resolve,
};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// A small link extractor (stand-in for the parser).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Extracted {
    kind: ExpectKind,
    link_kind: LinkKind,
    original: String,
    raw: String,
}

/// What the parser would hand the resolver for a Markdown destination:
/// angle brackets removed, backslash escapes of ASCII punctuation resolved.
fn parser_destination(dest: &str) -> String {
    let d = dest
        .strip_prefix('<')
        .and_then(|d| d.strip_suffix('>'))
        .unwrap_or(dest);
    let mut out = String::new();
    let mut chars = d.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek().is_some_and(char::is_ascii_punctuation) {
            out.push(chars.next().unwrap_or('\\'));
        } else {
            out.push(c);
        }
    }
    out
}

/// Parses `(dest "title")` at `s`; returns `(dest, end)` with `end` after `)`.
fn inline_destination(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.first() != Some(&b'(') {
        return None;
    }
    let mut i = 1;
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    let start = i;
    if b.get(i) == Some(&b'<') {
        let close = s[i..].find('>')? + i;
        i = close + 1;
    } else {
        let mut depth = 0;
        while i < b.len() {
            match b[i] {
                b'\\' => i += 1,
                b'(' => depth += 1,
                b')' if depth == 0 => break,
                b')' => depth -= 1,
                b' ' | b'\t' => break,
                _ => {}
            }
            i += 1;
        }
    }
    let dest = s[start..i.min(s.len())].to_owned();
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    if b.get(i) == Some(&b'"') {
        i = s[i + 1..].find('"')? + i + 2;
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
    }
    (b.get(i) == Some(&b')')).then_some((dest, i + 1))
}

/// Byte ranges of `line` hidden from link scanning: code spans, `%%…%%`
/// and `<!-- … -->` on the line.
fn masked(line: &str) -> Vec<bool> {
    let b = line.as_bytes();
    let mut mask = vec![false; b.len()];
    let mut i = 0;
    while i < b.len() {
        if !line.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let hide = |from: usize, to: usize, mask: &mut Vec<bool>| {
            mask[from..to].iter_mut().for_each(|m| *m = true)
        };
        if b[i] == b'`' {
            let run = b[i..].iter().take_while(|&&c| c == b'`').count();
            let fence = &line[i..i + run];
            if let Some(close) = line[i + run..].find(fence) {
                let end = i + run + close + run;
                hide(i, end, &mut mask);
                i = end;
                continue;
            }
        } else if line[i..].starts_with("%%") {
            if let Some(close) = line[i + 2..].find("%%") {
                let end = i + 2 + close + 2;
                hide(i, end, &mut mask);
                i = end;
                continue;
            }
        } else if line[i..].starts_with("<!--")
            && let Some(close) = line[i..].find("-->")
        {
            hide(i, i + close + 3, &mut mask);
            i += close + 3;
            continue;
        }
        i += 1;
    }
    mask
}

fn scan_line(
    line: &str,
    kind: ExpectKind,
    refs: &HashMap<String, String>,
    out: &mut Vec<Extracted>,
) {
    let mask = masked(line);
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if mask[i] || !line.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &line[i..];
        let embed = rest.starts_with('!');
        let at = if embed { i + 1 } else { i };
        let r = &line[at..];
        if let Some(after_open) = r.strip_prefix("[[") {
            if let Some(close) = after_open.find("]]") {
                let inner = &after_open[..close];
                let end = at + 2 + close + 2;
                out.push(Extracted {
                    kind: if embed { ExpectKind::Embed } else { kind },
                    link_kind: if embed {
                        LinkKind::Embed
                    } else {
                        LinkKind::Wikilink
                    },
                    original: line[i..end].to_owned(),
                    raw: inner.replace("\\|", "|"),
                });
                i = end;
                continue;
            }
        } else if r.starts_with('[') {
            if let Some(close) = r.find(']') {
                let after = &r[close + 1..];
                let text = &r[1..close];
                if let Some((dest, len)) = inline_destination(after) {
                    let end = at + close + 1 + len;
                    out.push(Extracted {
                        kind: if embed { ExpectKind::Embed } else { kind },
                        link_kind: if embed {
                            LinkKind::Image
                        } else {
                            LinkKind::Inline
                        },
                        original: line[i..end].to_owned(),
                        raw: parser_destination(&dest),
                    });
                    i = end;
                    continue;
                }
                let (label, len) = match after
                    .strip_prefix('[')
                    .and_then(|a| a.find(']').map(|c| (&a[..c], c + 2)))
                {
                    Some(("", len)) => (text, len),
                    Some((l, len)) => (l, len),
                    None => (text, 0),
                };
                if let Some(dest) = refs.get(&label.to_lowercase()) {
                    let end = at + close + 1 + len;
                    out.push(Extracted {
                        kind: if embed { ExpectKind::Embed } else { kind },
                        link_kind: if embed {
                            LinkKind::Image
                        } else {
                            LinkKind::Reference
                        },
                        original: line[i..end].to_owned(),
                        raw: parser_destination(dest),
                    });
                    i = end;
                    continue;
                }
            }
        } else if !embed && rest.starts_with('<') {
            if let Some(close) = rest.find('>') {
                let inner = &rest[1..close];
                if gv_vault::url::has_scheme(inner) && !inner.contains(' ') {
                    out.push(Extracted {
                        kind,
                        link_kind: LinkKind::Autolink,
                        original: rest[..=close].to_owned(),
                        raw: inner.to_owned(),
                    });
                    i += close + 1;
                    continue;
                }
            }
        } else if !embed && (rest.starts_with("https://") || rest.starts_with("http://")) {
            let end = rest.find([' ', ')', '>']).unwrap_or(rest.len());
            out.push(Extracted {
                kind,
                link_kind: LinkKind::Autolink,
                original: rest[..end].to_owned(),
                raw: rest[..end].to_owned(),
            });
            i += end;
            continue;
        }
        i += 1;
    }
}

fn extract(text: &str) -> Vec<Extracted> {
    let mut out = Vec::new();
    let mut body = text;
    if let Some(fm) = text.strip_prefix("---\n")
        && let Some(end) = fm.find("\n---")
    {
        for line in fm[..end].lines() {
            scan_line(line, ExpectKind::Frontmatter, &HashMap::new(), &mut out);
        }
        body = &fm[end + 4..];
    }
    let mut refs = HashMap::new();
    for line in body.lines() {
        let t = line.trim_start();
        if let Some(r) = t.strip_prefix('[')
            && let Some((label, dest)) = r.split_once("]:")
        {
            refs.insert(label.to_lowercase(), dest.trim().to_owned());
        }
    }
    let mut fence: Option<String> = None;
    let mut in_comment = false;
    let mut in_html_comment = false;
    let mut prev_blank = true;
    let mut prev_indented_code = false;
    for line in body.lines() {
        let t = line.trim_start();
        if let Some(f) = &fence {
            if t.starts_with(f.as_str()) {
                fence = None;
            }
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = Some(t[..3].to_owned());
            continue;
        }
        if t == "%%" {
            in_comment = !in_comment;
            continue;
        }
        if in_comment {
            continue;
        }
        if in_html_comment {
            in_html_comment = !line.contains("-->");
            continue;
        }
        if t.starts_with("<!--") && !t.contains("-->") {
            in_html_comment = true;
            continue;
        }
        let indented = line.starts_with("    ") || line.starts_with('\t');
        if indented && (prev_blank || prev_indented_code) {
            prev_indented_code = true;
            prev_blank = false;
            continue;
        }
        prev_indented_code = false;
        prev_blank = t.is_empty();
        if t.starts_with('[') && t.contains("]:") && !t.starts_with("[[") {
            continue; // a reference definition
        }
        scan_line(line, ExpectKind::Link, &refs, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------
// An approximate Obsidian heading matcher (stand-in for the document model).
// ORACLE-PENDING: Obsidian's `stripHeading` rules and block-id case.
// ---------------------------------------------------------------------------

struct Heading {
    level: usize,
    text: String,
    line: u32,
}

struct DocInfo {
    headings: Vec<Heading>,
    blocks: Vec<(String, u32)>,
}

fn doc_info(text: &str) -> DocInfo {
    let mut headings = Vec::new();
    let mut blocks = Vec::new();
    let mut fence = false;
    let mut in_fm = text.starts_with("---\n");
    for (n, line) in text.lines().enumerate() {
        let n = u32::try_from(n).unwrap_or(u32::MAX);
        if in_fm {
            if n > 0 && line == "---" {
                in_fm = false;
            }
            continue;
        }
        let t = line.trim_start();
        if t.starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        let hashes = t.bytes().take_while(|&c| c == b'#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            let text = t[hashes..]
                .trim()
                .trim_end_matches('#')
                .trim_end()
                .to_owned();
            headings.push(Heading {
                level: hashes,
                text,
                line: n,
            });
        }
        if let Some(pos) = line.rfind('^') {
            let id = &line[pos + 1..];
            let before = &line[..pos];
            if !id.is_empty()
                && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                && (before.is_empty() || before.ends_with(' '))
            {
                blocks.push((id.to_owned(), n));
            }
        }
    }
    DocInfo { headings, blocks }
}

fn strip_heading(s: &str) -> String {
    let kept: String = s
        .chars()
        .map(|c| if "#|^:%[]*`".contains(c) { ' ' } else { c })
        .collect();
    kept.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

struct ObsidianHeadings {
    root: PathBuf,
}

impl FragmentResolver for ObsidianHeadings {
    fn resolve_fragment(
        &self,
        target: &VaultPath,
        fragment: &Fragment,
        _: &ProfileParams,
    ) -> FragmentTarget {
        match &fragment.kind {
            FragmentKind::Top => return FragmentTarget::Top,
            FragmentKind::Viewer(_) => return FragmentTarget::Passthrough,
            FragmentKind::Lines { start, end } => {
                return FragmentTarget::Lines {
                    start: *start,
                    end: end.unwrap_or(*start),
                };
            }
            _ => {}
        }
        if target
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("md"))
        {
            return FragmentTarget::Passthrough;
        }
        let Ok(text) = fs::read_to_string(target.to_fs_path(&self.root)) else {
            return FragmentTarget::Missing;
        };
        let info = doc_info(&text);
        match &fragment.kind {
            FragmentKind::Block(id) => {
                if let Some((bid, line)) = info.blocks.iter().find(|(b, _)| b == id) {
                    return FragmentTarget::Block {
                        id: bid.clone(),
                        line: *line,
                        loose: false,
                    };
                }
                if let Some((bid, line)) =
                    info.blocks.iter().find(|(b, _)| b.eq_ignore_ascii_case(id))
                {
                    return FragmentTarget::Block {
                        id: bid.clone(),
                        line: *line,
                        loose: true,
                    };
                }
                FragmentTarget::Missing
            }
            FragmentKind::Heading(chain) => {
                let mut range = 0..info.headings.len();
                let mut found = None;
                for part in chain {
                    let want = strip_heading(part);
                    let Some(i) = range
                        .clone()
                        .find(|&i| strip_heading(&info.headings[i].text) == want)
                    else {
                        return FragmentTarget::Missing;
                    };
                    let level = info.headings[i].level;
                    let end = (i + 1..info.headings.len())
                        .find(|&j| info.headings[j].level <= level)
                        .unwrap_or(info.headings.len());
                    range = i + 1..end;
                    found = Some(i);
                }
                match found {
                    Some(i) => FragmentTarget::Heading {
                        text: info.headings[i].text.clone(),
                        line: info.headings[i].line,
                        loose: false,
                    },
                    None => FragmentTarget::Missing,
                }
            }
            _ => FragmentTarget::Unchecked,
        }
    }
}

// ---------------------------------------------------------------------------
// Emitting and validating `glyphvane-expect/1`.
// ---------------------------------------------------------------------------

fn user_ignore_filters(vault: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(vault.join(".obsidian/app.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    v["userIgnoreFilters"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn expect_for_vault(vault: &Path, name: &str) -> (ExpectFile, BTreeMap<String, usize>) {
    let (idx, _) = PathIndex::build(vault, IndexConfig::obsidian(user_ignore_filters(vault)))
        .expect("test setup");
    let params = ProfileParams::for_profile(ProfileId::Obsidian);
    let fragments = ObsidianHeadings {
        root: vault.to_path_buf(),
    };
    let mut links = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut sources: Vec<&gv_vault::Entry> = idx
        .iter()
        .filter(|e| e.path.extension() == Some("md") && !e.kind.is_dir())
        .collect();
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    for src in sources {
        let text = fs::read_to_string(src.path.to_fs_path(vault)).expect("test setup");
        let mut ordinals: HashMap<(String, String), u32> = HashMap::new();
        for x in extract(&text) {
            let kind_name = format!("{:?}", x.kind);
            let ord = ordinals.entry((kind_name, x.original.clone())).or_insert(0);
            let link = LinkRef::new(
                x.link_kind,
                x.raw.clone(),
                src.path.clone(),
                ProfileId::Obsidian,
            );
            let r = resolve(&idx, &link, &params);
            let frag = r.resolve_fragment(&fragments, &params);
            let base = match r.status {
                Status::Ambiguous(_) => "Ambiguous".to_owned(),
                s => s.to_string(),
            };
            *counts.entry(base).or_default() += 1;
            links.push(r.to_expect_link(
                src.path.as_str(),
                x.kind,
                &x.original,
                *ord,
                frag.as_ref(),
            ));
            *ord += 1;
        }
    }
    let mut probes = Vec::new();
    if let Ok(text) = fs::read_to_string(vault.join(".oracle/probes.json")) {
        let v: Value = serde_json::from_str(&text).expect("test setup");
        for p in v["resolve"].as_array().into_iter().flatten() {
            let (Some(source), Some(linkpath)) = (p["source"].as_str(), p["linkpath"].as_str())
            else {
                continue;
            };
            let link = LinkRef::new(
                LinkKind::Wikilink,
                linkpath,
                VaultPath::new(source).expect("test setup"),
                ProfileId::Obsidian,
            );
            let r = resolve(&idx, &link, &params);
            probes.push(ExpectProbe {
                source: source.to_owned(),
                linkpath: linkpath.to_owned(),
                status: r.status.to_string(),
                target: r.expect_target(),
            });
        }
    }
    let file = ExpectFile {
        schema: EXPECT_SCHEMA.to_owned(),
        vault: name.to_owned(),
        profile: ProfileId::Obsidian.as_str().to_owned(),
        links,
        probes,
    };
    (file, counts)
}

/// Checks a `glyphvane-expect/1` document the way the oracle comparator
/// reads it.
fn validate(v: &Value) -> Result<(), String> {
    let str_or_null = |x: &Value| x.is_null() || x.is_string();
    if v["schema"] != EXPECT_SCHEMA {
        return Err(format!("schema {}", v["schema"]));
    }
    if !v["vault"].is_string() || !v["profile"].is_string() {
        return Err("vault/profile".into());
    }
    let mut keys = std::collections::HashSet::new();
    for l in v["links"].as_array().ok_or("links")? {
        let key = (
            l["source"].as_str(),
            l["kind"].as_str(),
            l["original"].as_str(),
            l["ordinal"].as_u64(),
        );
        let (Some(_), Some(kind), Some(_), Some(_)) = key else {
            return Err(format!("key fields: {l}"));
        };
        if !matches!(kind, "link" | "embed" | "frontmatter") {
            return Err(format!("kind {kind}"));
        }
        if !keys.insert(key) {
            return Err(format!("duplicate key {l}"));
        }
        if !l["status"].as_str().is_some_and(is_valid_status) {
            return Err(format!("status {}", l["status"]));
        }
        if !str_or_null(&l["target"]) {
            return Err(format!("target {}", l["target"]));
        }
        if let Some(c) = l.get("candidates")
            && !c.as_array().is_some_and(|a| a.iter().all(Value::is_string))
        {
            return Err(format!("candidates {c}"));
        }
        let sp = &l["subpath"];
        if !sp.is_null() {
            let ok = match sp["kind"].as_str() {
                Some("heading") => sp["heading"].is_string() && sp["line"].is_u64(),
                Some("block") => sp["id"].is_string() && sp["line"].is_u64(),
                Some("missing" | "passthrough") => true,
                _ => false,
            };
            if !ok {
                return Err(format!("subpath {sp}"));
            }
        }
    }
    for p in v["probes"].as_array().into_iter().flatten() {
        if !p["source"].is_string() || !p["linkpath"].is_string() || !str_or_null(&p["target"]) {
            return Err(format!("probe {p}"));
        }
        if !p["status"].as_str().is_some_and(is_valid_status) {
            return Err(format!("probe status {p}"));
        }
    }
    Ok(())
}

/// A `glyphvane-oracle/1` dump that agrees with `expected`: what the
/// comparator would see if Obsidian resolved exactly like we do. External
/// and blocked links are left out, as Obsidian does not index them.
fn synthetic_oracle(expected: &ExpectFile) -> Value {
    let link_path = |original: &str| -> String {
        let inner = original
            .trim_start_matches('!')
            .trim_start_matches("[[")
            .trim_end_matches("]]");
        inner.split(['|', '#']).next().unwrap_or("").to_owned()
    };
    let mut links = Vec::new();
    for l in &expected.links {
        if matches!(l.status.as_str(), "External" | "Blocked") {
            continue;
        }
        let sp = match &l.subpath {
            None => Value::Null,
            Some(gv_vault::expect::ExpectSubpath::Heading { heading, line }) => {
                json!({"type": "heading", "heading": heading, "line": line})
            }
            Some(gv_vault::expect::ExpectSubpath::Block { id, line }) => {
                json!({"type": "block", "id": id, "line": line})
            }
            Some(gv_vault::expect::ExpectSubpath::Missing) => json!({"type": "unresolved"}),
            Some(gv_vault::expect::ExpectSubpath::Passthrough) => json!({"type": "not-markdown"}),
        };
        let dest_all = l
            .candidates
            .clone()
            .unwrap_or_else(|| l.target.iter().cloned().collect());
        links.push(json!({
            "source": l.source, "kind": l.kind, "original": l.original, "ordinal": l.ordinal,
            "dest": l.target, "destAll": dest_all, "parsed": {"path": link_path(&l.original)},
            "subpathResult": sp,
        }));
    }
    let probes: Vec<Value> = expected
        .probes
        .iter()
        .map(|p| {
            json!({"source": p.source, "linkpath": p.linkpath, "dest": p.target,
                         "destAll": p.target.iter().cloned().collect::<Vec<_>>()})
        })
        .collect();
    json!({"schema": "glyphvane-oracle/1", "vault": {"name": expected.vault}, "links": links, "probes": probes,
           "obsidian": {"apiVersion": "synthetic"}})
}

/// Runs the oracle comparator (our own script, isolated mode).
fn run_compare(script: &Path, args: &[&Path], extra: &[&str]) -> (i32, String) {
    let out = Command::new("python3")
        .arg("-I")
        .arg(script)
        .args(args)
        .args(extra)
        .output()
        .expect("python3 runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn check_with_comparator(script: &Path, dir: &Path, expected: &ExpectFile) {
    let exp_path = dir.join(format!("{}.expect.json", expected.vault));
    let oracle_path = dir.join(format!("{}.synthetic-oracle.json", expected.vault));
    fs::write(
        &oracle_path,
        serde_json::to_string_pretty(&synthetic_oracle(expected)).expect("test setup"),
    )
    .expect("test setup");
    let (code, out) = run_compare(script, &[&oracle_path, &exp_path], &["--json"]);
    assert_eq!(
        code, 0,
        "compare.py found errors for {}: {out}",
        expected.vault
    );
    let report: Value = serde_json::from_str(&out).expect("test setup");
    assert!(report["counts"].get("error").is_none(), "{report}");
    // The skeleton the comparator seeds from an oracle has our shape.
    let (code, skeleton) = run_compare(script, &[&oracle_path], &["--skeleton"]);
    assert_eq!(code, 0);
    let sk: Value = serde_json::from_str(&skeleton).expect("test setup");
    validate(&sk).unwrap_or_else(|e| panic!("skeleton: {e}"));
    let ours: Value = serde_json::to_value(expected).expect("test setup");
    let our_keys: std::collections::BTreeSet<String> = ours["links"][0]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    for l in sk["links"].as_array().into_iter().flatten() {
        for k in l.as_object().expect("test setup").keys() {
            assert!(
                our_keys.contains(k) || k == "candidates",
                "skeleton key {k} not in our output"
            );
        }
    }
    let sk_path = dir.join(format!("{}.skeleton.json", expected.vault));
    fs::write(&sk_path, skeleton).expect("test setup");
    let (code, out) = run_compare(script, &[&oracle_path, &sk_path], &[]);
    assert_eq!(code, 0, "{out}");
}

fn emit(dir: &Path, file: &ExpectFile) -> Value {
    let text = serde_json::to_string_pretty(file).expect("test setup");
    fs::write(dir.join(format!("{}.expect.json", file.vault)), &text).expect("test setup");
    let v: Value = serde_json::from_str(&text).expect("test setup");
    validate(&v).unwrap_or_else(|e| panic!("{}: {e}", file.vault));
    v
}

fn find<'a>(file: &'a ExpectFile, source: &str, original: &str) -> &'a ExpectLink {
    file.links
        .iter()
        .find(|l| l.source == source && l.original == original)
        .unwrap_or_else(|| panic!("no link {original} in {source}"))
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[test]
fn extractor_skips_code_and_comments() {
    let text = "---\nrel: \"[[A]]\"\n---\n# T\n%% [[B]] %%\n`[[C]]`\n\n    [[D]] indented\n\n```\n[[E]]\n```\n<!-- [[F]] -->\n[[G\\|x]] ![[H|300]] [t](<a b.md>) [r][ref] ![i](p.png)\n\n[ref]: R.md\n";
    let got: Vec<(ExpectKind, String)> =
        extract(text).into_iter().map(|x| (x.kind, x.raw)).collect();
    assert_eq!(
        got,
        [
            (ExpectKind::Frontmatter, "A".to_owned()),
            (ExpectKind::Link, "G|x".to_owned()),
            (ExpectKind::Embed, "H|300".to_owned()),
            (ExpectKind::Link, "a b.md".to_owned()),
            (ExpectKind::Link, "R.md".to_owned()),
            (ExpectKind::Embed, "p.png".to_owned()),
        ]
    );
}

#[test]
fn mini_vault_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("mini");
    let files = [
        ("Note.md", "# Root\n\n## Part 2\n### Detail\n"),
        ("Folder/Note.md", "# Folder note\nPara ^abc\n"),
        (
            "Folder/Source.md",
            "[[Note]] [[./Note]] [[Note#Part 2#Detail]] [[Note#Nope]] [[Folder/Note#^ABC]]\n[x](../Note.md) [[Missing]] ![[pic.png|300]] https://example.com [js](javascript:x)\n",
        ),
        ("a/Meeting.md", ""),
        ("b/Meeting.md", ""),
        ("pic.png", ""),
    ];
    for (p, t) in files {
        let path = vault.join(p);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, t).unwrap();
    }
    fs::create_dir_all(vault.join(".oracle")).unwrap();
    fs::write(
        vault.join(".oracle/probes.json"),
        r#"{"resolve": [{"source": "Note.md", "linkpath": "Meeting"}]}"#,
    )
    .unwrap();
    let (file, _) = expect_for_vault(&vault, "mini");
    let out = tempfile::tempdir().unwrap();
    emit(out.path(), &file);

    let s = "Folder/Source.md";
    assert_eq!(
        find(&file, s, "[[Note]]").target.as_deref(),
        Some("Note.md")
    );
    assert_eq!(
        find(&file, s, "[[./Note]]").target.as_deref(),
        Some("Folder/Note.md")
    );
    assert_eq!(
        find(&file, s, "[[Note#Part 2#Detail]]").subpath,
        Some(gv_vault::expect::ExpectSubpath::Heading {
            heading: "Detail".into(),
            line: 3
        })
    );
    assert_eq!(
        find(&file, s, "[[Note#Nope]]").subpath,
        Some(gv_vault::expect::ExpectSubpath::Missing)
    );
    assert_eq!(
        find(&file, s, "[[Folder/Note#^ABC]]").subpath,
        Some(gv_vault::expect::ExpectSubpath::Block {
            id: "abc".into(),
            line: 1
        })
    );
    assert_eq!(find(&file, s, "[[Missing]]").status, "Unresolved");
    assert_eq!(
        find(&file, s, "![[pic.png|300]]").subpath,
        Some(gv_vault::expect::ExpectSubpath::Passthrough)
    );
    assert_eq!(find(&file, s, "https://example.com").status, "External");
    assert_eq!(find(&file, s, "[js](javascript:x)").status, "Blocked");
    assert_eq!(file.probes[0].status, "Ambiguous(2)");

    if let Some(script) = std::env::var_os("GLYPHVANE_ORACLE_COMPARE") {
        check_with_comparator(Path::new(&script), out.path(), &file);
    }
}

const VAULTS: [&str; 5] = [
    "obsidian-links",
    "obsidian-tiebreak",
    "obsidian-tiebreak-ignore",
    "obsidian-filesystem",
    "obsidian-config",
];

#[test]
fn oracle_fixture_vaults() {
    let Some(root) = std::env::var_os("GLYPHVANE_ORACLE_VAULTS").map(PathBuf::from) else {
        eprintln!("skipped: set GLYPHVANE_ORACLE_VAULTS to the fixture vault directory");
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let out = std::env::var_os("GLYPHVANE_EXPECT_OUT")
        .map_or_else(|| temp.path().to_path_buf(), PathBuf::from);
    fs::create_dir_all(&out).unwrap();
    let mut all = Vec::new();
    for name in VAULTS {
        let vault = root.join(name);
        assert!(vault.is_dir(), "missing fixture vault {}", vault.display());
        let (file, counts) = expect_for_vault(&vault, name);
        emit(&out, &file);
        eprintln!(
            "{name}: {} links, {} probes, {counts:?}",
            file.links.len(),
            file.probes.len()
        );
        assert!(
            !file.links.iter().any(|l| l.status == "Pending"),
            "{name}: Pending in a complete index"
        );
        if let Some(script) = std::env::var_os("GLYPHVANE_ORACLE_COMPARE") {
            check_with_comparator(Path::new(&script), &out, &file);
        }
        all.push(file);
    }
    eprintln!("wrote {} files to {}", all.len(), out.display());

    // Spot checks of the resolution rules (ORACLE-PENDING where Obsidian's
    // behaviour is unverified).
    let links = &all[0];
    let s = "Folder/Source.md";
    // Vault root first, for wikilinks and Markdown links.
    assert_eq!(
        find(links, s, "[[Note]]").target.as_deref(),
        Some("Note.md")
    );
    assert_eq!(
        find(links, s, "[bare](Note.md)").target.as_deref(),
        Some("Note.md")
    );
    // Dot paths are relative only.
    assert_eq!(
        find(links, s, "[[./Note]]").target.as_deref(),
        Some("Folder/Note.md")
    );
    assert_eq!(
        find(links, s, "[[../Note]]").target.as_deref(),
        Some("Note.md")
    );
    // Aliases are not targets.
    assert_eq!(find(links, "Alias links.md", "[[AI]]").status, "Unresolved");
    // `%20` in wikilinks is literal; in Markdown links it is decoded.
    assert_eq!(
        find(links, "Special names.md", "[[My%20Note]]").status,
        "Unresolved"
    );
    assert_eq!(
        find(links, "Special names.md", "[md enc](My%20Note.md)")
            .target
            .as_deref(),
        Some("My Note.md")
    );
    assert_eq!(
        find(links, "Special names.md", "[[Percent%20literal]]")
            .target
            .as_deref(),
        Some("Percent%20literal.md")
    );
    // Extensions are appended, never substituted.
    assert_eq!(
        find(links, "Dots and twins.md", "[[A]]").target.as_deref(),
        Some("A.md")
    );
    assert_eq!(
        find(links, "Dots and twins.md", "[[v1.2 notes]]")
            .target
            .as_deref(),
        Some("v1.2 notes.md")
    );
    // Outside the root and external URLs.
    assert_eq!(
        find(links, "External and outside.md", "[[../outside]]").status,
        "OutsideRoot"
    );
    assert_eq!(
        find(links, "External and outside.md", "[js](javascript:void(0))").status,
        "Blocked"
    );
    // Basename tie-break (ORACLE-PENDING).
    let tb = &all[1];
    assert_eq!(
        find(tb, "Root source.md", "[[Meeting]]").status,
        "Ambiguous(4)"
    );
    assert_eq!(
        find(tb, "Root source.md", "[[Meeting]]").target.as_deref(),
        Some("Work/Meeting.md")
    );
    assert_eq!(
        find(tb, "Zeta/Source.md", "[[Meeting]]").target.as_deref(),
        Some("Zeta/Meeting.md")
    );
    assert_eq!(
        find(tb, "Suffix/Source.md", "[[ive/2025/Meeting]]").status,
        "Unresolved"
    );
    // userIgnoreFilters ranked last.
    let ig = &all[2];
    assert_eq!(
        find(ig, "Root source.md", "[[Meeting]]").target.as_deref(),
        Some("Zeta/Meeting.md")
    );
}
