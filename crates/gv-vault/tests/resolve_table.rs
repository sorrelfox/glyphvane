// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Table-driven tests of the link-resolution rules: decoding examples,
//! statuses, every algorithm step, per-profile parameters, special targets,
//! URL schemes, repository URLs and the link-checker examples.
//!
//! Rows marked ORACLE-PENDING encode the documented best guess for an
//! Obsidian behaviour that the Obsidian oracle has not confirmed yet.

use std::sync::Arc;

use gv_vault::fragment::FragmentKind;
use gv_vault::link::{DecodedTarget, ImageSize, decode};
use gv_vault::repo::{RepoContext, RepoRemote};
use gv_vault::url::{BlockReason, ConfirmReason, UrlClass, UrlPolicy};
use gv_vault::{
    Change, EntryKind, FileMeta, IndexConfig, IndexState, LinkKind, LinkRef, Note, PathIndex,
    ProfileId, ProfileParams, ResolveSettings, Status, SymlinkTarget, TargetKind, VaultPath,
    resolve,
};

fn vp(s: &str) -> VaultPath {
    VaultPath::new(s).expect("test setup")
}

fn params(p: ProfileId) -> ProfileParams {
    ProfileParams::for_profile(p)
}

fn link(kind: LinkKind, raw: &str, source: &str, profile: ProfileId) -> LinkRef {
    LinkRef::new(kind, raw, vp(source), profile)
}

/// Resolves and returns `(status, target path)`.
fn res(
    idx: &PathIndex,
    kind: LinkKind,
    raw: &str,
    source: &str,
    profile: ProfileId,
) -> (Status, Option<String>) {
    let r = resolve(idx, &link(kind, raw, source, profile), &params(profile));
    (r.status, r.target_path.map(|p| p.as_str().to_owned()))
}

fn ok(status: Status, target: &str) -> (Status, Option<String>) {
    (status, Some(target.to_owned()))
}

fn none(status: Status) -> (Status, Option<String>) {
    (status, None)
}

use LinkKind::{Embed, HtmlHref, HtmlSrc, Image, Inline, Wikilink};
use ProfileId::{CommonMark, Extended, GfmFile, Obsidian};

// ---------------------------------------------------------------------------
// Parsing and decoding: the examples table, row by row.
// ---------------------------------------------------------------------------

#[test]
fn decoding_examples_table() {
    let gh = params(GfmFile);
    let ob = params(Obsidian);
    let dec = |kind, raw: &str, p: &ProfileParams| decode(&link(kind, raw, "src.md", p.profile), p);
    let path_of = |d: &gv_vault::link::DecodedLink| match &d.target {
        DecodedTarget::Path(p) => p.text.clone(),
        other => panic!("not a path: {other:?}"),
    };

    // `C%23%20notes.md` -> `C# notes.md`
    let d = dec(Inline, "C%23%20notes.md", &gh);
    assert_eq!(path_of(&d), "C# notes.md");
    assert_eq!((d.query.clone(), d.fragment.clone()), (None, None));

    // `docs/a%2Fb.md` -> Blocked (decoded `/` inside a segment)
    assert_eq!(
        dec(Inline, "docs/a%2Fb.md", &gh).target,
        DecodedTarget::Blocked(BlockReason::EncodedSeparator)
    );

    // `<my file.md#Some Heading>`: the parser removes the angle brackets.
    let d = dec(Inline, "my file.md#Some Heading", &gh);
    assert_eq!(path_of(&d), "my file.md");
    assert_eq!(d.fragment.unwrap().text, "Some Heading");

    // `img.png?raw=true` -> path `img.png`, query ignored
    let d = dec(Image, "img.png?raw=true", &gh);
    assert_eq!(path_of(&d), "img.png");
    assert_eq!(d.query.as_deref(), Some("raw=true"));
    assert!(!d.wants_source_mode());

    // `notes.md?plain=1#L5` -> Source mode, line 5
    let d = dec(Inline, "notes.md?plain=1#L5", &gh);
    assert_eq!(path_of(&d), "notes.md");
    assert!(d.wants_source_mode());
    assert_eq!(
        d.fragment.unwrap().kind,
        FragmentKind::Lines {
            start: 5,
            end: None
        }
    );

    // `[[My%20Note]]` -> literal
    assert_eq!(path_of(&dec(Wikilink, "My%20Note", &ob)), "My%20Note");

    // `[[Folder/Note#Part 1#Detail|label]]` -> chain, display
    let d = dec(Wikilink, "Folder/Note#Part 1#Detail|label", &ob);
    assert_eq!(path_of(&d), "Folder/Note");
    assert_eq!(
        d.fragment.unwrap().kind,
        FragmentKind::Heading(vec!["Part 1".into(), "Detail".into()])
    );
    assert_eq!(d.display.as_deref(), Some("label"));

    // `![[photo.png|300x200]]` -> size
    let d = dec(Embed, "photo.png|300x200", &ob);
    assert_eq!(path_of(&d), "photo.png");
    assert_eq!(
        d.size,
        Some(ImageSize {
            width: 300,
            height: Some(200)
        })
    );
    assert_eq!(d.display, None);
    // On a note, the slot is display text.
    assert_eq!(dec(Embed, "Note|300", &ob).display.as_deref(), Some("300"));
}

#[test]
fn decoding_rules() {
    let gh = params(GfmFile);
    let dec = |kind, raw: &str| decode(&link(kind, raw, "src.md", GfmFile), &gh).target;
    // Split before decoding: an encoded `#` stays in the path.
    assert!(matches!(dec(Inline, "a%23b.md#x"), DecodedTarget::Path(p) if p.text == "a#b.md"));
    // Encoded backslash and NUL are blocked; so is a raw backslash.
    assert_eq!(
        dec(Inline, "a%5Cb.md"),
        DecodedTarget::Blocked(BlockReason::EncodedSeparator)
    );
    assert_eq!(
        dec(Inline, "a%00b.md"),
        DecodedTarget::Blocked(BlockReason::EncodedSeparator)
    );
    assert_eq!(
        dec(Inline, "Folder\\Note.md"),
        DecodedTarget::Blocked(BlockReason::Backslash)
    );
    // Invalid UTF-8 after decoding.
    assert_eq!(
        dec(Inline, "%FF.md"),
        DecodedTarget::Blocked(BlockReason::InvalidUtf8)
    );
    // HTML attributes follow the Markdown rules.
    assert!(
        matches!(dec(HtmlHref, "My%20Note.md"), DecodedTarget::Path(p) if p.text == "My Note.md")
    );
    // Wikilinks: no decoding, pipe order per profile.
    let gl = ProfileParams {
        pipe_order: gv_vault::profile::PipeOrder::DisplayFirst,
        ..params(Obsidian)
    };
    let d = decode(&link(Wikilink, "Shown|Target", "s.md", Obsidian), &gl);
    assert!(matches!(d.target, DecodedTarget::Path(p) if p.text == "Target"));
    assert_eq!(d.display.as_deref(), Some("Shown"));
}

// ---------------------------------------------------------------------------
// Statuses.
// ---------------------------------------------------------------------------

#[test]
fn every_status() {
    let idx = PathIndex::from_paths([
        "README.md",
        "docs/Guide.md",
        "a/Meeting.md",
        "b/Meeting.md",
        "other.md",
    ])
    .unwrap();
    assert_eq!(
        res(&idx, Inline, "docs/Guide.md", "README.md", GfmFile),
        ok(Status::Exact, "docs/Guide.md")
    );
    assert_eq!(
        res(&idx, Inline, "docs/guide.md", "README.md", GfmFile),
        ok(Status::CaseFold, "docs/Guide.md")
    );
    assert_eq!(
        res(&idx, Inline, "other", "README.md", GfmFile),
        ok(Status::Loose, "other.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "Meeting", "README.md", Obsidian),
        ok(Status::Ambiguous(2), "a/Meeting.md")
    );
    let building = PathIndex::new_building("/vault", IndexConfig::default());
    assert_eq!(
        res(&building, Wikilink, "Meeting", "README.md", Obsidian),
        none(Status::Pending)
    );
    assert_eq!(
        res(&idx, Wikilink, "Nope", "README.md", Obsidian),
        none(Status::Unresolved)
    );
    assert_eq!(
        res(&idx, Inline, "../x.md", "README.md", GfmFile),
        none(Status::OutsideRoot)
    );
    assert_eq!(
        res(&idx, Inline, "https://example.com", "README.md", GfmFile),
        none(Status::External)
    );
    assert_eq!(
        res(&idx, Inline, "javascript:alert(1)", "README.md", GfmFile),
        none(Status::Blocked)
    );
}

// ---------------------------------------------------------------------------
// Steps of the algorithm.
// ---------------------------------------------------------------------------

#[test]
fn step1_schemes_drives_unc_file() {
    let idx = PathIndex::from_paths(["a.md"]).unwrap();
    let r = |raw: &str| resolve(&idx, &link(Inline, raw, "a.md", GfmFile), &params(GfmFile));
    // A scheme needs 2-32 characters; one letter + `:\` is a drive.
    assert_eq!(r("C:\\Windows\\x.md").status, Status::OutsideRoot);
    assert_eq!(r("c:/x.md").status, Status::OutsideRoot);
    assert_eq!(r("ab:x").status, Status::External);
    // UNC is blocked for loading; following needs confirmation.
    let unc = r("\\\\host\\share\\x.md");
    assert_eq!(unc.status, Status::Blocked);
    assert_eq!(
        unc.policy,
        Some(UrlPolicy::Confirm(ConfirmReason::NetworkPath))
    );
    // `//host/x` is protocol-relative https (security policy; never file:).
    let pr = r("//host/x.md");
    assert_eq!(pr.status, Status::External);
    assert!(matches!(
        pr.url,
        Some(UrlClass::Web {
            protocol_relative: true,
            ..
        })
    ));
    // file:///abs is a local path; file://host is UNC.
    assert_eq!(r("file:///etc/hosts").status, Status::OutsideRoot);
    assert_eq!(r("file://host/share/x.md").status, Status::Blocked);
    assert_eq!(r("file:////host/share/x.md").status, Status::Blocked);
}

#[test]
fn step1_file_url_inside_root() {
    let idx = PathIndex::from_entries(
        "/home/u/vault",
        IndexConfig::default(),
        IndexState::Complete,
        [(vp("notes/a.md"), EntryKind::File, FileMeta::default())],
    )
    .unwrap();
    let r = resolve(
        &idx,
        &link(Inline, "file:///home/u/vault/notes/a.md", "x.md", GfmFile),
        &params(GfmFile),
    );
    assert_eq!(r.status, Status::Exact);
    assert_eq!(r.target_path, Some(vp("notes/a.md")));
    assert!(r.notes.contains(&Note::AbsolutePathInsideRoot));
    // A link-root path that spells the root on disk.
    let r = resolve(
        &idx,
        &link(Inline, "/home/u/vault/notes/a.md", "x.md", GfmFile),
        &params(GfmFile),
    );
    assert_eq!(r.target_path, Some(vp("notes/a.md")));
    // Out of the root and back in by its name.
    let r = resolve(
        &idx,
        &link(Inline, "../vault/notes/a.md", "x.md", GfmFile),
        &params(GfmFile),
    );
    assert_eq!(
        (r.status, r.target_path),
        (Status::Exact, Some(vp("notes/a.md")))
    );
    assert!(r.notes.contains(&Note::ReenteredRoot));
}

#[test]
fn step2_empty_path_is_current_document() {
    let idx = PathIndex::from_paths(["dir/n.md"]).unwrap();
    for (kind, raw, prof) in [
        (Wikilink, "#Heading", Obsidian),
        (Inline, "#Local%20heading", GfmFile),
        (Inline, "", GfmFile),
    ] {
        let r = resolve(&idx, &link(kind, raw, "dir/n.md", prof), &params(prof));
        assert_eq!(
            (r.status, r.target_path.clone()),
            (Status::Exact, Some(vp("dir/n.md"))),
            "{raw}"
        );
    }
    let r = resolve(
        &idx,
        &link(Inline, "#Local%20heading", "dir/n.md", GfmFile),
        &params(GfmFile),
    );
    assert_eq!(r.fragment.unwrap().text, "Local heading");
}

#[test]
fn step3_base_selection() {
    let idx = PathIndex::from_paths([
        "Note.md",
        "Folder/Note.md",
        "Folder/Sub/Note.md",
        "Other/Note.md",
    ])
    .unwrap();
    let src = "Folder/Source.md";
    // Dot paths: relative only, in every profile (also Obsidian).
    assert_eq!(
        res(&idx, Wikilink, "./Note", src, Obsidian),
        ok(Status::Exact, "Folder/Note.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "../Note", src, Obsidian),
        ok(Status::Exact, "Note.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "./Sub/Note", src, Obsidian),
        ok(Status::Exact, "Folder/Sub/Note.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "./Missing", src, Obsidian),
        none(Status::Unresolved)
    );
    // Leading slash: link root.
    assert_eq!(
        res(&idx, Wikilink, "/Note", src, Obsidian),
        ok(Status::Exact, "Note.md")
    );
    assert_eq!(
        res(&idx, Inline, "/Folder/Note.md", src, GfmFile),
        ok(Status::Exact, "Folder/Note.md")
    );
    // Obsidian bare paths: vault root first, also for Markdown links.
    assert_eq!(
        res(&idx, Wikilink, "Note", src, Obsidian),
        ok(Status::Exact, "Note.md")
    );
    assert_eq!(
        res(&idx, Inline, "Note.md", src, Obsidian),
        ok(Status::Exact, "Note.md")
    );
    // ... then relative, then basename.
    assert_eq!(
        res(&idx, Wikilink, "Sub/Note", src, Obsidian),
        ok(Status::Exact, "Folder/Sub/Note.md")
    );
    // GFM: relative only.
    assert_eq!(
        res(&idx, Inline, "Note.md", src, GfmFile),
        ok(Status::Exact, "Folder/Note.md")
    );
    assert_eq!(
        res(&idx, Inline, "Other/Note.md", src, GfmFile),
        none(Status::Unresolved)
    );
    // Extended: relative, then (wikilinks) vault root and basename.
    assert_eq!(
        res(&idx, Wikilink, "Note", src, Extended),
        ok(Status::Exact, "Folder/Note.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "Other/Note", src, Extended),
        ok(Status::Exact, "Other/Note.md")
    );
    assert_eq!(
        res(&idx, Inline, "Other/Note.md", src, Extended),
        none(Status::Unresolved)
    );
}

#[test]
fn step4_lexical_normalization() {
    let idx = PathIndex::from_paths(["a/b.md", "c.md"]).unwrap();
    assert_eq!(
        res(&idx, Inline, "./x/../b.md", "a/s.md", GfmFile),
        ok(Status::Exact, "a/b.md")
    );
    assert_eq!(
        res(&idx, Inline, "../c.md", "a/s.md", GfmFile),
        ok(Status::Exact, "c.md")
    );
    let r = resolve(
        &idx,
        &link(Inline, "../../outside.md", "a/s.md", GfmFile),
        &params(GfmFile),
    );
    assert_eq!(r.status, Status::OutsideRoot);
    assert_eq!(r.outside_path.as_deref(), Some("../outside.md"));
    // Symlinks are never canonicalized: a symlinked folder inside the root
    // is followed through the index, lexically.
    let idx = PathIndex::from_entries(
        "/vault",
        IndexConfig::default(),
        IndexState::Complete,
        [
            (vp("real/n.md"), EntryKind::File, FileMeta::default()),
            (
                vp("alias"),
                EntryKind::Symlink(SymlinkTarget::Inside(vp("real"))),
                FileMeta::default(),
            ),
            (
                vp("out"),
                EntryKind::Symlink(SymlinkTarget::Outside("/etc".into())),
                FileMeta::default(),
            ),
        ],
    )
    .unwrap();
    let r = resolve(
        &idx,
        &link(Inline, "alias/n.md", "x.md", GfmFile),
        &params(GfmFile),
    );
    assert_eq!(
        (r.status, r.target_path.clone()),
        (Status::Exact, Some(vp("real/n.md")))
    );
    assert!(r.notes.contains(&Note::ViaSymlink));
    assert_eq!(
        res(&idx, Inline, "out", "x.md", GfmFile),
        none(Status::OutsideRoot)
    );
}

#[test]
fn step5_candidates_appended_never_substituted() {
    let idx =
        PathIndex::from_paths(["v1.2 notes.md", "Note.1.md", "A.md", "A.png", "x.txt"]).unwrap();
    assert_eq!(
        res(&idx, Wikilink, "v1.2 notes", "s.md", Obsidian),
        ok(Status::Exact, "v1.2 notes.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "Note.1", "s.md", Obsidian),
        ok(Status::Exact, "Note.1.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "A", "s.md", Obsidian),
        ok(Status::Exact, "A.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "A.png", "s.md", Obsidian),
        ok(Status::Exact, "A.png")
    );
    // Never substituted: `x.md` does not find `x.txt`.
    assert_eq!(
        res(&idx, Inline, "x.md", "s.md", GfmFile),
        none(Status::Unresolved)
    );
}

#[test]
fn step5_directory_index_files() {
    let idx = PathIndex::from_paths([
        "docs/README.md",
        "docs/index.md",
        "guide/readme.rst",
        "plain/x.txt",
        "site/index.md",
    ])
    .unwrap();
    // GitHub: README.md, then README.*.
    assert_eq!(
        res(&idx, Inline, "docs", "s.md", GfmFile),
        ok(Status::Exact, "docs/README.md")
    );
    assert_eq!(
        res(&idx, Inline, "docs/", "s.md", GfmFile),
        ok(Status::Exact, "docs/README.md")
    );
    assert_eq!(
        res(&idx, Inline, "guide", "s.md", GfmFile),
        ok(Status::Exact, "guide/readme.rst")
    );
    // No index file: the folder itself.
    let r = resolve(
        &idx,
        &link(Inline, "plain", "s.md", GfmFile),
        &params(GfmFile),
    );
    assert_eq!(
        (r.status, r.target_kind),
        (Status::Exact, Some(TargetKind::Directory))
    );
    // Extended: README.md, then index.md.
    assert_eq!(
        res(&idx, Inline, "site", "s.md", Extended),
        ok(Status::Exact, "site/index.md")
    );
    // CommonMark: no index files.
    let r = resolve(
        &idx,
        &link(Inline, "docs", "s.md", CommonMark),
        &params(CommonMark),
    );
    assert_eq!(
        (r.target_kind, r.target_path),
        (Some(TargetKind::Directory), Some(vp("docs")))
    );
}

#[test]
fn step5_obsidian_folders_after_files() {
    // ORACLE-PENDING: Obsidian resolves links to files only; a folder is
    // revealed only when no file matches at all.
    let idx = PathIndex::from_paths(["Topics/Topics.md", "assets/photo.png"]).unwrap();
    assert_eq!(
        res(&idx, Wikilink, "Topics", "x.md", Obsidian),
        ok(Status::Exact, "Topics/Topics.md")
    );
    let r = resolve(
        &idx,
        &link(Wikilink, "assets", "x.md", Obsidian),
        &params(Obsidian),
    );
    assert_eq!(
        (r.status, r.target_kind),
        (Status::Exact, Some(TargetKind::Directory))
    );
    assert!(r.notes.contains(&Note::FolderRevealed));
}

#[test]
fn step6_match_keys_per_profile() {
    let nfd = "Re\u{301}sume\u{301}.md";
    let idx = PathIndex::from_paths(["README.md", nfd]).unwrap();
    // Exact bytes, NFC, case fold.
    assert_eq!(
        res(&idx, Inline, "README.md", "s.md", GfmFile),
        ok(Status::Exact, "README.md")
    );
    assert_eq!(
        res(&idx, Inline, "R%C3%A9sum%C3%A9.md", "s.md", GfmFile),
        ok(Status::CaseFold, nfd)
    );
    assert_eq!(
        res(&idx, Inline, "Readme.md", "s.md", GfmFile),
        ok(Status::CaseFold, "README.md")
    );
    // Obsidian: case-insensitive by design -> Exact, with a note.
    let r = resolve(
        &idx,
        &link(Wikilink, "readme", "s.md", Obsidian),
        &params(Obsidian),
    );
    assert_eq!(
        (r.status, r.target_path.clone()),
        (Status::Exact, Some(vp("README.md")))
    );
    assert!(r.notes.contains(&Note::CaseDiffers));
    let r = resolve(
        &idx,
        &link(Wikilink, "R\u{e9}sum\u{e9}", "s.md", Obsidian),
        &params(Obsidian),
    );
    assert_eq!(r.status, Status::Exact);
    assert!(r.notes.contains(&Note::NormalizationDiffers));
    // Exact bytes win over a fold twin.
    let twins = PathIndex::from_paths(["Case.md", "case.md"]).unwrap();
    assert_eq!(
        res(&twins, Wikilink, "case", "s.md", Obsidian),
        ok(Status::Exact, "case.md")
    );
    assert_eq!(
        res(&twins, Wikilink, "Case", "s.md", Obsidian),
        ok(Status::Exact, "Case.md")
    );
    // ORACLE-PENDING: fold-only match on twins is ambiguous.
    assert_eq!(
        res(&twins, Wikilink, "CASE", "s.md", Obsidian),
        ok(Status::Ambiguous(2), "Case.md")
    );
}

#[test]
fn step7_suffix_lookup_at_segment_boundaries() {
    let idx = PathIndex::from_paths(["a/b/c.md", "ab/c.md"]).unwrap();
    assert_eq!(
        res(&idx, Wikilink, "b/c", "x.md", Obsidian),
        ok(Status::Exact, "a/b/c.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "B/C", "x.md", Obsidian),
        ok(Status::Exact, "a/b/c.md")
    );
    let idx = PathIndex::from_paths(["Archive/2025/Meeting.md"]).unwrap();
    assert_eq!(
        res(&idx, Wikilink, "2025/Meeting", "x.md", Obsidian),
        ok(Status::Exact, "Archive/2025/Meeting.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "ive/2025/Meeting", "x.md", Obsidian),
        none(Status::Unresolved)
    );
}

#[test]
fn step7_sync_conflicts_excluded() {
    let idx = PathIndex::from_paths([
        "a/Note.md",
        "a/Note (conflicted copy 2026-01-02).md",
        "b/Note.sync-conflict-20260102-030405-ABC1234.md",
    ])
    .unwrap();
    assert_eq!(
        res(&idx, Wikilink, "Note", "x.md", Obsidian),
        ok(Status::Exact, "a/Note.md")
    );
    // Conflict copies are excluded from basename resolution only; a direct
    // path still reaches them.
    let r = resolve(
        &idx,
        &link(
            Wikilink,
            "Note (conflicted copy 2026-01-02)",
            "x.md",
            Obsidian,
        ),
        &params(Obsidian),
    );
    assert_eq!(r.status, Status::Unresolved);
    assert!(r.notes.contains(&Note::ConflictCopiesExcluded(1)));
    assert_eq!(
        res(
            &idx,
            Wikilink,
            "a/Note (conflicted copy 2026-01-02)",
            "x.md",
            Obsidian
        ),
        ok(Status::Exact, "a/Note (conflicted copy 2026-01-02).md")
    );
}

#[test]
fn step7_user_ignore_filters_rank_last() {
    let cfg = IndexConfig::obsidian(vec!["Work/".into()]);
    let idx = PathIndex::from_entries(
        "/v",
        cfg,
        IndexState::Complete,
        ["Work/Meeting.md", "Archive/2025/Meeting.md"]
            .map(|p| (vp(p), EntryKind::File, FileMeta::default())),
    )
    .unwrap();
    // Without the filter Work/ (fewer segments) would win.
    assert_eq!(
        res(&idx, Wikilink, "Meeting", "x.md", Obsidian),
        ok(Status::Ambiguous(2), "Archive/2025/Meeting.md")
    );
}

#[test]
fn step8_tie_break_and_trace_example() {
    // ORACLE-PENDING tie-break; a full trace.
    let idx = PathIndex::from_paths([
        "Archive/2025/Meeting.md",
        "Work/Meeting.md",
        "Projects/Q4/notes.md",
    ])
    .unwrap();
    let r = resolve(
        &idx,
        &link(Wikilink, "Meeting", "Projects/Q4/notes.md", Obsidian),
        &params(Obsidian),
    );
    assert_eq!(r.status, Status::Ambiguous(2));
    assert_eq!(r.target_path, Some(vp("Work/Meeting.md")));
    let cands: Vec<&str> = r.candidates.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(cands, ["Work/Meeting.md", "Archive/2025/Meeting.md"]);
    let text = r.trace_text();
    assert!(
        text.contains("vault-root  Meeting | Meeting.md  not found"),
        "{text}"
    );
    assert!(
        text.contains("relative    Projects/Q4/Meeting | Projects/Q4/Meeting.md  not found"),
        "{text}"
    );
    assert!(
        text.contains("basename    \"meeting\"  2 candidates:"),
        "{text}"
    );
    assert!(
        text.contains("Work/Meeting.md  (2 segments)  <- chosen (fewer segments)"),
        "{text}"
    );
    assert!(
        text.contains("status      Ambiguous(2) -> Work/Meeting.md"),
        "{text}"
    );

    // Exact case first, then fewer segments, shorter path, lexical.
    let idx = PathIndex::from_paths(["x/Draft.md", "y/draft.md"]).unwrap();
    assert_eq!(
        res(&idx, Wikilink, "draft", "s.md", Obsidian),
        ok(Status::Ambiguous(2), "y/draft.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "Draft", "s.md", Obsidian),
        ok(Status::Ambiguous(2), "x/Draft.md")
    );
    let idx = PathIndex::from_paths(["long-folder-name/Spec.md", "x/Spec.md"]).unwrap();
    assert_eq!(
        res(&idx, Wikilink, "Spec", "s.md", Obsidian),
        ok(Status::Ambiguous(2), "x/Spec.md")
    );
    let idx = PathIndex::from_paths(["b/Plan.md", "a/Plan.md", "C/Plan.md"]).unwrap();
    assert_eq!(
        res(&idx, Wikilink, "Plan", "s.md", Obsidian),
        ok(Status::Ambiguous(3), "a/Plan.md")
    );
}

#[test]
fn step9_pending_while_building() {
    let building = PathIndex::new_building("/vault", IndexConfig::default());
    // Needs the basename map: Pending, never Unresolved (no create offer).
    assert_eq!(
        res(&building, Wikilink, "Anything", "s.md", Obsidian),
        none(Status::Pending)
    );
    assert_eq!(
        res(&building, Inline, "x.md", "s.md", GfmFile),
        none(Status::Pending)
    );
    // Decidable without the index.
    assert_eq!(
        res(&building, Inline, "https://x.org", "s.md", GfmFile),
        none(Status::External)
    );
    assert_eq!(
        res(&building, Inline, "../../x.md", "a/s.md", GfmFile),
        none(Status::OutsideRoot)
    );
    assert_eq!(
        res(&building, Inline, "javascript:x", "s.md", GfmFile),
        none(Status::Blocked)
    );
}

#[test]
fn aliases_are_not_targets() {
    // The index knows file names only; an alias never resolves.
    let idx = PathIndex::from_paths(["Artificial Intelligence.md"]).unwrap();
    assert_eq!(
        res(&idx, Wikilink, "AI", "x.md", Obsidian),
        none(Status::Unresolved)
    );
    assert_eq!(
        res(&idx, Wikilink, "artificial intelligence", "x.md", Obsidian),
        ok(Status::Exact, "Artificial Intelligence.md")
    );
}

// ---------------------------------------------------------------------------
// Per-profile parameter table.
// ---------------------------------------------------------------------------

#[test]
fn per_profile_table() {
    let idx =
        PathIndex::from_paths(["docs/x.md", "docs/sub/y.md", "y.md", "Docs2/README.md"]).unwrap();
    // `.md` appended: Loose in CommonMark/GFM, Exact in Obsidian/Extended.
    assert_eq!(
        res(&idx, Inline, "x", "docs/s.md", CommonMark),
        ok(Status::Loose, "docs/x.md")
    );
    assert_eq!(
        res(&idx, Inline, "x", "docs/s.md", GfmFile),
        ok(Status::Loose, "docs/x.md")
    );
    assert_eq!(
        res(&idx, Inline, "x", "docs/s.md", ProfileId::GfmComment),
        ok(Status::Loose, "docs/x.md")
    );
    assert_eq!(
        res(&idx, Inline, "x", "docs/s.md", Obsidian),
        ok(Status::Exact, "docs/x.md")
    );
    assert_eq!(
        res(&idx, Inline, "x", "docs/s.md", Extended),
        ok(Status::Exact, "docs/x.md")
    );
    // Case-only match: CaseFold everywhere but Obsidian.
    for p in [CommonMark, GfmFile, Extended] {
        assert_eq!(
            res(&idx, Inline, "X.md", "docs/s.md", p),
            ok(Status::CaseFold, "docs/x.md"),
            "{p}"
        );
    }
    assert_eq!(
        res(&idx, Inline, "X.md", "docs/s.md", Obsidian),
        ok(Status::Exact, "docs/x.md")
    );
    // Link root for `/x`: configurable (git root, docs_dir).
    let mut docs_root = params(GfmFile);
    docs_root.link_root = vp("docs");
    let r = resolve(
        &idx,
        &link(Inline, "/sub/y.md", "docs/s.md", GfmFile),
        &docs_root,
    );
    assert_eq!(r.target_path, Some(vp("docs/sub/y.md")));
    assert_eq!(
        res(&idx, Inline, "/y.md", "docs/s.md", GfmFile),
        ok(Status::Exact, "y.md")
    );
    // Basename lookup: only Obsidian and Extended wikilinks.
    assert_eq!(
        res(&idx, Wikilink, "y", "Docs2/s.md", Obsidian),
        ok(Status::Exact, "y.md")
    );
    assert_eq!(
        res(&idx, Wikilink, "sub/y", "Docs2/s.md", Extended),
        ok(Status::Exact, "docs/sub/y.md")
    );
    assert_eq!(
        res(&idx, Inline, "sub/y.md", "Docs2/s.md", GfmFile),
        none(Status::Unresolved)
    );
}

#[test]
fn profile_mismatch_is_traced() {
    let idx = PathIndex::from_paths(["a.md"]).unwrap();
    let r = resolve(
        &idx,
        &link(Inline, "a.md", "s.md", Obsidian),
        &params(GfmFile),
    );
    assert!(r.notes.contains(&Note::ProfileMismatch));
}

// ---------------------------------------------------------------------------
// URL schemes and security policy.
// ---------------------------------------------------------------------------

#[test]
fn url_scheme_table() {
    let idx = PathIndex::from_paths(["a.md"]).unwrap();
    let r = |kind, raw: &str| resolve(&idx, &link(kind, raw, "a.md", GfmFile), &params(GfmFile));
    let row = |kind, raw: &str| {
        let x = r(kind, raw);
        (x.status, x.policy)
    };
    use UrlPolicy::{Allow, Block, Confirm};
    assert_eq!(
        row(Inline, "https://example.com/x"),
        (Status::External, Some(Allow))
    );
    assert_eq!(
        row(Inline, "HTTP://example.com/x"),
        (Status::External, Some(Allow))
    );
    assert_eq!(row(Inline, "mailto:a@b.c"), (Status::External, Some(Allow)));
    for s in [
        "tel:123",
        "sms:1",
        "xmpp:a@b",
        "irc://x",
        "ircs://x",
        "ftp://x/y",
        "sftp://x",
        "ssh://x",
        "news:x",
        "geo:1,2",
        "magnet:?xt=1",
    ] {
        assert_eq!(
            row(Inline, s),
            (
                Status::External,
                Some(Confirm(ConfirmReason::ExternalScheme))
            ),
            "{s}"
        );
    }
    assert_eq!(
        row(Inline, "foo:bar"),
        (
            Status::External,
            Some(Confirm(ConfirmReason::UnknownScheme))
        )
    );
    for s in ["smb://h/s", "afp://h/s", "cifs://h/s", "nfs://h/s"] {
        assert_eq!(
            row(Inline, s),
            (Status::Blocked, Some(Confirm(ConfirmReason::NetworkPath))),
            "{s}"
        );
    }
    for s in [
        "javascript:alert(1)",
        "JaVaScRiPt:alert(1)",
        "jav\tascript:alert(1)",
        "\u{1}javascript:x",
        "vbscript:x",
    ] {
        assert_eq!(r(Inline, s).status, Status::Blocked, "{s:?}");
    }
    assert_eq!(
        row(Inline, "data:text/html,<b>x</b>"),
        (Status::Blocked, Some(Block(BlockReason::Data)))
    );
    assert_eq!(
        row(Image, "data:image/png;base64,iVBORw0KGgo="),
        (Status::External, Some(Allow))
    );
    assert_eq!(
        row(Image, "data:image/svg+xml;base64,PHN2Zz4="),
        (Status::Blocked, Some(Block(BlockReason::Data)))
    );
    // Dangerous OS protocol handlers are blocked, not confirmed.
    for s in [
        "ms-msdt:/id PCWDiagnostic",
        "search-ms:query=x",
        "search:x",
        "ms-officecmd:x",
        "ms-cxh:x",
        "ms-cxh-full:x",
        "ms-appinstaller:?x",
        "its:x",
        "ms-its:x",
        "mk:x",
        "hcp://x",
        "shell:x",
    ] {
        assert_eq!(
            row(Inline, s),
            (Status::Blocked, Some(Block(BlockReason::DangerousHandler))),
            "{s}"
        );
    }
}

#[test]
fn network_path_vectors_are_never_loaded() {
    // Regression vectors from the HTML pipeline work: every network-path
    // form, as images (auto-loaded) and as links (confirm to follow).
    let idx = PathIndex::from_paths(["a.md"]).unwrap();
    let vectors = [
        "\\\\evil.example\\share\\x.png",
        "/\\evil.example\\share\\x.png",
        "\\/evil.example/share/x.png",
        "///evil.example/share/x.png",
        "%5C%5Cevil.example%5Cshare%5Cx.png",
        "%2F%2Fevil.example/share/x.png",
        "file://evil.example/share/x.png",
        "file:////evil.example/share/x.png",
        "file:\\\\evil.example\\share\\x.png",
        "file:///%5C%5Cevil.example%5Cshare%5Cx.png",
        "smb://evil.example/share/x.png",
    ];
    for v in vectors {
        for kind in [Image, HtmlSrc] {
            let r = resolve(&idx, &link(kind, v, "a.md", GfmFile), &params(GfmFile));
            assert_eq!(r.status, Status::Blocked, "{kind} {v}");
            assert!(
                matches!(r.policy, Some(UrlPolicy::Block(_))),
                "{kind} {v}: {:?}",
                r.policy
            );
        }
        let r = resolve(&idx, &link(Inline, v, "a.md", GfmFile), &params(GfmFile));
        assert_eq!(r.status, Status::Blocked, "link {v}");
    }
    // Wikilinks are never percent-decoded and never fetched: `%5C` is part
    // of a file name there. Separators that would form a network path are
    // blocked.
    for v in [
        "//evil/share/x.png",
        "\\\\evil\\share\\x.png",
        "/\\evil\\share\\x.png",
    ] {
        assert_eq!(
            res(&idx, Embed, v, "a.md", Obsidian),
            none(Status::Blocked),
            "{v}"
        );
    }
    assert_eq!(
        res(&idx, Embed, "%5C%5Cevil%5Cshare%5Cx.png", "a.md", Obsidian),
        none(Status::Unresolved)
    );
}

#[test]
fn lan_hosts_ask_for_media() {
    let idx = PathIndex::from_paths(["a.md"]).unwrap();
    for (raw, lan) in [
        ("http://192.168.1.1/cgi-bin/reboot", true),
        ("http://localhost:8080/admin", true),
        ("http://127.1/x.png", true),
        ("http://0x7f000001/x.png", true),
        ("http://[::1]/x.png", true),
        ("http://10.0.0.5/x.png", true),
        ("http://printer.local/x.png", true),
        ("https://example.com/x.png", false),
    ] {
        let r = resolve(&idx, &link(Image, raw, "a.md", GfmFile), &params(GfmFile));
        let want = if lan {
            UrlPolicy::Confirm(ConfirmReason::LanHost)
        } else {
            UrlPolicy::Allow
        };
        assert_eq!(r.policy, Some(want), "{raw}");
        // Links to LAN hosts are clickable.
        let r = resolve(&idx, &link(Inline, raw, "a.md", GfmFile), &params(GfmFile));
        assert_eq!(r.policy, Some(UrlPolicy::Allow), "{raw}");
    }
}

#[test]
fn special_targets_and_embeds_outside_root() {
    let idx = PathIndex::from_paths(["a/n.md"]).unwrap();
    let r = resolve(
        &idx,
        &link(Embed, "../../outside", "a/n.md", Obsidian),
        &params(Obsidian),
    );
    assert_eq!(r.status, Status::OutsideRoot);
    assert_eq!(r.block, Some(BlockReason::OutsideRootEmbed));
    let r = resolve(
        &idx,
        &link(Inline, "../../outside.md", "a/n.md", Obsidian),
        &params(Obsidian),
    );
    assert_eq!((r.status, r.block), (Status::OutsideRoot, None));
    let mut allow = params(Obsidian);
    allow.settings.outside_root_embeds = true;
    let r = resolve(
        &idx,
        &link(Embed, "../../outside", "a/n.md", Obsidian),
        &allow,
    );
    assert_eq!(r.block, None);
}

#[test]
fn obsidian_uri_inside_this_vault() {
    let idx = PathIndex::from_paths(["Note.md", "Folder/Deep.md"]).unwrap();
    let mut p = params(Obsidian);
    p.settings = ResolveSettings {
        vault_name: Some("My Vault".into()),
        ..ResolveSettings::default()
    };
    let r = |raw: &str| resolve(&idx, &link(Inline, raw, "x.md", Obsidian), &p);
    let a = r("obsidian://open?vault=My%20Vault&file=Note");
    assert_eq!(
        (a.status, a.target_path),
        (Status::Exact, Some(vp("Note.md")))
    );
    let b = r("obsidian://open?vault=My%20Vault&file=Deep%23Part");
    assert_eq!(b.target_path, Some(vp("Folder/Deep.md")));
    assert_eq!(b.fragment.unwrap().text, "Part");
    let c = r("obsidian://open?vault=Other&file=Note");
    assert_eq!(
        (c.status, c.policy),
        (
            Status::External,
            Some(UrlPolicy::Confirm(ConfirmReason::Obsidian))
        )
    );
    let d = r("obsidian://new?vault=My%20Vault&name=x");
    assert_eq!(d.status, Status::External);
}

// ---------------------------------------------------------------------------
// Repository URL mapping.
// ---------------------------------------------------------------------------

fn repo_params() -> ProfileParams {
    let repo = RepoContext {
        remotes: vec![RepoRemote::parse("git@github.com:o/r.git").expect("test setup")],
        refs: vec!["main".into(), "release/1.0".into()],
        repo_root: VaultPath::root(),
    };
    let mut p = params(GfmFile);
    p.settings.repo = Some(Arc::new(repo));
    p
}

#[test]
fn repository_urls_map_to_local_files() {
    let idx = PathIndex::from_paths(["README.md", "docs/x.md", "img/a.png"]).unwrap();
    let p = repo_params();
    let r = |kind, raw: &str, src: &str| resolve(&idx, &link(kind, raw, src, GfmFile), &p);
    let a = r(
        Inline,
        "https://github.com/o/r/blob/main/docs/x.md",
        "README.md",
    );
    assert_eq!(
        (a.status, a.target_path.clone()),
        (Status::Exact, Some(vp("docs/x.md")))
    );
    assert!(a.notes.contains(&Note::MappedFromUrl));
    // Refs containing `/`: the longest known ref.
    let b = r(
        Inline,
        "https://github.com/o/r/blob/release/1.0/docs/x.md",
        "README.md",
    );
    assert_eq!(b.target_path, Some(vp("docs/x.md")));
    // A full SHA maps to the working tree, with a note.
    let c = r(
        Inline,
        "https://github.com/o/r/blob/0123456789abcdef0123456789abcdef01234567/docs/x.md",
        "README.md",
    );
    assert!(c.notes.contains(&Note::WorkingTreeForSha));
    // raw.githubusercontent.com and `?raw=true` images.
    let d = r(
        Image,
        "https://raw.githubusercontent.com/o/r/main/img/a.png",
        "README.md",
    );
    assert_eq!(d.target_path, Some(vp("img/a.png")));
    // `../../blob/main/x` leaves the repo and maps back in.
    let e = r(Inline, "../../blob/main/docs/x.md", "README.md");
    assert_eq!(
        (e.status, e.target_path),
        (Status::Exact, Some(vp("docs/x.md")))
    );
    let f = r(Inline, "../../../blob/main/docs/x.md", "docs/y.md");
    assert_eq!(f.target_path, Some(vp("docs/x.md")));
    // Other repositories stay external; mapping can be turned off.
    assert_eq!(
        r(
            Inline,
            "https://github.com/o/other/blob/main/x.md",
            "README.md"
        )
        .status,
        Status::External
    );
    let mut off = repo_params();
    off.settings.map_repo_urls = false;
    let g = resolve(
        &idx,
        &link(
            Inline,
            "https://github.com/o/r/blob/main/docs/x.md",
            "README.md",
            GfmFile,
        ),
        &off,
    );
    assert_eq!(g.status, Status::External);
}

// ---------------------------------------------------------------------------
// Link checker examples.
// ---------------------------------------------------------------------------

#[test]
fn checker_examples() {
    let idx = PathIndex::from_paths([
        "README.md",
        "a/Meeting.md",
        "b/Meeting.md",
        "other.md",
        "x/n.md",
    ])
    .unwrap();
    assert_eq!(
        res(&idx, Wikilink, "Missing note", "x/n.md", Obsidian),
        none(Status::Unresolved)
    );
    assert_eq!(
        res(&idx, Embed, "//server/share/x.png", "x/n.md", Obsidian),
        none(Status::Blocked)
    );
    assert_eq!(
        res(&idx, Wikilink, "Meeting", "x/n.md", Obsidian).0,
        Status::Ambiguous(2)
    );
    assert_eq!(
        res(&idx, Inline, "Readme.md", "x.md", GfmFile),
        ok(Status::CaseFold, "README.md")
    );
    assert_eq!(
        res(&idx, Inline, "other", "x.md", GfmFile),
        ok(Status::Loose, "other.md")
    );
    assert_eq!(
        res(&idx, Inline, "../../outside.md", "x/n.md", GfmFile),
        none(Status::OutsideRoot)
    );
}

#[test]
fn incremental_updates_change_resolution() {
    let idx = PathIndex::from_paths(["a/Meeting.md"]).unwrap();
    assert_eq!(
        res(&idx, Wikilink, "Meeting", "x.md", Obsidian),
        ok(Status::Exact, "a/Meeting.md")
    );
    // A new duplicate makes it ambiguous.
    let two = idx
        .apply(&[Change::Upsert {
            path: vp("b/Meeting.md"),
            kind: EntryKind::File,
            meta: FileMeta::default(),
        }])
        .unwrap();
    assert_eq!(
        res(&two, Wikilink, "Meeting", "x.md", Obsidian).0,
        Status::Ambiguous(2)
    );
    // A rename keeps the FileId.
    let id = idx.id_of("a/Meeting.md").unwrap();
    let moved = idx
        .apply(&[Change::Rename {
            from: vp("a/Meeting.md"),
            to: vp("z/Meeting.md"),
        }])
        .unwrap();
    let r = resolve(
        &moved,
        &link(Wikilink, "Meeting", "x.md", Obsidian),
        &params(Obsidian),
    );
    assert_eq!(
        (r.target, r.target_path),
        (Some(id), Some(vp("z/Meeting.md")))
    );
}

/// Wikilinks as the parser reads them (through the content index's
/// extraction) resolve with the Obsidian order: vault root, then the
/// source's folder, then the basename; files before folders; the
/// tie-break for several basename matches; fragments and sizes decoded.
#[test]
fn parsed_wikilinks_resolve_in_obsidian_order() {
    let idx = PathIndex::from_paths([
        "Note.md",
        "Sub/Note.md",
        "Sub/Only.md",
        "Deep/x/Twin.md",
        "Twin.md",
        "A/Twin2.md",
        "B/Twin2.md",
        "img.png",
        "Folder/x.md",
    ])
    .unwrap();
    let text = "[[Note]] [[Only#Heading]] [[ Twin ]] [[Twin2]] ![[img.png|100x20]] [[Folder]] \
                [[Missing]] %%[[Hidden]]%% `[[Code]]`\n\n| [[Sub/Note\\|alias]] |\n| -- |\n";
    let data = gv_vault::content::extract(text, Obsidian);
    let got: Vec<(String, Status, Option<String>)> = data
        .links
        .iter()
        .map(|l| {
            let lref = LinkRef::new(l.kind, &*l.raw, vp("Sub/Source.md"), Obsidian);
            let r = resolve(&idx, &lref, &params(Obsidian));
            (
                l.raw.to_string(),
                r.status,
                r.target_path.map(|p| p.as_str().to_owned()),
            )
        })
        .collect();
    let want = [
        // The vault root first, even beside a note of the same name.
        ("Note", Status::Exact, Some("Note.md")),
        // Then the basename.
        ("Only#Heading", Status::Exact, Some("Sub/Only.md")),
        // Spaces around the target are trimmed (ORACLE-PENDING); the root
        // path wins over the deeper basename match.
        (" Twin ", Status::Exact, Some("Twin.md")),
        // Two basename matches: the tie-break picks one.
        ("Twin2", Status::Ambiguous(2), Some("A/Twin2.md")),
        ("img.png|100x20", Status::Exact, Some("img.png")),
        // A folder is a target only after every file lookup failed.
        ("Folder", Status::Exact, Some("Folder")),
        ("Missing", Status::Unresolved, None),
        // `\|` in a table cell is the pipe.
        ("Sub/Note|alias", Status::Exact, Some("Sub/Note.md")),
    ];
    let want: Vec<(String, Status, Option<String>)> = want
        .into_iter()
        .map(|(r, s, t)| (r.to_owned(), s, t.map(str::to_owned)))
        .collect();
    assert_eq!(got, want);
    let d = decode(
        &link(Embed, &data.links[4].raw, "Sub/Source.md", Obsidian),
        &params(Obsidian),
    );
    assert_eq!(
        d.size,
        Some(ImageSize {
            width: 100,
            height: Some(20)
        })
    );
    let d = decode(
        &link(Wikilink, &data.links[1].raw, "Sub/Source.md", Obsidian),
        &params(Obsidian),
    );
    assert!(
        matches!(d.fragment.map(|f| f.kind), Some(FragmentKind::Heading(h)) if h == ["Heading"])
    );
}
