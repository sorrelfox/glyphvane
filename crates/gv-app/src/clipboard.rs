// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The HTML flavor of the system clipboard, read through `arboard` for smart
//! paste and written for *Copy as rich text* (GPUI's clipboard item carries
//! no HTML).
//!
//! Writing works like reading: on demand, on a worker thread that keeps the
//! platform clipboard open (on X11 and Wayland the program that copied
//! serves the data for as long as it holds the clipboard), and the UI
//! thread waits at most [`WRITE_TIMEOUT`]. A failure (no data-control
//! protocol and no X server, a stuck connection) is reported, and the copy
//! falls back to plain text.
//!
//! - **On demand only.** Nothing is read until a paste asks, and the
//!   clipboard is never polled. The platform clipboard is opened at the
//!   first paste and kept for the next ones.
//! - **Off the UI thread, with a deadline.** A worker thread does the read;
//!   the UI thread waits for it at most [`READ_TIMEOUT`]. On X11 (and
//!   through XWayland) a read is a round trip to the program that owns the
//!   clipboard, which `arboard` waits for up to 4 s, and on Wayland the
//!   owner writes the data through a pipe with no deadline at all. A late
//!   answer is dropped, and while the worker is still stuck on one, later
//!   pastes do not wait for it: they get no HTML.
//! - **Failure is silent.** No HTML, an empty one, a clipboard that cannot
//!   be opened or a timeout all give `None` (logged at debug level), and
//!   the paste falls back to the clipboard's text.
//!
//! Per platform:
//!
//! - **Windows**: the `HTML Format` (CF_HTML) flavor. `arboard` returns its
//!   fragment without the header, so the `SourceURL` is not known.
//! - **macOS**: `public.html`.
//! - **Linux, Wayland**: the `text/html` offer through the data-control
//!   protocol (`ext-data-control-v1` or `wlr-data-control-unstable-v1`),
//!   which KDE Plasma and wlroots compositors provide. GNOME's compositor
//!   does not: `arboard` then uses X11 through XWayland, which sees the
//!   clipboard as the compositor shares it with X11 programs; without an
//!   X server (`DISPLAY` unset) there is no HTML.
//! - **Linux, X11**: the `text/html` target.
//!
//! The page the HTML came from (`text/x-moz-url`, `public.url`) is not
//! available through `arboard`, so relative links stay as they are.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use gv_ui::editor::{ClipboardHtml, HtmlClipboard};
use gv_ui::export::RichClipboard;

/// How long a paste waits for the clipboard's HTML. Programs that own the
/// clipboard answer within milliseconds; this only bounds a stuck owner.
pub const READ_TIMEOUT: Duration = Duration::from_millis(400);

/// The system clipboard's HTML, as smart paste reads it.
pub fn system_reader() -> Rc<dyn HtmlClipboard> {
    Rc::new(WorkerReader::new(ArboardSource::default(), READ_TIMEOUT))
}

/// How long *Copy as rich text* waits for the clipboard to take the HTML.
pub const WRITE_TIMEOUT: Duration = Duration::from_millis(400);

/// The system clipboard, as *Copy as rich text* writes to it.
pub fn system_writer() -> Rc<dyn RichClipboard> {
    Rc::new(WorkerWriter::new(ArboardSink::default(), WRITE_TIMEOUT))
}

/// Something that puts HTML with a plain-text alternative on the clipboard,
/// blocking.
trait HtmlSink {
    fn write_html(&mut self, html: &str, text: &str) -> Result<(), String>;
}

/// The platform clipboard through `arboard`, opened at the first write and
/// kept open, so that it goes on serving what was copied.
#[derive(Default)]
struct ArboardSink {
    clipboard: Option<arboard::Clipboard>,
}

impl HtmlSink for ArboardSink {
    fn write_html(&mut self, html: &str, text: &str) -> Result<(), String> {
        let clipboard = match &mut self.clipboard {
            Some(clipboard) => clipboard,
            slot => slot.insert(arboard::Clipboard::new().map_err(|e| e.to_string())?),
        };
        clipboard.set().html(html, Some(text)).map_err(|e| {
            // Open the clipboard again next time.
            self.clipboard = None;
            e.to_string()
        })
    }
}

/// A write request to the worker: the HTML, its text and where to answer.
type WriteRequest = (String, String, mpsc::SyncSender<Result<(), String>>);

/// Writes to the clipboard on a worker thread, started at the first write,
/// and waits for it at most `timeout`.
struct WorkerWriter<S> {
    sink: RefCell<Option<S>>,
    requests: RefCell<Option<mpsc::Sender<WriteRequest>>>,
    timeout: Duration,
}

impl<S: HtmlSink + Send + 'static> WorkerWriter<S> {
    fn new(sink: S, timeout: Duration) -> Self {
        Self {
            sink: RefCell::new(Some(sink)),
            requests: RefCell::new(None),
            timeout,
        }
    }

    fn requests(&self) -> Result<mpsc::Sender<WriteRequest>, String> {
        let mut requests = self.requests.borrow_mut();
        if requests.is_none() {
            let mut sink = self
                .sink
                .borrow_mut()
                .take()
                .ok_or("the clipboard writer could not start")?;
            let (tx, rx) = mpsc::channel::<WriteRequest>();
            std::thread::Builder::new()
                .name("clipboard-write".into())
                .spawn(move || {
                    for (html, text, reply) in rx {
                        // The copy may have stopped waiting.
                        let _ = reply.send(sink.write_html(&html, &text));
                    }
                })
                .map_err(|e| format!("cannot start the clipboard writer: {e}"))?;
            *requests = Some(tx);
        }
        requests
            .clone()
            .ok_or_else(|| "no clipboard writer".to_owned())
    }
}

impl<S: HtmlSink + Send + 'static> RichClipboard for WorkerWriter<S> {
    fn write(&self, html: &str, text: &str) -> Result<(), String> {
        let requests = self.requests()?;
        let (reply, answer) = mpsc::sync_channel(1);
        requests
            .send((html.to_owned(), text.to_owned(), reply))
            .map_err(|_| "the clipboard writer has stopped".to_owned())?;
        match answer.recv_timeout(self.timeout) {
            Ok(result) => result,
            Err(_) => Err(format!(
                "the clipboard did not take the text within {:?}",
                self.timeout
            )),
        }
    }
}

/// Something that reads the clipboard's HTML, blocking.
trait HtmlSource {
    /// The HTML on the clipboard now, or why there is none.
    fn read_html(&mut self) -> Result<String, String>;
}

/// The platform clipboard through `arboard`, opened at the first read.
#[derive(Default)]
struct ArboardSource {
    clipboard: Option<arboard::Clipboard>,
}

impl HtmlSource for ArboardSource {
    fn read_html(&mut self) -> Result<String, String> {
        let clipboard = match &mut self.clipboard {
            Some(clipboard) => clipboard,
            slot => slot.insert(arboard::Clipboard::new().map_err(|e| e.to_string())?),
        };
        match clipboard.get().html() {
            Ok(html) => Ok(html),
            Err(e @ (arboard::Error::ContentNotAvailable | arboard::Error::ConversionFailure)) => {
                Err(e.to_string())
            }
            Err(e) => {
                // The connection may be gone (a restarted X server or
                // compositor): open the clipboard again next time.
                self.clipboard = None;
                Err(e.to_string())
            }
        }
    }
}

/// A request to the worker: where to send the HTML.
type Reply = mpsc::SyncSender<Option<String>>;

/// Reads the clipboard's HTML on a worker thread, started at the first
/// read, and waits for it at most `timeout`.
struct WorkerReader<S> {
    /// The source, until the worker takes it.
    source: RefCell<Option<S>>,
    requests: RefCell<Option<mpsc::Sender<Reply>>>,
    /// The worker is reading (a timed-out read may still be running).
    busy: Arc<AtomicBool>,
    timeout: Duration,
}

impl<S: HtmlSource + Send + 'static> WorkerReader<S> {
    fn new(source: S, timeout: Duration) -> Self {
        Self {
            source: RefCell::new(Some(source)),
            requests: RefCell::new(None),
            busy: Arc::new(AtomicBool::new(false)),
            timeout,
        }
    }

    /// The worker's request channel, starting the worker if needed.
    fn requests(&self) -> Option<mpsc::Sender<Reply>> {
        let mut requests = self.requests.borrow_mut();
        if requests.is_none() {
            let mut source = self.source.borrow_mut().take()?;
            let busy = Arc::clone(&self.busy);
            let (tx, rx) = mpsc::channel::<Reply>();
            let spawned = std::thread::Builder::new()
                .name("clipboard-html".into())
                .spawn(move || {
                    for reply in rx {
                        let html = match source.read_html() {
                            Ok(html) => Some(html),
                            Err(e) => {
                                tracing::debug!("no HTML on the clipboard: {e}");
                                None
                            }
                        };
                        busy.store(false, Ordering::Release);
                        // The paste may have stopped waiting.
                        let _ = reply.send(html);
                    }
                });
            if let Err(e) = spawned {
                tracing::debug!("cannot start the clipboard reader: {e}");
                return None;
            }
            *requests = Some(tx);
        }
        requests.clone()
    }
}

impl<S: HtmlSource + Send + 'static> HtmlClipboard for WorkerReader<S> {
    fn read_html(&self) -> Option<ClipboardHtml> {
        if self.busy.swap(true, Ordering::AcqRel) {
            tracing::debug!("the clipboard is still being read; pasting without HTML");
            return None;
        }
        let Some(requests) = self.requests() else {
            self.busy.store(false, Ordering::Release);
            return None;
        };
        let (reply, answer) = mpsc::sync_channel(1);
        if requests.send(reply).is_err() {
            tracing::debug!("the clipboard reader has stopped");
            self.busy.store(false, Ordering::Release);
            return None;
        }
        match answer.recv_timeout(self.timeout) {
            Ok(html) => html
                .filter(|html| !html.trim().is_empty())
                .map(|html| ClipboardHtml {
                    html,
                    source_url: None,
                }),
            Err(_) => {
                tracing::debug!(
                    "the clipboard's HTML took longer than {:?}; pasting without it",
                    self.timeout
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// Answers from a script, after an optional delay, counting reads.
    struct Fake {
        answer: Result<String, String>,
        delay: Duration,
        reads: Arc<AtomicUsize>,
    }

    impl Fake {
        fn new(answer: Result<&str, &str>, delay: Duration) -> (Self, Arc<AtomicUsize>) {
            let reads = Arc::new(AtomicUsize::new(0));
            let fake = Self {
                answer: answer.map(str::to_owned).map_err(str::to_owned),
                delay,
                reads: Arc::clone(&reads),
            };
            (fake, reads)
        }
    }

    impl HtmlSource for Fake {
        fn read_html(&mut self) -> Result<String, String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            self.answer.clone()
        }
    }

    const LONG: Duration = Duration::from_secs(5);

    #[test]
    fn reads_html_on_demand_only() {
        let (fake, reads) = Fake::new(Ok("<b>hi</b>"), Duration::ZERO);
        let reader = WorkerReader::new(fake, LONG);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        let html = reader.read_html();
        assert_eq!(
            html,
            Some(ClipboardHtml {
                html: "<b>hi</b>".into(),
                source_url: None
            })
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert!(reader.read_html().is_some());
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn no_html_or_empty_html_is_none() {
        let (fake, _) = Fake::new(Err("content not available"), Duration::ZERO);
        assert_eq!(WorkerReader::new(fake, LONG).read_html(), None);
        let (fake, _) = Fake::new(Ok("  \n"), Duration::ZERO);
        assert_eq!(WorkerReader::new(fake, LONG).read_html(), None);
    }

    /// Records writes, or fails, after an optional delay.
    struct FakeSink {
        written: Arc<std::sync::Mutex<Vec<(String, String)>>>,
        fail: bool,
        delay: Duration,
    }

    impl HtmlSink for FakeSink {
        fn write_html(&mut self, html: &str, text: &str) -> Result<(), String> {
            std::thread::sleep(self.delay);
            if self.fail {
                return Err("no data-control protocol".into());
            }
            self.written
                .lock()
                .expect("lock")
                .push((html.to_owned(), text.to_owned()));
            Ok(())
        }
    }

    #[test]
    fn rich_text_is_written_on_the_worker() {
        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = FakeSink {
            written: Arc::clone(&written),
            fail: false,
            delay: Duration::ZERO,
        };
        let writer = WorkerWriter::new(sink, LONG);
        assert_eq!(writer.write("<b>x</b>", "x"), Ok(()));
        assert_eq!(writer.write("<i>y</i>", "y"), Ok(()));
        assert_eq!(
            *written.lock().expect("lock"),
            [
                ("<b>x</b>".to_owned(), "x".to_owned()),
                ("<i>y</i>".to_owned(), "y".to_owned())
            ]
        );
    }

    #[test]
    fn write_failures_and_timeouts_are_reported() {
        let failing = FakeSink {
            written: Arc::default(),
            fail: true,
            delay: Duration::ZERO,
        };
        assert!(
            WorkerWriter::new(failing, LONG)
                .write("<b>x</b>", "x")
                .is_err()
        );
        let slow = FakeSink {
            written: Arc::default(),
            fail: false,
            delay: Duration::from_millis(300),
        };
        assert!(
            WorkerWriter::new(slow, Duration::from_millis(20))
                .write("<b>x</b>", "x")
                .is_err()
        );
    }

    #[test]
    fn a_slow_owner_times_out_and_does_not_hold_later_pastes() {
        let (fake, reads) = Fake::new(Ok("<p>late</p>"), Duration::from_millis(300));
        let reader = WorkerReader::new(fake, Duration::from_millis(20));
        assert_eq!(reader.read_html(), None);
        // The first read is still running: no second request is queued.
        assert_eq!(reader.read_html(), None);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        // Once the worker is free, reads go through again (and time out
        // again, this owner being slow).
        assert_eq!(reader.read_html(), None);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }
}
