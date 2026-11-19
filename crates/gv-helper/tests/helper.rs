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
use gv_helper::{Helper, HelperConfig, HelperError, PdfJob, TextJob};

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
fn a_cancelled_job_that_ends_soon_keeps_the_helper() {
    let helper = Helper::new(config());
    helper.run(sleep(0), 30 * SECOND, 1, &never()).expect("ok");
    let pid = helper.pid().expect("running");
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        flag.store(true, Ordering::Relaxed);
    });
    // Cancelled, then done within the grace time: not killed.
    let err = helper
        .run(sleep(300), 30 * SECOND, 1, &cancel)
        .expect_err("cancelled");
    canceller.join().expect("thread");
    assert!(matches!(err, HelperError::Cancelled), "{err}");
    assert_eq!(helper.pid(), Some(pid), "the same helper");
    assert_eq!(
        helper.run(sleep(0), 30 * SECOND, 1, &never()).expect("ok"),
        Output::Done
    );
    assert_eq!(helper.pid(), Some(pid));
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

#[test]
fn the_helper_draws_a_page_of_a_pdf() {
    let helper = Helper::new(config());
    let pdf = helper
        .pdf(pdf_job(), 30 * SECOND, 1 << 20, &never())
        .expect("a PDF")
        .pdf;
    let job = gv_helper::PageJob {
        pdf: pdf.clone(),
        page: 0,
        max_width: 50,
        max_height: 80,
    };
    let image = helper.page(job, 30 * SECOND, &never()).expect("drawn");
    // A 100 × 100 pt page in a 50 × 80 box.
    assert_eq!((image.width, image.height), (50, 50));
    assert_eq!(image.pages, 1);
    // The red box (Typst's `red`, #ff4136), on white.
    let red = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 200 && p[1] < 100 && p[2] < 100)
        .count();
    assert!(red > 0, "the box is drawn");
    assert_eq!(&image.rgba[..4], &[255, 255, 255, 255]);

    // Not a PDF: an engine error, and the helper stays.
    let pid = helper.pid();
    let bad = gv_helper::PageJob {
        pdf: b"%PDF-1.7 nothing".to_vec(),
        page: 0,
        max_width: 50,
        max_height: 50,
    };
    let err = helper
        .page(bad, 30 * SECOND, &never())
        .expect_err("not drawn");
    assert!(matches!(err, HelperError::Failed(_)), "{err}");
    assert_eq!(helper.pid(), pid);
}

#[test]
fn the_helper_reads_the_text_of_an_exported_pdf() {
    // Typst embeds a subset of the bundled font with a ToUnicode map, as
    // most PDFs do.
    let helper = Helper::new(config());
    let job = PdfJob {
        source: "#set page(width: 300pt, height: 200pt, margin: 20pt)\n\
                 #set text(font: \"Inter Variable\", size: 12pt)\n\
                 Find the office.\n\nSecond paragraph."
            .into(),
        fonts: vec!["Inter Variable".into()],
        max_pages: 5,
        ident: "text".into(),
        ..PdfJob::default()
    };
    let pdf = helper
        .pdf(job, 30 * SECOND, 1 << 20, &never())
        .expect("a PDF")
        .pdf;
    helper.open(1, pdf, 30 * SECOND, &never()).expect("opens");
    let text = helper
        .text(TextJob { key: 1, page: 0 }, 30 * SECOND, &never())
        .expect("read")
        .expect("held");
    // The "ffi" ligature, if Typst set one, reads as its letters.
    assert_eq!(text.text, "Find the office.\nSecond paragraph.");
    assert_eq!(text.boxes.len(), text.text.chars().count());
    // Within the margins, in reading order.
    let first = text.boxes[0];
    assert!(first.x0 >= 19.0 && first.x0 < 25.0, "{first:?}");
    assert!(first.y0 >= 15.0 && first.y1 < 45.0, "{first:?}");
    let second = text.boxes[17];
    assert!(second.y0 > first.y1 - 1.0, "{second:?}");
    helper.shutdown();
}

#[test]
fn the_text_of_a_page_in_two_columns_is_read_column_by_column() {
    let helper = Helper::new(config());
    let job = PdfJob {
        source: "#set page(width: 400pt, height: 300pt, margin: 20pt, columns: 2,\n\
                   footer: align(center)[7])\n\
                 #set text(font: \"Inter Variable\", size: 9pt)\n\
                 #set par(justify: true)\n\
                 #place(top + center, float: true, scope: \"parent\")[A title across the page]\n\
                 Alpha starts the left column. #lorem(140) Omega ends the right column."
            .into(),
        fonts: vec!["Inter Variable".into()],
        max_pages: 5,
        ident: "columns".into(),
        ..PdfJob::default()
    };
    let pdf = helper
        .pdf(job, 30 * SECOND, 1 << 20, &never())
        .expect("a PDF")
        .pdf;
    helper.open(1, pdf, 30 * SECOND, &never()).expect("opens");
    let text = helper
        .text(TextJob { key: 1, page: 0 }, 30 * SECOND, &never())
        .expect("read")
        .expect("held");
    assert_eq!(text.boxes.len(), text.text.chars().count());
    assert!(
        text.text.starts_with("A title across the page\nAlpha"),
        "{}",
        text.text
    );
    assert!(text.text.ends_with("right column.\n7"), "{}", text.text);
    helper.shutdown();
}
