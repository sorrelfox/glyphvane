// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use gv_profile::ProfileId;
use gv_vault::VaultPath;
use gv_vault::content::{FileStamp, NoteData, NoteRecord, RecordKind};

use crate::query::{Comparison, Expr, Operator, PropertyTest, TextPattern, parse};
use crate::{
    CancelToken, FileResult, MemorySource, Query, QueryError, SearchOptions, rank, search,
    search_all,
};

fn lit(s: &str) -> Expr {
    Expr::Text(TextPattern::Literal(s.into()))
}

fn op(o: Operator, e: Expr) -> Expr {
    Expr::Op(o, Box::new(e))
}

fn not(e: Expr) -> Expr {
    Expr::Not(Box::new(e))
}

// ---------------------------------------------------------------- parser

#[test]
fn parses_words_as_and() {
    assert_eq!(
        parse("foo bar"),
        Some(Expr::And(vec![lit("foo"), lit("bar")]))
    );
    assert_eq!(parse("  foo  "), Some(lit("foo")));
    assert_eq!(parse(""), None);
    assert_eq!(parse("   "), None);
}

#[test]
fn parses_or_with_and_precedence() {
    assert_eq!(
        parse("a b OR c"),
        Some(Expr::Or(vec![
            Expr::And(vec![lit("a"), lit("b")]),
            lit("c")
        ]))
    );
    // Lower-case `or` and `ORANGE` are words.
    assert_eq!(
        parse("a or b"),
        Some(Expr::And(vec![lit("a"), lit("or"), lit("b")]))
    );
    assert_eq!(parse("ORANGE"), Some(lit("ORANGE")));
    // A dangling OR is ignored.
    assert_eq!(parse("a OR"), Some(lit("a")));
}

#[test]
fn parses_negation_and_groups() {
    assert_eq!(
        parse("-(a OR b) c"),
        Some(Expr::And(vec![
            not(Expr::Or(vec![lit("a"), lit("b")])),
            lit("c")
        ]))
    );
    assert_eq!(parse("--a"), Some(not(not(lit("a")))));
    // A lone dash and a mid-word dash are text.
    assert_eq!(parse("-"), Some(lit("-")));
    assert_eq!(parse("well-known"), Some(lit("well-known")));
}

#[test]
fn parses_phrases_with_escapes() {
    assert_eq!(parse(r#""foo bar""#), Some(lit("foo bar")));
    assert_eq!(parse(r#""say \"hi\"""#), Some(lit(r#"say "hi""#)));
    // Unclosed: to the end.
    assert_eq!(parse(r#""foo bar"#), Some(lit("foo bar")));
    assert_eq!(parse(r#""""#), Some(Expr::All));
}

#[test]
fn parses_regex() {
    assert_eq!(
        parse(r"/fo+\/x/ y"),
        Some(Expr::And(vec![
            Expr::Text(TextPattern::Regex("fo+/x".into())),
            lit("y")
        ]))
    );
    assert_eq!(
        parse("/a b/"),
        Some(Expr::Text(TextPattern::Regex("a b".into())))
    );
    // No closing slash: a word.
    assert_eq!(parse("/usr"), Some(lit("/usr")));
    assert_eq!(parse("//"), Some(lit("//")));
}

#[test]
fn parses_operators() {
    assert_eq!(
        parse("file:readme"),
        Some(op(Operator::File, lit("readme")))
    );
    assert_eq!(parse("PATH:a/b"), Some(op(Operator::Path, lit("a/b"))));
    assert_eq!(
        parse(r#"line:(foo "bar baz")"#),
        Some(op(
            Operator::Line,
            Expr::And(vec![lit("foo"), lit("bar baz")])
        ))
    );
    assert_eq!(parse("task-todo:x"), Some(op(Operator::TaskTodo, lit("x"))));
    assert_eq!(parse("task-done:x"), Some(op(Operator::TaskDone, lit("x"))));
    assert_eq!(parse("task:\"\""), Some(op(Operator::Task, Expr::All)));
    assert_eq!(
        parse("match-case:Foo"),
        Some(op(Operator::MatchCase, lit("Foo")))
    );
    assert_eq!(parse("tag:#a/b"), Some(op(Operator::Tag, lit("#a/b"))));
    // Nested operators.
    assert_eq!(
        parse("ignore-case:file:X"),
        Some(op(Operator::IgnoreCase, op(Operator::File, lit("X"))))
    );
    // An operator still being typed is dropped.
    assert_eq!(parse("tag:"), None);
    assert_eq!(
        parse("foo path: bar"),
        Some(Expr::And(vec![lit("foo"), lit("bar")]))
    );
    // Unknown prefixes are words.
    assert_eq!(parse("http://x"), Some(lit("http://x")));
    for (kw, o) in Operator::ALL {
        assert_eq!(o.keyword(), kw);
    }
}

#[test]
fn parses_properties() {
    let prop = |name: &str, test| Expr::Property {
        name: name.into(),
        test,
    };
    assert_eq!(
        parse("[status]"),
        Some(prop("status", PropertyTest::Exists))
    );
    assert_eq!(
        parse("[status:null]"),
        Some(prop("status", PropertyTest::Null))
    );
    assert_eq!(
        parse("[status:done]"),
        Some(prop("status", PropertyTest::Value(Box::new(lit("done")))))
    );
    assert_eq!(
        parse("[prio:>5]"),
        Some(prop(
            "prio",
            PropertyTest::Compare(Comparison::Gt, "5".into())
        ))
    );
    assert_eq!(
        parse("[due:<=2026-01-01]"),
        Some(prop(
            "due",
            PropertyTest::Compare(Comparison::Le, "2026-01-01".into())
        ))
    );
    assert_eq!(
        parse(r#"["my prop":"a b"]"#),
        Some(prop("my prop", PropertyTest::Value(Box::new(lit("a b")))))
    );
    assert_eq!(
        parse("[s:(a OR b)] x"),
        Some(Expr::And(vec![
            prop(
                "s",
                PropertyTest::Value(Box::new(Expr::Or(vec![lit("a"), lit("b")])))
            ),
            lit("x")
        ]))
    );
    // Unclosed and empty.
    assert_eq!(parse("[status"), Some(prop("status", PropertyTest::Exists)));
    assert_eq!(parse("[]"), None);
}

#[test]
fn parser_is_total_on_odd_input() {
    for q in [
        ")",
        "(((",
        "a)b",
        "-)",
        "\"",
        "[",
        "[:]",
        "/",
        "OR",
        "OR OR",
        "- -",
        "file:(",
        "é/ü/",
        "tag:#",
        "(a OR) b",
        "\\",
        "[a:[b]]",
        "line:(task:x)",
        "🙂 -🙂",
    ] {
        // Must not panic or loop; compiling may fail only on regexes.
        let _ = parse(q);
        let _ = Query::new(q, false);
    }
    assert_eq!(parse("a)b"), Some(Expr::And(vec![lit("a"), lit("b")])));
}

// ---------------------------------------------------------------- matching

fn vault() -> MemorySource {
    let mut src = MemorySource::new();
    let p = ProfileId::Obsidian;
    src.add(
        "Projects/Alpha.md",
        "---\nstatus: done\nprio: 7\ntags: [work]\naliases: [First]\n---\n# Alpha plan\n\nThe quick brown fox.\nJumps over the lazy dog.\n\n## Tasks\n\n- [ ] write the report #todo\n- [x] book the room\n- [/] review draft\n",
        p,
    );
    src.add(
        "Projects/Beta.md",
        "---\nstatus: draft\nprio: 3\ndue: 2026-03-01\n---\n# Beta\n\nA fox appears.\n\nThe dog sleeps. #animals/dog\n\n```\n#notatag fox in code\n```\n",
        p,
    );
    src.add(
        "Daily/2026-11-17.md",
        "Met with the FOX team.\nNothing about canines.\n\n- [x] fox call\n",
        p,
    );
    src.add(
        "notes/Straße.md",
        "Grüße aus der STRASSE. Über alles.\n#Animals\n",
        p,
    );
    src.add("empty.md", "", p);
    src
}

fn run(q: &str) -> Vec<FileResult> {
    run_case(q, false)
}

fn run_case(q: &str, match_case: bool) -> Vec<FileResult> {
    let query = Query::new(q, match_case).unwrap();
    let (results, _) = search_all(
        &query,
        &vault(),
        &SearchOptions::default(),
        &CancelToken::new(),
    );
    results
}

fn paths(q: &str) -> Vec<String> {
    let mut v: Vec<String> = run(q).iter().map(|r| r.path.to_string()).collect();
    v.sort();
    v
}

#[test]
fn words_match_text_or_file_name_ignoring_case() {
    assert_eq!(
        paths("fox"),
        [
            "Daily/2026-11-17.md",
            "Projects/Alpha.md",
            "Projects/Beta.md"
        ]
    );
    assert_eq!(paths("beta"), ["Projects/Beta.md"]);
    assert_eq!(paths("fox dog"), ["Projects/Alpha.md", "Projects/Beta.md"]);
    assert_eq!(paths("fox -dog"), ["Daily/2026-11-17.md"]);
    assert_eq!(
        paths("canines OR room"),
        ["Daily/2026-11-17.md", "Projects/Alpha.md"]
    );
    assert!(paths("nonexistentword").is_empty());
    // Empty query: nothing.
    assert!(paths("").is_empty());
}

#[test]
fn match_case_toggle_and_operators() {
    let p = |q: &str, c| {
        let mut v: Vec<String> = run_case(q, c).iter().map(|r| r.path.to_string()).collect();
        v.sort();
        v
    };
    assert_eq!(p("FOX", true), ["Daily/2026-11-17.md"]);
    assert_eq!(p("match-case:FOX", false), ["Daily/2026-11-17.md"]);
    assert_eq!(p("ignore-case:FOX", true).len(), 3);
    // Non-ASCII case folding.
    assert_eq!(p("grüsse", false), Vec::<String>::new());
    assert_eq!(p("GRÜSSE", false), Vec::<String>::new());
    assert_eq!(p("GRÜẞE", false), ["notes/Straße.md"]);
    assert_eq!(p("über", false), ["notes/Straße.md"]);
    assert_eq!(p("über", true), Vec::<String>::new());
}

#[test]
fn phrases_and_regex() {
    assert_eq!(paths("\"quick brown\""), ["Projects/Alpha.md"]);
    assert!(paths("\"brown quick\"").is_empty());
    assert_eq!(paths("/qu[i]ck\\s+brown/"), ["Projects/Alpha.md"]);
    assert_eq!(paths("/^Met with/"), ["Daily/2026-11-17.md"]);
    assert_eq!(paths("/sleeps\\.$/"), Vec::<String>::new());
    let err = Query::new("/(unclosed/", false).unwrap_err();
    assert!(matches!(err, QueryError::Regex { .. }));
    assert!(err.to_string().contains("(unclosed"));
}

#[test]
fn file_path_and_content_operators() {
    assert_eq!(paths("file:alpha"), ["Projects/Alpha.md"]);
    assert!(paths("file:projects").is_empty());
    assert_eq!(
        paths("path:projects"),
        ["Projects/Alpha.md", "Projects/Beta.md"]
    );
    assert_eq!(paths("path:projects -file:beta"), ["Projects/Alpha.md"]);
    assert_eq!(paths("file:(alpha OR beta)").len(), 2);
    // `content:` ignores the file name.
    assert_eq!(paths("content:beta"), ["Projects/Beta.md"]);
    assert!(paths("content:2026-11-17").is_empty());
    assert_eq!(paths("2026-11-17"), ["Daily/2026-11-17.md"]);
}

#[test]
fn tags_come_from_the_index() {
    assert_eq!(paths("tag:#todo"), ["Projects/Alpha.md"]);
    assert_eq!(paths("tag:todo"), ["Projects/Alpha.md"]);
    // Front matter tags.
    assert_eq!(paths("tag:work"), ["Projects/Alpha.md"]);
    // Nested: a parent finds its children; case is ignored.
    assert_eq!(
        paths("tag:#animals"),
        ["Projects/Beta.md", "notes/Straße.md"]
    );
    assert_eq!(paths("tag:animals/dog"), ["Projects/Beta.md"]);
    assert!(paths("tag:anim").is_empty());
    // Not in code blocks.
    assert!(paths("tag:notatag").is_empty());
    assert_eq!(
        paths("tag:/^anim/"),
        ["Projects/Beta.md", "notes/Straße.md"]
    );
    // The tag is highlighted in its line.
    let r = run("tag:todo");
    let s = &r[0].snippets[0];
    assert!(s.text.contains("#todo"));
    assert_eq!(&s.text[s.highlights[0].clone()], "#todo");
}

#[test]
fn line_block_and_section_scopes() {
    // "quick" and "dog" are in one block but on two lines.
    assert!(paths("line:(quick dog)").is_empty());
    assert_eq!(paths("block:(quick dog)"), ["Projects/Alpha.md"]);
    assert_eq!(paths("line:(quick fox)"), ["Projects/Alpha.md"]);
    // Beta: "fox" and "dog" are in different blocks, same section.
    assert_eq!(paths("block:(fox dog)"), ["Projects/Alpha.md"]);
    assert_eq!(
        paths("section:(fox dog)"),
        ["Projects/Alpha.md", "Projects/Beta.md"]
    );
    // Alpha: "fox" and "report" are in different sections.
    assert!(paths("section:(fox report)").is_empty());
    // A fenced block is one block, blank lines and all.
    assert_eq!(paths("block:(notatag code)"), ["Projects/Beta.md"]);
    // Negation inside a scope.
    assert_eq!(
        paths("line:(fox -quick)"),
        ["Daily/2026-11-17.md", "Projects/Beta.md"]
    );
    // Only matching lines are highlighted.
    let r = run("line:(fox team)");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].snippets.len(), 1);
    assert_eq!(r[0].snippets[0].line, 0);
}

#[test]
fn tasks() {
    assert_eq!(paths("task:report"), ["Projects/Alpha.md"]);
    assert!(paths("task-done:report").is_empty());
    assert_eq!(paths("task-todo:report"), ["Projects/Alpha.md"]);
    // Any status other than a space is done.
    assert_eq!(paths("task-done:review"), ["Projects/Alpha.md"]);
    assert_eq!(paths("task-done:fox"), ["Daily/2026-11-17.md"]);
    // "fox" outside tasks does not count.
    assert!(paths("task:quick").is_empty());
    assert_eq!(
        paths("task:\"\""),
        ["Daily/2026-11-17.md", "Projects/Alpha.md"]
    );
    let r = run("task-done:\"\" path:projects");
    assert_eq!(r.len(), 1);
}

#[test]
fn properties() {
    assert_eq!(paths("[status]"), ["Projects/Alpha.md", "Projects/Beta.md"]);
    assert_eq!(paths("[STATUS:done]"), ["Projects/Alpha.md"]);
    assert_eq!(paths("[status:(done OR draft)]").len(), 2);
    assert_eq!(paths("[status:-done]"), ["Projects/Beta.md"]);
    assert_eq!(paths("[prio:>5]"), ["Projects/Alpha.md"]);
    assert_eq!(paths("[prio:<=3]"), ["Projects/Beta.md"]);
    assert_eq!(paths("[prio:=7]"), ["Projects/Alpha.md"]);
    assert_eq!(paths("[due:<2026-06-01]"), ["Projects/Beta.md"]);
    assert_eq!(paths("[aliases:first]"), ["Projects/Alpha.md"]);
    assert!(paths("[missing]").is_empty());
    assert_eq!(paths("-[status]").len(), 3);
    // A property query reads no text: no snippets.
    assert!(run("[status:done]")[0].snippets.is_empty());
}

#[test]
fn null_property() {
    let mut src = MemorySource::new();
    src.add("a.md", "---\nx:\n---\nbody\n", ProfileId::Obsidian);
    src.add("b.md", "---\nx: 1\n---\nbody\n", ProfileId::Obsidian);
    let q = Query::new("[x:null]", false).unwrap();
    let (r, _) = search_all(&q, &src, &SearchOptions::default(), &CancelToken::new());
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].path.as_str(), "a.md");
}

#[test]
fn snippets_and_highlights() {
    let r = run("fox");
    let alpha = r
        .iter()
        .find(|r| r.path.as_str() == "Projects/Alpha.md")
        .unwrap();
    assert_eq!(alpha.match_count, 1);
    let s = &alpha.snippets[0];
    assert_eq!(s.text, "The quick brown fox.");
    assert_eq!(s.highlights, vec![16..19]);
    assert!(!s.clipped_start && !s.clipped_end);
    // Path highlights for file-name matches.
    let r = run("beta");
    assert_eq!(r[0].path_hits, vec![9..13]);
    assert!(r[0].name_match);
    // Overlapping terms merge.
    let r = run("quick \"quick brown\"");
    assert_eq!(r[0].snippets[0].highlights, vec![4..15]);
}

#[test]
fn long_lines_are_clipped_on_char_boundaries() {
    let mut src = MemorySource::new();
    let line = format!("{}needle{}", "é".repeat(300), "ü".repeat(300));
    src.add("long.md", &line, ProfileId::Obsidian);
    let q = Query::new("needle", false).unwrap();
    let opts = SearchOptions {
        context_bytes: 21,
        ..SearchOptions::default()
    };
    let (r, _) = search_all(&q, &src, &opts, &CancelToken::new());
    let s = &r[0].snippets[0];
    assert!(s.clipped_start && s.clipped_end);
    assert_eq!(&s.text[s.highlights[0].clone()], "needle");
    assert!(s.text.len() <= 3 * 21 + 4);
}

#[test]
fn limits() {
    let mut src = MemorySource::new();
    for i in 0..200 {
        src.add(
            &format!("n{i}.md"),
            &"word ".repeat(50),
            ProfileId::Obsidian,
        );
    }
    let q = Query::new("word", false).unwrap();
    let opts = SearchOptions {
        max_files: 10,
        max_matches_per_file: 5,
        max_snippets_per_file: 1,
        ..SearchOptions::default()
    };
    let (r, summary) = search_all(&q, &src, &opts, &CancelToken::new());
    assert_eq!(r.len(), 10);
    assert!(summary.limit_reached);
    assert_eq!(summary.files_matched, 10);
    for f in &r {
        assert_eq!(f.match_count, 50);
        assert_eq!(f.snippets.len(), 1);
        assert!(f.snippets[0].highlights.len() <= 5);
    }
}

#[test]
fn streams_batches_and_cancels() {
    let mut src = MemorySource::new();
    for i in 0..500 {
        src.add(&format!("n{i}.md"), "alpha beta\n", ProfileId::Obsidian);
    }
    let q = Query::new("alpha", false).unwrap();
    let opts = SearchOptions {
        batch_size: 16,
        batch_interval: Duration::from_millis(5),
        ..SearchOptions::default()
    };
    let mut batches = 0;
    let mut files = 0;
    let summary = search(&q, &src, &opts, &CancelToken::new(), None, |b| {
        batches += 1;
        files += b.files.len();
        assert_eq!(b.total, 500);
        assert!(b.scanned <= 500);
    });
    assert_eq!(files, 500);
    assert!(batches >= 2);
    assert_eq!(summary.files_matched, 500);
    assert_eq!(summary.scanned, 500);
    assert!(summary.first_batch.is_some());
    assert!(!summary.cancelled);

    // Cancelled from the first batch: nothing more is delivered.
    let cancel = CancelToken::new();
    let mut seen = 0;
    let summary = search(&q, &src, &opts, &cancel, None, |b| {
        seen += b.files.len();
        cancel.cancel();
    });
    assert!(summary.cancelled);
    assert!(seen < 500);

    // Cancelled before it starts.
    let cancel = CancelToken::new();
    cancel.cancel();
    let summary = search(&q, &src, &opts, &cancel, None, |_| panic!("no batch"));
    assert!(summary.cancelled);
    assert_eq!(summary.scanned, 0);
}

#[test]
fn runs_on_a_given_pool() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    let q = Query::new("fox", false).unwrap();
    let mut n = 0;
    let s = search(
        &q,
        &vault(),
        &SearchOptions::default(),
        &CancelToken::new(),
        Some(&pool),
        |b| n += b.files.len(),
    );
    assert_eq!(n, 3);
    assert_eq!(s.files_matched, 3);
}

#[test]
fn ranking() {
    let mut src = MemorySource::new();
    let p = ProfileId::Obsidian;
    src.add(
        "a.md",
        "fox once in a long text ...........................",
        p,
    );
    src.add("b.md", "fox fox fox", p);
    src.add("fox.md", "nothing", p);
    src.add("c.md", "# Fox\n", p);
    src.add(
        "d.md",
        "fox once in a long text ...........................",
        p,
    );
    src.set_mtime("d.md", 100);
    let q = Query::new("fox", false).unwrap();
    let (r, _) = search_all(&q, &src, &SearchOptions::default(), &CancelToken::new());
    let order: Vec<&str> = r.iter().map(|r| r.path.as_str()).collect();
    // Title or file-name matches first (denser first), then density,
    // then recency.
    assert_eq!(order, ["c.md", "fox.md", "b.md", "d.md", "a.md"]);
    let mut again = r.clone();
    again.reverse();
    rank(&mut again);
    assert_eq!(again, r);
}

#[test]
fn path_only_records_match_by_name() {
    let mut src = MemorySource::new();
    src.push(
        NoteRecord {
            path: VaultPath::new("cloud/Remote fox.md").unwrap(),
            stamp: FileStamp::default(),
            kind: RecordKind::Placeholder,
            data: NoteData::default(),
        },
        "fox fox (not readable)",
    );
    assert_eq!(src.len(), 1);
    let q = Query::new("fox", false).unwrap();
    let (r, _) = search_all(&q, &src, &SearchOptions::default(), &CancelToken::new());
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].match_count, 0);
    assert!(r[0].name_match);
    let q = Query::new("content:fox", false).unwrap();
    let (r, _) = search_all(&q, &src, &SearchOptions::default(), &CancelToken::new());
    assert!(r.is_empty());
}

#[test]
fn match_note_single() {
    let src = vault();
    let q = Query::new("lazy", false).unwrap();
    let opts = SearchOptions::default();
    use crate::SearchSource as _;
    let recs = src.records();
    let alpha = recs
        .iter()
        .find(|r| r.path.as_str() == "Projects/Alpha.md")
        .unwrap();
    let load = || src.text(alpha);
    let hit = q.match_note(alpha, &load, &opts).unwrap();
    assert_eq!(hit.snippets[0].line, 9);
    let beta = recs
        .iter()
        .find(|r| r.path.as_str() == "Projects/Beta.md")
        .unwrap();
    let load = || src.text(beta);
    assert!(q.match_note(beta, &load, &opts).is_none());
}
