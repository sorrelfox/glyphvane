// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! GitHub heading anchors against GitHub's own rendering.
//!
//! Runs when `GLYPHVANE_GITHUB_ORACLE` names a directory with
//! `readmes/<name>.md` (README sources) and `oracle/<name>.html` (the same
//! READMEs as GitHub renders them, `Accept: application/vnd.github.html`);
//! otherwise it only says it was skipped. The corpus is not part of the
//! repository.
//!
//! GitHub gives every heading an anchor `<a id="user-content-SLUG"
//! class="anchor">`, Markdown headings and raw HTML `<h1>`–`<h6>` alike, and
//! both share one duplicate counter. The model has the Markdown headings
//! only (raw HTML is the HTML pipeline's), so the comparison aligns the
//! model's slugs, in order, with GitHub's ids: each slug must be found
//! among the ids not yet consumed, in order. Mismatches are listed with the
//! heading text. `GLYPHVANE_GITHUB_ORACLE_STRICT=1` fails on any.
//!
//! The same corpus also checks the GitLab algorithm against comrak (which
//! GitLab renders with) on every README: comrak's anchorizer over comrak's
//! heading text must equal the model's GitLab slugs, and both must find
//! the same number of links.

use std::path::{Path, PathBuf};

use comrak::nodes::{AstNode, NodeValue};
use gv_model::{DocModel, SlugAlgorithm};

/// The heading anchors of a GitHub-rendered page: (id without prefix,
/// heading text with tags stripped).
fn oracle_ids(html: &str) -> Vec<(String, String)> {
    const OPEN: &str = "<a id=\"user-content-";
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(i) = html[pos..].find(OPEN) {
        let start = pos + i + OPEN.len();
        let Some(q) = html[start..].find('"') else {
            break;
        };
        let id = &html[start..start + q];
        let tag_end = html[start..].find('>').map_or(html.len(), |e| start + e);
        pos = tag_end;
        if !html[start..tag_end].contains("class=\"anchor\"") {
            continue;
        }
        // The heading element just before the anchor.
        let before = &html[..start];
        let text = before
            .rfind("<h")
            .map(|h| strip_tags(&before[h..]))
            .unwrap_or_default();
        out.push((unescape_html(id), text));
    }
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    unescape_html(out.trim())
}

fn unescape_html(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

struct Outcome {
    headings: usize,
    matched: usize,
    /// GitHub ids passed over by the alignment (raw HTML headings).
    skipped: usize,
    mismatches: Vec<String>,
}

fn compare(md: &str, html: &str) -> Outcome {
    // GitHub's README rendering: YAML front matter is not Markdown.
    let options = gv_syntax::ParseOptions::builtin(gv_syntax::ProfileId::GfmFile);
    let tree = gv_syntax::parse_with(md, options);
    let m = DocModel::build(&tree, md);
    let slugs = m.slugs(SlugAlgorithm::GitHub);
    let ids = oracle_ids(html);
    let mut next = 0;
    let mut matched = 0;
    let mut mismatches = Vec::new();
    for (h, slug) in m.headings().iter().zip(slugs.iter()) {
        match ids[next..].iter().position(|(id, _)| id == &**slug) {
            Some(k) => {
                next += k + 1;
                matched += 1;
            }
            None => {
                // The nearest unconsumed id with the same heading text, for
                // the report.
                let near = ids[next..]
                    .iter()
                    .find(|(_, t)| t.trim() == h.text.trim())
                    .map_or("-", |(id, _)| id.as_str());
                mismatches.push(format!(
                    "line {}: {:?} -> ours {:?}, GitHub {:?}",
                    h.line + 1,
                    h.text,
                    slug,
                    near
                ));
            }
        }
    }
    Outcome {
        headings: m.headings().len(),
        matched,
        skipped: ids.len() - matched,
        mismatches,
    }
}

#[test]
fn github_rendered_readmes() {
    let Some(dir) = std::env::var_os("GLYPHVANE_GITHUB_ORACLE") else {
        eprintln!("skipped: set GLYPHVANE_GITHUB_ORACLE to the corpus directory");
        return;
    };
    let dir = PathBuf::from(dir);
    let mut names: Vec<String> = std::fs::read_dir(dir.join("oracle"))
        .expect("oracle directory")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            (p.extension()? == "html").then(|| p.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    names.sort();
    let (mut headings, mut matched, mut skipped, mut files) = (0, 0, 0, 0);
    let mut report = Vec::new();
    for name in &names {
        let md = read(&dir.join("readmes").join(format!("{name}.md")));
        let html = read(&dir.join("oracle").join(format!("{name}.html")));
        let o = compare(&md, &html);
        files += 1;
        headings += o.headings;
        matched += o.matched;
        skipped += o.skipped;
        if !o.mismatches.is_empty() {
            report.push(format!("{name}: {}/{}", o.matched, o.headings));
            report.extend(o.mismatches.iter().map(|m| format!("  {m}")));
        }
    }
    eprintln!(
        "{files} READMEs, {headings} Markdown headings, {matched} slugs equal to GitHub's ids, \
         {skipped} GitHub ids passed over (raw HTML headings)"
    );
    for line in &report {
        eprintln!("{line}");
    }
    if std::env::var_os("GLYPHVANE_GITHUB_ORACLE_STRICT").is_some() {
        assert_eq!(matched, headings);
    }
}

fn comrak_walk<'a>(n: &'a AstNode<'a>, f: &mut impl FnMut(&'a AstNode<'a>)) {
    f(n);
    for c in n.children() {
        comrak_walk(c, f);
    }
}

#[test]
fn gitlab_slugs_on_readmes_match_comrak() {
    let Some(dir) = std::env::var_os("GLYPHVANE_GITHUB_ORACLE") else {
        eprintln!("skipped: set GLYPHVANE_GITHUB_ORACLE to the corpus directory");
        return;
    };
    let mut paths: Vec<PathBuf> = std::fs::read_dir(PathBuf::from(dir).join("readmes"))
        .expect("readmes directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    paths.sort();
    let (mut headings, mut equal, mut links, mut link_diffs) = (0, 0, 0, 0);
    let mut report = Vec::new();
    for p in &paths {
        let md = read(p);
        let m = DocModel::build(&gv_syntax::parse(&md), &md);
        let arena = comrak::Arena::new();
        let root = comrak::parse_document(&arena, &md, &comrak::Options::default());
        let mut anchorizer = comrak::Anchorizer::new();
        let mut ids = Vec::new();
        let mut n_links = 0;
        comrak_walk(root, &mut |n| match &n.data().value {
            NodeValue::Heading(_) => ids.push(anchorizer.anchorize(&n.collect_text())),
            NodeValue::Link(_) | NodeValue::Image(_) => n_links += 1,
            _ => {}
        });
        let ours = m.slugs(SlugAlgorithm::GitLab);
        headings += ours.len();
        links += m.links().len();
        if m.links().len() != n_links {
            link_diffs += 1;
            report.push(format!(
                "{}: {} links, comrak {n_links}",
                p.display(),
                m.links().len()
            ));
        }
        if ours.len() != ids.len() {
            report.push(format!(
                "{}: {} headings, comrak {}",
                p.display(),
                ours.len(),
                ids.len()
            ));
            continue;
        }
        for ((o, c), h) in ours.iter().zip(&ids).zip(m.headings()) {
            if **o == **c {
                equal += 1;
            } else {
                report.push(format!(
                    "{}:{}: ours {o:?}, comrak {c:?} ({:?})",
                    p.display(),
                    h.line + 1,
                    h.text
                ));
            }
        }
    }
    eprintln!(
        "{} READMEs, {headings} headings, {equal} GitLab slugs equal to comrak's, \
         {links} links, {link_diffs} files with a different link count",
        paths.len()
    );
    for line in &report {
        eprintln!("{line}");
    }
    if std::env::var_os("GLYPHVANE_GITHUB_ORACLE_STRICT").is_some() {
        assert!(report.is_empty());
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn oracle_parser() {
    let html = "<div class=\"markdown-heading\" dir=\"auto\"><h2 class=\"heading-element\" dir=\"auto\">A &amp; <code>b</code></h2><a id=\"user-content-a--b\" class=\"anchor\" aria-label=\"Permalink\" href=\"#a--b\"></a></div><a id=\"user-content-other\">x</a>";
    assert_eq!(oracle_ids(html), [("a--b".to_owned(), "A & b".to_owned())]);
    let o = compare("## A & `b`\n", html);
    assert_eq!((o.headings, o.matched), (1, 1));
}
