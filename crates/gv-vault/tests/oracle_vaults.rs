// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Obsidian oracle fixture vaults through the parser, the document model,
//! the path index and the resolver, emitted as `glyphvane-expect/1`.
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
//! Markdown links, images and autolinks come from the document model
//! ([`links_from_model`]); heading fragments are matched by
//! [`ModelFragments`]. The parser has no Obsidian syntax yet, so three
//! small stand-ins remain here, each covering only what the fixtures use:
//!
//! - front matter is cut off before parsing, and its values are parsed as
//!   Markdown on their own (`frontmatter` links);
//! - wikilinks, embeds, `%%` comments and bare URLs are found by a text scan
//!   of the paragraph and heading lines the parser reports, outside code
//!   spans, raw HTML and Markdown links;
//! - block ids (`^id`) are found by a line scan.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

use gv_model::DocModel;
use gv_syntax::inline::InlineKind;
use gv_syntax::{Leaf, LeafKind, SyntaxTree, Visitor};
use gv_vault::expect::{
    EXPECT_SCHEMA, ExpectFile, ExpectKind, ExpectLink, ExpectProbe, is_valid_status,
};
use gv_vault::{
    Fragment, FragmentKind, FragmentResolver, FragmentTarget, IndexConfig, LinkKind, LinkRef,
    ModelFragments, PathIndex, ProfileId, ProfileParams, Status, VaultPath, links_from_model,
    resolve,
};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Parsing.
// ---------------------------------------------------------------------------

/// A leading YAML front matter block: the range of its content (between
/// the fences) and of the whole block (closing fence line included).
fn front_matter(text: &str) -> Option<(Range<usize>, Range<usize>)> {
    let body = text.strip_prefix("---\n")?;
    let end = body.find("\n---")? + 4;
    let close = end + 1 + 3;
    let after = text[close..].find('\n').map_or(text.len(), |n| close + n);
    Some((4..end, 0..after))
}

/// `text` with the bytes in `ranges` (line endings excepted) replaced by
/// spaces: byte offsets and line numbers stay the same.
fn blank(text: &str, ranges: &[Range<usize>]) -> String {
    let mut bytes = text.as_bytes().to_vec();
    for r in ranges {
        for b in &mut bytes[r.clone()] {
            if !matches!(*b, b'\n' | b'\r') {
                *b = b' ';
            }
        }
    }
    String::from_utf8(bytes).expect("ASCII replacements of whole ranges")
}

/// The text the parser sees: front matter blanked.
// TODO(gv-syntax front matter): parse the front matter block instead, once
// the parser has one. Until then a leading `---…---` block would read as a
// thematic break and a setext heading.
fn parsed_text(text: &str) -> String {
    match front_matter(text) {
        Some((_, block)) => blank(text, &[block]),
        None => text.to_owned(),
    }
}

fn parse(text: &str) -> (SyntaxTree, DocModel) {
    let tree = gv_syntax::parse(text);
    let model = DocModel::build(&tree, text);
    (tree, model)
}

// ---------------------------------------------------------------------------
// Links: the model's, plus a scan for Obsidian syntax.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Extracted {
    kind: ExpectKind,
    start: usize,
    original: String,
    link: LinkRef,
}

/// The content lines of paragraphs and headings, and the ranges inside
/// them the scan must skip (code spans, raw HTML).
struct TextLines<'a> {
    tree: &'a SyntaxTree,
    text: &'a str,
    lines: Vec<Range<usize>>,
    masked: Vec<Range<usize>>,
}

impl Visitor for TextLines<'_> {
    fn leaf(&mut self, leaf: &Leaf<'_>) {
        if !matches!(leaf.kind, LeafKind::Paragraph | LeafKind::Heading { .. }) {
            return;
        }
        self.lines
            .extend(leaf.lines.iter().map(|l| l.content.clone()));
        let inlines = self.tree.inlines(leaf, self.text);
        self.masked.extend(
            inlines
                .tokens()
                .filter(|(k, _)| matches!(k, InlineKind::CodeText | InlineKind::HtmlText))
                .map(|(_, r)| r),
        );
    }
}

fn inside(ranges: &[Range<usize>], at: usize) -> bool {
    ranges.iter().any(|r| r.contains(&at))
}

/// Obsidian comments (`%%…%%`, possibly over several lines and blocks):
/// `%%` pairs in document order, outside masked ranges.
// TODO(gv-syntax profile hooks): Obsidian comments in the parser.
fn comments(text: &str, lines: &[Range<usize>], masked: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut marks = Vec::new();
    for line in lines {
        let mut i = line.start;
        while let Some(n) = text[i..line.end].find("%%") {
            let at = i + n;
            if !inside(masked, at) {
                marks.push(at);
            }
            i = at + 2;
        }
    }
    marks
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[open, close]| *open..*close + 2)
        .collect()
}

/// Wikilinks, embeds and bare URLs in one content line.
// TODO(gv-syntax profile hooks): wikilinks, embeds and extended autolinks
// from the parser and the model.
fn scan_line(
    text: &str,
    line: Range<usize>,
    skip: &[Range<usize>],
    kind: ExpectKind,
    source: &VaultPath,
    out: &mut Vec<Extracted>,
) {
    let mut push = |kind, start: usize, end: usize, link_kind, raw: String| {
        out.push(Extracted {
            kind,
            start,
            original: text[start..end].to_owned(),
            link: LinkRef::new(link_kind, raw, source.clone(), ProfileId::Obsidian),
        });
    };
    let mut i = line.start;
    while i < line.end {
        if inside(skip, i) || !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &text[i..line.end];
        let embed = rest.starts_with("![[");
        let r = if embed { &rest[1..] } else { rest };
        if let Some(after_open) = r.strip_prefix("[[")
            && let Some(close) = after_open.find("]]")
        {
            let end = i + usize::from(embed) + 2 + close + 2;
            let (k, lk) = if embed {
                (ExpectKind::Embed, LinkKind::Embed)
            } else {
                (kind, LinkKind::Wikilink)
            };
            push(k, i, end, lk, after_open[..close].replace("\\|", "|"));
            i = end;
            continue;
        }
        if rest.starts_with("https://") || rest.starts_with("http://") {
            let end = i + rest.find([' ', ')', '>']).unwrap_or(rest.len());
            push(kind, i, end, LinkKind::Autolink, text[i..end].to_owned());
            i = end;
            continue;
        }
        i += 1;
    }
}

/// Every link of a parsed `text`, in document order.
fn extract_parsed(text: &str, kind: ExpectKind, source: &VaultPath) -> Vec<Extracted> {
    let (tree, model) = parse(text);
    let mut v = TextLines {
        tree: &tree,
        text,
        lines: Vec::new(),
        masked: Vec::new(),
    };
    gv_syntax::walk(&tree, &mut v);
    let hidden = comments(text, &v.lines, &v.masked);
    let mut out = Vec::new();
    let refs = links_from_model(&model, source, ProfileId::Obsidian);
    for (l, link) in model.links().iter().zip(refs) {
        if inside(&hidden, l.range.start) {
            continue;
        }
        out.push(Extracted {
            kind: if l.image { ExpectKind::Embed } else { kind },
            start: l.range.start,
            original: text[l.range.clone()].to_owned(),
            link,
        });
    }
    let mut skip = v.masked;
    skip.extend(hidden);
    skip.extend(model.links().iter().map(|l| l.range.clone()));
    for line in v.lines {
        scan_line(text, line, &skip, kind, source, &mut out);
    }
    out.sort_by_key(|x| x.start);
    out
}

/// Every link of a note: front matter values first, then the body.
// TODO(gv-model properties): front matter links from the model.
fn extract(text: &str, source: &VaultPath) -> Vec<Extracted> {
    let Some((content, block)) = front_matter(text) else {
        return extract_parsed(text, ExpectKind::Link, source);
    };
    let values = blank(text, &[0..content.start, content.end..text.len()]);
    let mut out = extract_parsed(&values, ExpectKind::Frontmatter, source);
    out.extend(extract_parsed(
        &blank(text, &[block]),
        ExpectKind::Link,
        source,
    ));
    out
}

// ---------------------------------------------------------------------------
// Fragments: the model's, plus a block-id scan.
// ---------------------------------------------------------------------------

/// Block ids (`^id` at the end of a line, after a space or alone) with
/// their 0-based lines, outside fenced code.
// TODO(gv-model block ids): the Obsidian profile's `DocModel::block_anchors`.
fn block_ids(text: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut fence = false;
    for (n, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        if let Some(pos) = line.rfind('^') {
            let id = &line[pos + 1..];
            let before = &line[..pos];
            if !id.is_empty()
                && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                && (before.is_empty() || before.ends_with(' '))
            {
                out.push((id.to_owned(), u32::try_from(n).unwrap_or(u32::MAX)));
            }
        }
    }
    out
}

/// Notes of a vault, parsed once each.
struct Notes {
    root: PathBuf,
    cache: RefCell<HashMap<VaultPath, Option<(String, DocModel)>>>,
}

impl Notes {
    fn get(&self, path: &VaultPath) -> Option<(String, DocModel)> {
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            return None;
        }
        self.cache
            .borrow_mut()
            .entry(path.clone())
            .or_insert_with(|| {
                let text = parsed_text(&fs::read_to_string(path.to_fs_path(&self.root)).ok()?);
                let (_, model) = parse(&text);
                Some((text, model))
            })
            .clone()
    }
}

/// [`ModelFragments`] over the vault's notes, with block ids from
/// [`block_ids`] while the model has none.
struct VaultFragments<'a, F> {
    notes: &'a Notes,
    model: ModelFragments<F>,
}

impl<F: Fn(&VaultPath) -> Option<DocModel>> FragmentResolver for VaultFragments<'_, F> {
    fn resolve_fragment(
        &self,
        target: &VaultPath,
        fragment: &Fragment,
        params: &ProfileParams,
    ) -> FragmentTarget {
        let FragmentKind::Block(id) = &fragment.kind else {
            return self.model.resolve_fragment(target, fragment, params);
        };
        let Some((text, model)) = self.notes.get(target) else {
            return FragmentTarget::Unchecked;
        };
        if !model.block_anchors().is_empty() {
            return self.model.resolve_fragment(target, fragment, params);
        }
        let ids = block_ids(&text);
        let lower = id.to_lowercase();
        let exact = ids.iter().find(|(b, _)| b == id).map(|x| (x, false));
        match exact.or_else(|| {
            ids.iter()
                .find(|(b, _)| b.to_lowercase() == lower)
                .map(|x| (x, true))
        }) {
            Some(((bid, line), loose)) => FragmentTarget::Block {
                id: bid.clone(),
                line: *line,
                loose,
            },
            None => FragmentTarget::Missing,
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
    let notes = Notes {
        root: vault.to_path_buf(),
        cache: RefCell::new(HashMap::new()),
    };
    let fragments = VaultFragments {
        notes: &notes,
        model: ModelFragments::new(|p: &VaultPath| notes.get(p).map(|(_, m)| m)),
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
        for x in extract(&text, &src.path) {
            let kind_name = format!("{:?}", x.kind);
            let ord = ordinals.entry((kind_name, x.original.clone())).or_insert(0);
            let r = resolve(&idx, &x.link, &params);
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
            // `loose` is optional and only written when true.
            let loose_ok = match sp.get("loose") {
                None => true,
                Some(b) => {
                    b.as_bool() == Some(true)
                        && matches!(sp["kind"].as_str(), Some("heading" | "block"))
                }
            };
            if !ok || !loose_ok {
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
            Some(gv_vault::expect::ExpectSubpath::Heading { heading, line, .. }) => {
                json!({"type": "heading", "heading": heading, "line": line})
            }
            Some(gv_vault::expect::ExpectSubpath::Block { id, line, .. }) => {
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
    let text = "---\nrel: \"[[A]]\"\n---\n# T\n%% [[B]] [b](B.md) %%\n`[[C]]`\n\n    [[D]] indented\n\n```\n[[E]]\n```\n<!-- [[F]] -->\n[[G\\|x]] ![[H|300]] [t](<a b.md>) [r][ref] ![i](p.png) <https://x.org> https://y.org <i>[[I]]</i>\n\n[ref]: R.md\n";
    let source = VaultPath::new("S.md").unwrap();
    let got: Vec<(ExpectKind, LinkKind, String)> = extract(text, &source)
        .into_iter()
        .map(|x| (x.kind, x.link.kind, x.link.raw_destination))
        .collect();
    let want = [
        (ExpectKind::Frontmatter, LinkKind::Wikilink, "A"),
        (ExpectKind::Link, LinkKind::Wikilink, "G|x"),
        (ExpectKind::Embed, LinkKind::Embed, "H|300"),
        (ExpectKind::Link, LinkKind::Inline, "a b.md"),
        (ExpectKind::Link, LinkKind::Reference, "R.md"),
        (ExpectKind::Embed, LinkKind::Image, "p.png"),
        (ExpectKind::Link, LinkKind::Autolink, "https://x.org"),
        (ExpectKind::Link, LinkKind::Autolink, "https://y.org"),
        (ExpectKind::Link, LinkKind::Wikilink, "I"),
    ];
    assert_eq!(got, want.map(|(k, l, r)| (k, l, r.to_owned())));
}

#[test]
fn front_matter_is_blanked_for_the_parser() {
    let text = "---\ntitle: x\n---\n# Heading\n";
    let parsed = parsed_text(text);
    assert_eq!(parsed.len(), text.len());
    let (_, model) = parse(&parsed);
    let h = &model.headings()[0];
    assert_eq!((&*h.source, h.line, h.level), ("Heading", 3, 1));
    assert_eq!(model.headings().len(), 1);
    // Without blanking it is a thematic break and a setext heading.
    let (_, raw) = parse(text);
    assert_eq!(&*raw.headings()[0].source, "title: x");
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
            "[[Note]] [[./Note]] [[Note#Part 2#Detail]] [[Note#Nope]] [[Folder/Note#^ABC]] [[Note#part-2]]\n[x](../Note.md) [[Missing]] ![[pic.png|300]] https://example.com [js](javascript:x)\n",
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
            line: 3,
            loose: false
        })
    );
    assert_eq!(
        find(&file, s, "[[Note#part-2]]").subpath,
        Some(gv_vault::expect::ExpectSubpath::Heading {
            heading: "Part 2".into(),
            line: 2,
            loose: true
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
            line: 1,
            loose: true
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
