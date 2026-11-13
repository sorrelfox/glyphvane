// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `cargo xtask spec`: runs the CommonMark or GFM spec examples against
//! gv-syntax's HTML emitters, compares with spec-style normalization, and
//! keeps a ratchet of examples that must keep passing.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use anyhow::{Context, Result, bail};
use gv_syntax::harness;
use serde_json::json;

use crate::fixtures::{self, Example};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Suite {
    CommonMark,
    Gfm,
}

impl Suite {
    fn id(self) -> &'static str {
        match self {
            Suite::CommonMark => "commonmark",
            Suite::Gfm => "gfm",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Suite::CommonMark => "CommonMark 0.31.2",
            Suite::Gfm => "GFM 0.29 (cmark-gfm 0.29.0.gfm.13)",
        }
    }

    fn fixture(self) -> &'static str {
        match self {
            Suite::CommonMark => fixtures::COMMONMARK_JSON,
            Suite::Gfm => fixtures::GFM_JSON,
        }
    }

    /// The ratchet: examples that passed when it was last updated.
    fn passing_file(self) -> &'static str {
        match self {
            Suite::CommonMark => "passing.txt",
            Suite::Gfm => "passing-gfm.txt",
        }
    }

    fn render(self, ex: &Example) -> String {
        match self {
            Suite::CommonMark => harness::render_commonmark_html(&ex.markdown),
            Suite::Gfm => {
                let exts: Vec<&str> = ex.extensions.iter().map(String::as_str).collect();
                harness::render_gfm_html(&ex.markdown, &exts)
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

struct Opts {
    suite: Suite,
    section: Option<String>,
    examples: Vec<u32>,
    format: Format,
    min: Option<usize>,
    ratchet: bool,
    update_ratchet: bool,
    diffs: Option<usize>,
}

const USAGE: &str = "\
usage: cargo xtask spec [--suite commonmark|gfm] [--section NAME] [--example N]...
                        [--format text|json] [--min N] [--ratchet | --update-ratchet]
                        [--diffs N]";

fn parse_args(args: &[String]) -> Result<Opts> {
    let mut o = Opts {
        suite: Suite::CommonMark,
        section: None,
        examples: Vec::new(),
        format: Format::Text,
        min: None,
        ratchet: false,
        update_ratchet: false,
        diffs: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .with_context(|| format!("`{a}` needs a value\n{USAGE}"))
        };
        match a.as_str() {
            "--suite" => {
                o.suite = match value()?.as_str() {
                    "commonmark" => Suite::CommonMark,
                    "gfm" => Suite::Gfm,
                    other => bail!("unknown suite `{other}` (commonmark or gfm)"),
                };
            }
            "--section" => o.section = Some(value()?),
            "--example" => o
                .examples
                .push(value()?.parse().context("--example takes a number")?),
            "--format" => {
                o.format = match value()?.as_str() {
                    "text" => Format::Text,
                    "json" => Format::Json,
                    other => bail!("unknown format `{other}` (text or json)"),
                };
            }
            "--min" => o.min = Some(value()?.parse().context("--min takes a number")?),
            "--diffs" => o.diffs = Some(value()?.parse().context("--diffs takes a number")?),
            "--ratchet" => o.ratchet = true,
            "--update-ratchet" => o.update_ratchet = true,
            "-h" | "--help" => bail!("{USAGE}"),
            other => bail!("unknown option `{other}`\n{USAGE}"),
        }
    }
    if o.update_ratchet && (o.ratchet || o.section.is_some() || !o.examples.is_empty()) {
        bail!(
            "--update-ratchet records the whole suite: it cannot be combined with --ratchet, --section or --example"
        );
    }
    Ok(o)
}

struct Outcome<'a> {
    ex: &'a Example,
    actual: String,
    passed: bool,
}

fn run_example(suite: Suite, ex: &Example) -> (String, bool) {
    match catch_unwind(AssertUnwindSafe(|| suite.render(ex))) {
        Ok(actual) => {
            let passed = harness::normalize_html(&actual) == harness::normalize_html(&ex.html);
            (actual, passed)
        }
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            (format!("<panic: {msg}>"), false)
        }
    }
}

fn read_ratchet(path: &Path) -> Result<BTreeSet<u32>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut set = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let n: u32 = line
            .parse()
            .with_context(|| format!("{}:{}: not an example number", path.display(), i + 1))?;
        set.insert(n);
    }
    Ok(set)
}

fn write_ratchet(path: &Path, suite: Suite, passing: &BTreeSet<u32>) -> Result<()> {
    let mut text = format!(
        "# {} spec examples that pass. `cargo xtask spec --suite {} --ratchet` fails\n\
         # if one of them stops passing. Regenerate with\n\
         # `cargo xtask spec --suite {} --update-ratchet`. One example number per line.\n",
        suite.title(),
        suite.id(),
        suite.id()
    );
    for n in passing {
        let _ = writeln!(text, "{n}");
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// Shows tabs as `→`, like the spec.
fn visible(s: &str) -> String {
    s.replace('\t', "→")
}

/// A minimal line diff (LCS) between `expected` and `actual`.
fn line_diff(expected: &str, actual: &str) -> String {
    let a: Vec<&str> = expected.lines().collect();
    let b: Vec<&str> = actual.lines().collect();
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = String::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            let _ = writeln!(out, "   {}", visible(a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            let _ = writeln!(out, " + {}", visible(b[j]));
            j += 1;
        } else {
            let _ = writeln!(out, " - {}", visible(a[i]));
            i += 1;
        }
    }
    out
}

fn indent(s: &str) -> String {
    if s.is_empty() {
        return "   (empty)\n".to_owned();
    }
    s.lines().fold(String::new(), |mut out, l| {
        let _ = writeln!(out, "   {}", visible(l));
        out
    })
}

/// `cargo xtask spec …`
pub(crate) fn run(root: &Path, args: &[String]) -> Result<bool> {
    let opts = parse_args(args)?;
    run_suite(root, &opts)
}

/// Runs both suites with `--ratchet` and summaries only (for `xtask ci`).
pub(crate) fn ratchet_all(root: &Path) -> Result<bool> {
    let mut ok = true;
    for suite in [Suite::CommonMark, Suite::Gfm] {
        let opts = Opts {
            suite,
            section: None,
            examples: Vec::new(),
            format: Format::Text,
            min: None,
            ratchet: true,
            update_ratchet: false,
            diffs: Some(0),
        };
        ok &= run_suite(root, &opts)?;
    }
    Ok(ok)
}

fn run_suite(root: &Path, opts: &Opts) -> Result<bool> {
    let suite = opts.suite;
    let all = fixtures::load(root, suite.fixture())?;
    let section_filter = opts.section.as_ref().map(|s| s.to_lowercase());
    let selected: Vec<&Example> = all
        .iter()
        .filter(|ex| {
            section_filter
                .as_ref()
                .is_none_or(|f| ex.section.to_lowercase().contains(f.as_str()))
        })
        .filter(|ex| opts.examples.is_empty() || opts.examples.contains(&ex.example))
        .collect();
    for n in &opts.examples {
        if !all.iter().any(|ex| ex.example == *n) {
            bail!("{} has no example {n}", suite.title());
        }
    }
    if selected.is_empty() {
        bail!("no {} examples match the filters", suite.title());
    }

    let outcomes: Vec<Outcome<'_>> = selected
        .iter()
        .map(|ex| {
            let (actual, passed) = run_example(suite, ex);
            Outcome { ex, actual, passed }
        })
        .collect();

    // Sections in spec order, with pass counts.
    let mut sections: Vec<(String, usize, usize)> = Vec::new();
    for o in &outcomes {
        if sections.last().is_none_or(|s| s.0 != o.ex.section) {
            sections.push((o.ex.section.clone(), 0, 0));
        }
        if let Some(s) = sections.last_mut() {
            s.2 += 1;
            s.1 += usize::from(o.passed);
        }
    }
    let passed = outcomes.iter().filter(|o| o.passed).count();
    let total = outcomes.len();
    let passing: BTreeSet<u32> = outcomes
        .iter()
        .filter(|o| o.passed)
        .map(|o| o.ex.example)
        .collect();

    let ratchet_path = root.join(fixtures::DIR).join(suite.passing_file());
    let recorded = read_ratchet(&ratchet_path)?;
    let selected_numbers: BTreeSet<u32> = outcomes.iter().map(|o| o.ex.example).collect();
    let regressions: Vec<u32> = recorded
        .iter()
        .filter(|n| selected_numbers.contains(n) && !passing.contains(n))
        .copied()
        .collect();
    let unrecorded: Vec<u32> = passing.difference(&recorded).copied().collect();
    let failures: Vec<&Outcome<'_>> = outcomes.iter().filter(|o| !o.passed).collect();

    let mut ok = true;
    if opts.ratchet && !regressions.is_empty() {
        ok = false;
    }
    if let Some(min) = opts.min
        && passed < min
    {
        ok = false;
    }

    match opts.format {
        Format::Json => {
            let report = json!({
                "suite": suite.id(),
                "spec": suite.title(),
                "placeholder": harness::IS_PLACEHOLDER,
                "passed": passed,
                "total": total,
                "sections": sections.iter().map(|(name, p, t)| json!({
                    "name": name, "passed": p, "total": t,
                })).collect::<Vec<_>>(),
                "failures": failures.iter().map(|o| json!({
                    "example": o.ex.example,
                    "section": o.ex.section,
                    "markdown": o.ex.markdown,
                    "expected": o.ex.html,
                    "actual": o.actual,
                })).collect::<Vec<_>>(),
                "ratchet": {
                    "file": format!("{}/{}", fixtures::DIR, suite.passing_file()),
                    "recorded": recorded.len(),
                    "regressions": regressions,
                    "unrecorded_passes": unrecorded,
                },
                "min": opts.min,
                "ok": ok,
            });
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Format::Text => {
            let mut out = String::new();
            let _ = writeln!(
                out,
                "{} spec ({}/{})",
                suite.title(),
                fixtures::DIR,
                suite.fixture()
            );
            let width = sections.iter().map(|s| s.0.len()).max().unwrap_or(0).max(7);
            let _ = writeln!(
                out,
                "\n  {:<width$}  {:>5}  {:>5}",
                "Section", "Pass", "Total"
            );
            for (name, p, t) in &sections {
                let mark = if p == t { ' ' } else { '*' };
                let _ = writeln!(out, "{mark} {name:<width$}  {p:>5}  {t:>5}");
            }
            let _ = writeln!(out, "  {:<width$}  {passed:>5}  {total:>5}", "Total");

            let limit = opts.diffs.unwrap_or(if opts.examples.is_empty() {
                10
            } else {
                usize::MAX
            });
            if !failures.is_empty() && limit > 0 {
                let shown = failures.len().min(limit);
                let _ = writeln!(
                    out,
                    "\nFailures: showing {shown} of {} (--diffs N to change)",
                    failures.len()
                );
                for o in failures.iter().take(limit) {
                    let _ = writeln!(
                        out,
                        "\nExample {} ({}, spec lines {}-{})\n  markdown:\n{}  diff (- expected, + actual):\n{}",
                        o.ex.example,
                        o.ex.section,
                        o.ex.start_line,
                        o.ex.end_line,
                        indent(&o.ex.markdown),
                        line_diff(&o.ex.html, &o.actual),
                    );
                }
            }
            print!("{out}");

            if harness::IS_PLACEHOLDER {
                eprintln!(
                    "\nnote: gv-syntax's renderers are still placeholders (gv_syntax::harness::IS_PLACEHOLDER)"
                );
            }
            eprintln!(
                "\nratchet ({}/{}): {} recorded, {} regressed, {} passing but unrecorded",
                fixtures::DIR,
                suite.passing_file(),
                recorded.len(),
                regressions.len(),
                unrecorded.len()
            );
            if !regressions.is_empty() {
                eprintln!("  regressed examples: {regressions:?}");
            }
            if !unrecorded.is_empty() {
                eprintln!(
                    "  record new passes with `cargo xtask spec --suite {} --update-ratchet`",
                    suite.id()
                );
            }
            if let Some(min) = opts.min
                && passed < min
            {
                eprintln!("\n{passed} examples pass, below the required minimum of {min}");
            }
            if opts.ratchet && !regressions.is_empty() {
                eprintln!("\nratchet FAILED: previously passing examples now fail");
            }
        }
    }

    if opts.update_ratchet {
        write_ratchet(&ratchet_path, suite, &passing)?;
        eprintln!(
            "wrote {} ({} examples)",
            ratchet_path.display(),
            passing.len()
        );
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_marks_changed_lines() {
        let d = line_diff("<p>a</p>\n<p>b</p>\n", "<p>a</p>\n<p>c</p>\n");
        assert_eq!(d, "   <p>a</p>\n + <p>c</p>\n - <p>b</p>\n");
    }

    #[test]
    fn ratchet_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("xtask-ratchet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let p = dir.join("passing.txt");
        let set: BTreeSet<u32> = [3, 1, 652].into_iter().collect();
        write_ratchet(&p, Suite::CommonMark, &set).expect("write");
        assert_eq!(read_ratchet(&p).expect("read"), set);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
