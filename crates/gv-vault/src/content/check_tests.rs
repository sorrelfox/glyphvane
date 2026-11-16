// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Link checker tests: severities per resolver status, fragments, scopes,
//! fixes, the command line's output and exit codes.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

fn vp(s: &str) -> VaultPath {
    VaultPath::new(s).expect("valid path")
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
    std::fs::write(full, text).expect("write");
}

/// A vault on disk, its snapshot under `profile`.
fn snapshot(root: &Path, profile: ProfileId) -> ContentSnapshot {
    let (paths, _) = PathIndex::build(root, IndexConfig::default()).expect("index");
    let mut options = ContentOptions::for_profile(profile);
    options.threads = 2;
    let source = DiskSource::new(root, &options);
    ContentSnapshot::build(paths, options, &source, &AtomicBool::new(false)).expect("built")
}

fn check(root: &Path, snap: &ContentSnapshot, scope: CheckScope) -> CheckReport {
    let source = DiskSource::new(root, &ContentOptions::for_profile(ProfileId::Extended));
    snap.check_links(
        &scope,
        &CheckOptions::default(),
        Some(&source),
        &CheckProgress::default(),
        &AtomicBool::new(false),
    )
    .expect("not cancelled")
}

/// `(raw, kind, severity)` of every finding.
fn kinds(report: &CheckReport) -> Vec<(String, FindingKind, Severity)> {
    report
        .findings
        .iter()
        .map(|f| (f.raw.to_string(), f.kind, f.severity))
        .collect()
}

fn has(report: &CheckReport, raw: &str, kind: FindingKind, severity: Severity) -> bool {
    report
        .findings
        .iter()
        .any(|f| &*f.raw == raw && f.kind == kind && f.severity == severity)
}

/// An Obsidian vault with one link of each kind of problem.
fn obsidian_vault(root: &Path) {
    std::fs::create_dir_all(root.join(".obsidian")).expect("marker");
    write(
        root,
        "Note.md",
        "# Heading One\n\nSome text ^blk\n\n## Part 2\n### Detail\n",
    );
    write(root, "Work/Meeting.md", "work\n");
    write(root, "Archive/2025/Meeting.md", "archive\n");
    write(root, "README.md", "readme\n");
    write(root, "aux.md", "reserved on Windows\n");
    write(
        root,
        "Artificial Intelligence.md",
        "---\naliases: [AI]\n---\n# Artificial Intelligence\n",
    );
    write(
        root,
        "Home.md",
        "# Home\n\
         [[Note]] [[Note#Heading One]] [[Note#^blk]] [[Note#Part 2#Detail]]\n\
         [[Note#No such heading]] [[Note#^nope]] [[Note#^BLK]]\n\
         [md](Note.md#heading-one) [md2](Note.md#Heading%20One)\n\
         [[Meeting]] [[Missing note]] [[AI]] [[aux]]\n\
         [r](readme.md) [x](javascript:alert(1)) [o](../../outside.md)\n\
         [web](https://example.org) [[#Home]] [[#Nowhere]]\n",
    );
}

#[test]
fn severities_follow_resolver_statuses_in_obsidian() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    obsidian_vault(root);
    let snap = snapshot(root, ProfileId::Obsidian);
    let report = check(root, &snap, CheckScope::Note(vp("Home.md")));
    let k = kinds(&report);
    use FindingKind as K;
    use Severity as S;
    // Fine: exact, heading, block, chain, external, same-note heading.
    for raw in [
        "Note",
        "Note#Heading One",
        "Note#^blk",
        "Note#Part 2#Detail",
        "#Home",
    ] {
        assert!(!k.iter().any(|(r, ..)| r == raw), "{raw}: {k:?}");
    }
    assert!(!k.iter().any(|(r, ..)| r.starts_with("https:")), "{k:?}");
    assert!(has(
        &report,
        "Note#No such heading",
        K::MissingAnchor,
        S::Error
    ));
    assert!(has(&report, "Note#^nope", K::MissingAnchor, S::Error));
    assert!(has(&report, "#Nowhere", K::MissingAnchor, S::Error));
    // Block ids ignoring case, GitHub slugs in Obsidian: loose.
    assert!(has(&report, "Note#^BLK", K::Loose, S::Warning));
    assert!(has(&report, "Note.md#heading-one", K::Loose, S::Warning));
    assert!(
        !k.iter().any(|(r, ..)| r == "Note.md#Heading%20One"),
        "{k:?}"
    );
    assert!(has(&report, "Meeting", K::Ambiguous, S::Warning));
    assert!(has(&report, "Missing note", K::Unresolved, S::Error));
    assert!(has(&report, "AI", K::Unresolved, S::Error));
    assert!(has(&report, "AI", K::AliasHint, S::Info));
    assert!(has(&report, "aux", K::NonPortable, S::Warning));
    // Obsidian ignores case: noted, not warned.
    assert!(has(&report, "readme.md", K::CaseFold, S::Info));
    assert!(has(&report, "javascript:alert(1)", K::Blocked, S::Error));
    assert!(has(&report, "../../outside.md", K::OutsideRoot, S::Warning));

    let ambiguous = report
        .findings
        .iter()
        .find(|f| f.kind == K::Ambiguous)
        .expect("ambiguous");
    assert_eq!(
        ambiguous.candidates,
        [vp("Work/Meeting.md"), vp("Archive/2025/Meeting.md")]
    );
    assert_eq!(ambiguous.status, Status::Ambiguous(2));
    // Positions: 1-based line and column of the construct.
    let missing = report
        .findings
        .iter()
        .find(|f| &*f.raw == "Missing note")
        .expect("missing");
    assert_eq!((missing.line, missing.column), (5, 13));
    assert_eq!(report.exit_code(false), EXIT_ERRORS);
}

#[test]
fn github_profile_warns_about_case_and_appended_extensions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    write(root, "README.md", "# Read me\n\n## Install it\n");
    write(root, "other.md", "o\n");
    write(
        root,
        "docs/a.md",
        "[r](../Readme.md) [o](../other) [s](../README.md#install-it) \
         [t](../README.md#Install-It) [n](../README.md#nope) [ok](../README.md)\n",
    );
    let snap = snapshot(root, ProfileId::GfmFile);
    let report = check(root, &snap, CheckScope::Vault);
    use FindingKind as K;
    use Severity as S;
    assert!(has(&report, "../Readme.md", K::CaseFold, S::Warning));
    assert!(has(&report, "../other", K::Loose, S::Warning));
    assert!(has(
        &report,
        "../README.md#Install-It",
        K::Loose,
        S::Warning
    ));
    assert!(has(
        &report,
        "../README.md#nope",
        K::MissingAnchor,
        S::Error
    ));
    assert_eq!(report.findings.len(), 4, "{:?}", kinds(&report));
    // Fixes: the file's case; the extension.
    let fix = |raw: &str| {
        report
            .findings
            .iter()
            .find(|f| &*f.raw == raw)
            .and_then(|f| f.fixes.first())
            .map(|f| f.action.clone())
    };
    assert_eq!(
        fix("../Readme.md"),
        Some(FixAction::Rewrite {
            path: "../README.md".into()
        })
    );
    assert_eq!(
        fix("../other"),
        Some(FixAction::Rewrite {
            path: "../other.md".into()
        })
    );
    assert_eq!(report.notes, 3);
    assert_eq!(report.links, 6);
}

#[test]
fn scopes_select_notes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    write(root, "a.md", "[[x1]]\n");
    write(root, "f/b.md", "[[x2]] [[x3]]\n");
    write(root, "f/g/c.md", "[[x4]]\n");
    write(root, "h/d.md", "[[x5]]\n");
    let snap = snapshot(root, ProfileId::Obsidian);
    let count = |scope| check(root, &snap, scope).findings.len();
    assert_eq!(count(CheckScope::Note(vp("a.md"))), 1);
    assert_eq!(count(CheckScope::Folder(vp("f"))), 3);
    assert_eq!(count(CheckScope::Folder(vp("f/g"))), 1);
    assert_eq!(count(CheckScope::Vault), 5);
    assert_eq!(count(CheckScope::Note(vp("nope.md"))), 0);
    let progress = CheckProgress::default();
    snap.check_links(
        &CheckScope::Vault,
        &CheckOptions::default(),
        None,
        &progress,
        &AtomicBool::new(false),
    )
    .expect("checked");
    assert_eq!(progress.total.load(Ordering::Relaxed), 4);
    assert_eq!(progress.done.load(Ordering::Relaxed), 4);
    // Cancelled.
    let cancelled = snap.check_links(
        &CheckScope::Vault,
        &CheckOptions::default(),
        None,
        &progress,
        &AtomicBool::new(true),
    );
    assert!(cancelled.is_err());
}

#[test]
fn fixes_rewrite_links_and_create_notes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    obsidian_vault(root);
    write(
        root,
        "Sub/Source.md",
        "[[Folder/New idea]] [[Plain]] [x](../Rel%20note.md) [[CON]]\n",
    );
    let snap = snapshot(root, ProfileId::Obsidian);
    let report = check(root, &snap, CheckScope::Vault);
    let fixes = |raw: &str, kind: FindingKind| -> Vec<FixAction> {
        report
            .findings
            .iter()
            .find(|f| &*f.raw == raw && f.kind == kind)
            .map(|f| f.fixes.iter().map(|x| x.action.clone()).collect())
            .unwrap_or_default()
    };
    let rewrite = |p: &str| FixAction::Rewrite { path: p.into() };
    let create = |p: &str| FixAction::CreateNote { path: vp(p) };
    assert_eq!(
        fixes("Meeting", FindingKind::Ambiguous),
        [rewrite("Work/Meeting"), rewrite("2025/Meeting")]
    );
    assert_eq!(
        fixes("readme.md", FindingKind::CaseFold),
        [rewrite("README.md")]
    );
    assert_eq!(
        fixes("AI", FindingKind::AliasHint),
        [rewrite("Artificial Intelligence")]
    );
    assert_eq!(
        fixes("Missing note", FindingKind::Unresolved),
        [create("Missing note.md")]
    );
    // The folder written in the link (from the vault root); the source's
    // folder; relative paths.
    assert_eq!(
        fixes("Folder/New idea", FindingKind::Unresolved),
        [create("Folder/New idea.md")]
    );
    assert_eq!(
        fixes("Plain", FindingKind::Unresolved),
        [create("Sub/Plain.md")]
    );
    assert_eq!(
        fixes("../Rel%20note.md", FindingKind::Unresolved),
        [create("Rel note.md")]
    );
    // Not a valid name everywhere: no offer.
    assert_eq!(fixes("CON", FindingKind::Unresolved), []);

    // Rewriting the link text keeps fragments and display text.
    assert_eq!(
        rewrite_link(
            "[[Meeting#Notes|the meeting]]",
            LinkKind::Wikilink,
            "Meeting#Notes|the meeting",
            "Work/Meeting"
        ),
        Some("[[Work/Meeting#Notes|the meeting]]".into())
    );
    assert_eq!(
        rewrite_link("![[ meeting ]]", LinkKind::Embed, "meeting", "Meeting"),
        Some("![[ Meeting ]]".into())
    );
    assert_eq!(
        rewrite_link(
            "[see](readme.md#top \"t\")",
            LinkKind::Inline,
            "readme.md#top",
            "README.md"
        ),
        Some("[see](README.md#top \"t\")".into())
    );
    assert_eq!(
        rewrite_link("[see][ref]", LinkKind::Reference, "readme.md", "README.md"),
        None
    );
}

#[test]
fn sync_conflict_copies_are_skipped() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    write(root, "a.md", "# Old\n");
    write(root, "b.md", "[[a#New]]\n");
    write(root, "b (conflicted copy 2026-01-02).md", "[[nothing]]\n");
    let snap = snapshot(root, ProfileId::Obsidian);
    let report = check(root, &snap, CheckScope::Vault);
    assert_eq!(
        kinds(&report),
        [("a#New".into(), FindingKind::MissingAnchor, Severity::Error)]
    );
}

#[test]
fn profile_divergence_is_reported_on_request() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    write(root, "sub/x.md", "x\n");
    write(root, "x.md", "root x\n");
    write(root, "sub/a.md", "[x](x.md) [y](../x.md)\n");
    let snap = snapshot(root, ProfileId::Obsidian);
    let report = snap
        .check_links(
            &CheckScope::Vault,
            &CheckOptions {
                compare_with: Some(ProfileId::GfmFile),
            },
            None,
            &CheckProgress::default(),
            &AtomicBool::new(false),
        )
        .expect("checked");
    // Obsidian tries the vault root first; GitHub the source's folder.
    assert_eq!(
        kinds(&report),
        [(
            "x.md".into(),
            FindingKind::ProfileDivergent,
            Severity::Warning
        )]
    );
}

#[test]
fn heading_matching_against_the_index() {
    let h = |level: u8, text: &str| IndexedHeading {
        level,
        text: text.into(),
        source: text.into(),
        span: Span::default(),
        line: 0,
    };
    let hs = [
        h(1, "Intro"),
        h(2, "Dup"),
        h(3, "Detail"),
        h(2, "Dup"),
        h(1, "Time: 10:30"),
    ];
    let gh = Anchoring::Slug(SlugAlgorithm::GitHub);
    let m = |parts: &[&str], a| {
        let parts: Vec<String> = parts.iter().map(|s| (*s).to_owned()).collect();
        match_heading(&hs, &parts, a)
    };
    assert_eq!(m(&["dup-1"], gh), AnchorMatch::Exact);
    assert_eq!(m(&["dup-2"], gh), AnchorMatch::Missing);
    assert_eq!(m(&["user-content-detail"], gh), AnchorMatch::Exact);
    assert_eq!(m(&["time-1030"], gh), AnchorMatch::Exact);
    assert_eq!(m(&["Time-1030"], gh), AnchorMatch::Loose);
    assert_eq!(m(&["top"], gh), AnchorMatch::Top);
    assert_eq!(
        m(&["Intro#Detail"], Anchoring::HeadingText),
        AnchorMatch::Missing
    );
    assert_eq!(
        m(&["Intro", "Detail"], Anchoring::HeadingText),
        AnchorMatch::Exact
    );
    assert_eq!(
        m(&["Detail", "Intro"], Anchoring::HeadingText),
        AnchorMatch::Missing
    );
    assert_eq!(
        m(&["time 10 30"], Anchoring::HeadingText),
        AnchorMatch::Exact
    );
    assert_eq!(m(&["intro"], gh), AnchorMatch::Exact);
    assert_eq!(m(&["Intro"], Anchoring::None), AnchorMatch::Loose);
}

// ----- The command line's output ----------------------------------------------

/// A small generated vault for the golden files.
fn cli_vault(root: &Path) {
    std::fs::create_dir_all(root.join(".obsidian")).expect("marker");
    write(
        root,
        "Index.md",
        "# Index\n[[Topic 1]] [[Topic 2#Overview]] [[Topic 3]] [[Leaf]]\n",
    );
    write(
        root,
        "Topic 1.md",
        "# Topic 1\n## Overview\n[[Index]] [[Topic 2#Missing part]]\n",
    );
    write(
        root,
        "Topic 2.md",
        "# Topic 2\n## Overview\n[[index]] [x](Topic%201.md#overview) [o](../outside.md)\n",
    );
    write(root, "sub/Topic 1.md", "copy of a name\n");
    write(root, "deep/er/Leaf.md", "another leaf\n");
    write(root, "sub/Leaf.md", "[[Topic 1]] [\"q\"](../nowhere.md)\n");
}

#[test]
fn command_line_output_and_exit_codes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    cli_vault(root);
    let run = check_path(root, None, &CheckOptions::default()).expect("checked");
    assert_eq!(run.profile, ProfileId::Obsidian);
    assert_eq!(run.scope, CheckScope::Vault);
    assert_eq!(run.report.exit_code(false), EXIT_ERRORS);

    let golden = |name: &str, got: &str| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/check-links")
            .join(name);
        if std::env::var_os("GV_BLESS").is_some() {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, got).expect("bless");
        }
        let want = std::fs::read_to_string(&path).expect("golden file (GV_BLESS=1 writes it)");
        assert_eq!(got, want, "{name} differs from the golden file");
    };
    golden("vault.json", &run.report.to_json());
    golden("vault.txt", &run.report.to_text());

    // A folder; a note; warnings only.
    let folder = check_path(&root.join("sub"), None, &CheckOptions::default()).expect("checked");
    assert_eq!(folder.scope, CheckScope::Folder(vp("sub")));
    assert_eq!(folder.root, std::path::absolute(root).expect("abs"));
    let note = check_path(&root.join("Topic 2.md"), None, &CheckOptions::default()).expect("ok");
    assert_eq!(note.scope, CheckScope::Note(vp("Topic 2.md")));
    golden("topic-2.json", &note.report.to_json());
    assert_eq!(note.report.exit_code(false), EXIT_CLEAN);
    assert_eq!(note.report.exit_code(true), EXIT_WARNINGS);
    // GitHub's rules on the same vault.
    let gh = check_path(root, Some(ProfileId::GfmFile), &CheckOptions::default()).expect("ok");
    assert_eq!(gh.profile, ProfileId::GfmFile);
    golden("vault-gfm-file.txt", &gh.report.to_text());
    // Nothing there.
    assert!(check_path(&root.join("nope"), None, &CheckOptions::default()).is_err());
    // A clean vault.
    let clean = tempfile::tempdir().expect("temp dir");
    write(clean.path(), "a.md", "[[b]]\n");
    write(clean.path(), "b.md", "b\n");
    let report = check_path(clean.path(), None, &CheckOptions::default())
        .expect("ok")
        .report;
    assert_eq!(report.to_json(), "[]\n");
    assert_eq!(
        report.to_text(),
        "0 errors, 0 warnings, 0 info messages in 2 files (1 link)\n"
    );
    assert_eq!(report.exit_code(true), EXIT_CLEAN);
}

#[test]
fn json_strings_are_escaped() {
    assert_eq!(json_str("a\"b\\c\nd\u{1}é"), "\"a\\\"b\\\\c\\nd\\u0001é\"");
}

/// The link checker on a generated 10,000-note vault (run with
/// `--ignored --nocapture` for the timings).
#[test]
#[ignore = "performance report"]
fn ten_thousand_notes() {
    use std::time::Instant;
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    std::fs::create_dir_all(root.join(".obsidian")).expect("marker");
    let n = 10_000;
    for i in 0..n {
        let folder = format!("f{}", i % 50);
        let mut text = format!("# Note {i}\n\n## Part A\n\nText ^b{i}\n\n## Part B\n");
        for k in 1..=8 {
            let j = (i * 7 + k * 131) % n;
            let _ = writeln!(
                text,
                "- [[Note {j}]] [[Note {j}#Part A]] [m](../f{}/Note%20{j}.md#part-b)",
                j % 50
            );
        }
        let _ = writeln!(
            text,
            "[[Missing {i}]] [[Note {}#Nope]] [[note {}]]",
            (i + 1) % n,
            (i + 2) % n
        );
        write(root, &format!("{folder}/Note {i}.md"), &text);
    }
    let started = Instant::now();
    let (paths, _) = PathIndex::build(root, IndexConfig::default()).expect("index");
    let walk = started.elapsed();
    let options = ContentOptions::for_profile(ProfileId::Obsidian);
    let threads = options.threads;
    let source = DiskSource::new(root, &options);
    let started = Instant::now();
    let snap =
        ContentSnapshot::build(paths, options, &source, &AtomicBool::new(false)).expect("ok");
    let index = started.elapsed();
    let started = Instant::now();
    let report = snap
        .check_links(
            &CheckScope::Vault,
            &CheckOptions::default(),
            Some(&source),
            &CheckProgress::default(),
            &AtomicBool::new(false),
        )
        .expect("checked");
    let vault = started.elapsed();
    let started = Instant::now();
    let note = snap
        .check_links(
            &CheckScope::Note(vp("f0/Note 0.md")),
            &CheckOptions::default(),
            Some(&source),
            &CheckProgress::default(),
            &AtomicBool::new(false),
        )
        .expect("checked");
    let one = started.elapsed();
    let started = Instant::now();
    let json = report.to_json();
    let format = started.elapsed();
    println!(
        "10k notes, {} links, {} findings ({} errors), {threads} threads: walk {walk:?}, \
         content index {index:?}, check vault {vault:?}, check one note {one:?} ({} findings), \
         JSON {format:?} ({} bytes)",
        report.links,
        report.findings.len(),
        report.count(Severity::Error),
        note.findings.len(),
        json.len()
    );
    assert_eq!(report.notes, n);
    assert_eq!(report.count(Severity::Error), 2 * n);
}
