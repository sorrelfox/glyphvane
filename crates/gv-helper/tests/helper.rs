// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The supervised helper process: the real `glyphvane-helper` executable,
//! started by the client; deadlines, cancellation, crashes, the memory
//! limit and the idle stop. Every helper is a child of the test, killed by
//! its own PID only.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gv_helper::protocol::{Fault, Job, Output};
use gv_helper::{Helper, HelperConfig, HelperError, PdfJob};

fn config() -> HelperConfig {
    HelperConfig::new(env!("CARGO_BIN_EXE_glyphvane-helper"))
}

fn never() -> AtomicBool {
    AtomicBool::new(false)
}

const SECOND: Duration = Duration::from_secs(1);

fn pdf_job() -> PdfJob {
    PdfJob {
        source:
            "#set page(width: 100pt, height: 100pt)\n#box(width: 10pt, height: 10pt, fill: red)"
                .into(),
        max_pages: 5,
        ident: "test".into(),
        ..PdfJob::default()
    }
}

fn sleep(ms: u64) -> Job {
    Job::Fault(Fault::Sleep { ms })
}

#[test]
fn jobs_run_in_one_warm_helper() {
    let helper = Helper::new(config());
    assert_eq!(helper.pid(), None, "started on demand");
    let out = helper
        .pdf(pdf_job(), 30 * SECOND, 1 << 20, &never())
        .expect("a PDF");
    assert!(out.pdf.starts_with(b"%PDF-"));
    assert_eq!(out.pages, 1);
    let pid = helper.pid().expect("running");
    helper
        .pdf(pdf_job(), 30 * SECOND, 1 << 20, &never())
        .expect("again");
    assert_eq!(helper.pid(), Some(pid), "the same helper");

    // An engine error keeps the helper.
    let bad = PdfJob {
        source: "#read(\"/etc/passwd\")".into(),
        ..pdf_job()
    };
    let err = helper
        .pdf(bad, 30 * SECOND, 1 << 20, &never())
        .expect_err("denied");
    assert!(matches!(err, HelperError::Failed(_)), "{err}");
    assert_eq!(helper.pid(), Some(pid));

    // Too large an output is an error too.
    let err = helper
        .pdf(pdf_job(), 30 * SECOND, 10, &never())
        .expect_err("too large");
    assert!(matches!(err, HelperError::Failed(_)), "{err}");
    helper.shutdown();
    assert_eq!(helper.pid(), None);
}

#[test]
fn a_late_helper_is_killed_and_replaced() {
    let helper = Helper::new(config());
    helper.run(sleep(0), 30 * SECOND, 1, &never()).expect("ok");
    let first = helper.pid().expect("running");
    let start = Instant::now();
    let err = helper
        .run(sleep(60_000), Duration::from_millis(300), 1, &never())
        .expect_err("late");
    assert!(matches!(err, HelperError::Timeout(_)), "{err}");
    assert!(start.elapsed() < 10 * SECOND);
    assert_eq!(helper.pid(), None, "killed");
    helper.run(sleep(0), 30 * SECOND, 1, &never()).expect("ok");
    assert_ne!(helper.pid(), Some(first), "a new helper");
}

#[test]
fn a_job_can_be_cancelled() {
    let helper = Helper::new(config());
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::Relaxed);
    });
    let start = Instant::now();
    let err = helper
        .run(sleep(60_000), 60 * SECOND, 1, &cancel)
        .expect_err("cancelled");
    canceller.join().expect("thread");
    assert!(matches!(err, HelperError::Cancelled), "{err}");
    assert!(start.elapsed() < 10 * SECOND);
    assert_eq!(helper.pid(), None);
    // Cancelled before it starts: nothing runs.
    assert!(matches!(
        helper.run(sleep(0), SECOND, 1, &cancel),
        Err(HelperError::Cancelled)
    ));
}

#[test]
fn a_crash_is_reported_and_the_next_job_runs() {
    let helper = Helper::new(config());
    let err = helper
        .run(Job::Fault(Fault::Exit), 30 * SECOND, 1, &never())
        .expect_err("crashed");
    assert!(matches!(err, HelperError::Crashed(_)), "{err}");
    assert_eq!(
        helper.run(sleep(0), 30 * SECOND, 1, &never()).expect("ok"),
        Output::Done
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_memory_limit_holds() {
    let mut config = config();
    config.memory_limit_mb = Some(512);
    let helper = Helper::new(config);
    let err = helper
        .run(
            Job::Fault(Fault::Allocate { mb: 2048 }),
            60 * SECOND,
            1,
            &never(),
        )
        .expect_err("over the limit");
    assert!(matches!(err, HelperError::Crashed(_)), "{err}");
    helper.run(sleep(0), 30 * SECOND, 1, &never()).expect("ok");
}

#[test]
fn an_idle_helper_stops() {
    let mut config = config();
    config.idle = Duration::from_millis(200);
    let helper = Helper::new(config);
    helper.run(sleep(0), 30 * SECOND, 1, &never()).expect("ok");
    assert!(helper.pid().is_some());
    let start = Instant::now();
    while helper.pid().is_some() && start.elapsed() < 10 * SECOND {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(helper.pid(), None, "stopped when idle");
}

#[test]
fn a_missing_helper_says_so() {
    let helper = Helper::new(HelperConfig::new("/nonexistent/glyphvane-helper"));
    let err = helper
        .run(sleep(0), SECOND, 1, &never())
        .expect_err("missing");
    assert!(matches!(err, HelperError::NotFound), "{err}");
    assert!(err.to_string().contains("not installed"));
}
