// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The HTML flavor of the system clipboard, read through `arboard` for smart
//! paste (GPUI's clipboard item carries no HTML).
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

/// How long a paste waits for the clipboard's HTML. Programs that own the
/// clipboard answer within milliseconds; this only bounds a stuck owner.
pub const READ_TIMEOUT: Duration = Duration::from_millis(400);

/// The system clipboard's HTML, as smart paste reads it.
pub fn system_reader() -> Rc<dyn HtmlClipboard> {
    Rc::new(WorkerReader::new(ArboardSource::default(), READ_TIMEOUT))
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
