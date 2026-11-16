// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `--wait`: files opened for an edit that a caller waits for, such as Git
//! running `$EDITOR`.
//!
//! - The files open in a new window, as tabs in Edit mode (Source), with the
//!   side panels hidden, like any window opened for a file.
//! - A file is closed when its tab lets go of it: the tab is closed, it goes
//!   on to show another file, its window closes, or the application quits.
//!   Closing saves what autosave may save, and the file is reported only
//!   once that save has finished, so the caller reads the final text.
//! - With autosave off (or held back, or failing), closing keeps the
//!   unsaved changes in the recovery journal as every close does, and the
//!   file is reported closed with them left out of it. The caller then sees
//!   the file as last saved: Git, for one, aborts a commit whose message was
//!   never saved. The changes come back at the next start.
//! - Once every file of the request is closed, the waiter is done. A client
//!   gets `done` and exits; a process that started Glyphvane itself for the
//!   wait quits, because the caller waits for that process to end.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gpui::{AnyWindowHandle, App};
use gv_ui::{CloseHook, FileClosed, Placement};

use crate::cli::OpenRequest;
use crate::instance::Reporter;

/// Who waits for the files.
#[derive(Debug)]
pub enum Waiter {
    /// This process, started with `--wait`: it quits when they are done.
    Local,
    /// A client connected to this instance.
    Remote(Reporter),
}

impl Waiter {
    fn closed(&self, closed: &FileClosed) {
        match self {
            Self::Local => {
                if closed.unsaved {
                    eprintln!("{}", unsaved_message(&closed.path));
                }
            }
            Self::Remote(reporter) => reporter.closed(closed.path.clone(), closed.unsaved),
        }
    }

    fn done(self, cx: &mut App) {
        match self {
            Self::Local => {
                tracing::info!("--wait: every file is closed; quitting");
                cx.defer(gv_ui::files::quit);
            }
            Self::Remote(reporter) => reporter.done(),
        }
    }
}

/// What the user is told about a file closed with changes that are not in
/// it.
pub fn unsaved_message(path: &Path) -> String {
    format!(
        "glyphvane: {} was closed with changes that are not saved in it; \
         they are kept for recovery at the next start",
        path.display()
    )
}

/// The files of one request that are still open.
#[derive(Debug)]
struct Pending {
    open: usize,
    waiter: Option<Waiter>,
}

/// Opens the files of `request` for `waiter` (see the module documentation).
/// A request without files is done at once and brings a window forward.
pub fn open(request: OpenRequest, waiter: Waiter, cx: &mut App) -> gpui::Result<AnyWindowHandle> {
    for folder in &request.folders {
        // A folder opens as a vault, but is not waited for.
        tracing::info!("open folder {}", folder.display());
        if let Err(err) = gv_ui::open_folder(cx, folder, Placement::RecentWindow) {
            tracing::error!("cannot open the folder {}: {err:#}", folder.display());
        }
    }
    for file in &request.files {
        tracing::info!("open {} for editing (waiting)", file.path.display());
    }
    if request.files.is_empty() {
        waiter.done(cx);
        return gv_ui::open(cx, Vec::new(), Placement::RecentWindow);
    }
    let pending = Rc::new(RefCell::new(Pending {
        open: request.files.len(),
        waiter: Some(waiter),
    }));
    let targets = request
        .files
        .into_iter()
        .map(|target| {
            let pending = Rc::clone(&pending);
            let hook = CloseHook::new(move |closed, cx| file_closed(&pending, &closed, cx));
            (target, hook)
        })
        .collect();
    gv_ui::open_for_edit(cx, targets, Placement::NewWindow)
}

fn file_closed(pending: &RefCell<Pending>, closed: &FileClosed, cx: &mut App) {
    tracing::info!(
        "--wait: {} closed{}",
        closed.path.display(),
        if closed.unsaved {
            " with unsaved changes"
        } else {
            ""
        }
    );
    let done = {
        let mut pending = pending.borrow_mut();
        if let Some(waiter) = &pending.waiter {
            waiter.closed(closed);
        }
        pending.open = pending.open.saturating_sub(1);
        if pending.open == 0 {
            pending.waiter.take()
        } else {
            None
        }
    };
    if let Some(waiter) = done {
        waiter.done(cx);
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};
    use gv_ui::shell::ViewMode;
    use gv_ui::{AppSettings, OpenTarget, ThemeChoice};

    use super::*;
    use crate::instance::WaitEvent;

    #[gpui::test]
    fn the_waiter_is_told_each_file_and_then_done(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().expect("temp dir");
        let (a, b) = (dir.path().join("a.md"), dir.path().join("b.md"));
        for path in [&a, &b] {
            std::fs::write(path, "# Note\n").expect("write");
        }
        let request = OpenRequest {
            cwd: dir.path().to_path_buf(),
            files: vec![OpenTarget::new(&a), OpenTarget::new(&b)],
            folders: Vec::new(),
            new_window: false,
            wait: true,
        };
        let (reporter, events) = Reporter::channel();
        let window = cx.update(|cx| {
            gv_ui::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
            open(request, Waiter::Remote(reporter), cx).expect("files open")
        });
        cx.run_until_parked();
        let workspace = cx.update(|cx| {
            gv_ui::most_recent_window(cx)
                .and_then(|w| w.workspace.upgrade())
                .expect("a workspace")
        });
        let modes = workspace.read_with(cx, |ws, _| {
            ws.tabs().iter().map(|t| t.mode()).collect::<Vec<_>>()
        });
        assert_eq!(modes, [ViewMode::Edit, ViewMode::Edit]);

        let close_first = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| {
                workspace.update(cx, |ws, cx| ws.close_tab(0, window, cx));
            })
            .expect("window open");
            cx.run_until_parked();
        };
        close_first(cx);
        assert_eq!(
            events.try_iter().collect::<Vec<_>>(),
            [WaitEvent::Closed {
                path: a,
                unsaved: false
            }],
            "one file is still open"
        );
        close_first(cx);
        assert_eq!(
            events.try_iter().collect::<Vec<_>>(),
            [
                WaitEvent::Closed {
                    path: b,
                    unsaved: false
                },
                WaitEvent::Done
            ]
        );
    }

    #[gpui::test]
    fn a_request_without_files_is_done_at_once(cx: &mut TestAppContext) {
        let (reporter, events) = Reporter::channel();
        let request = OpenRequest {
            cwd: std::env::temp_dir(),
            files: Vec::new(),
            folders: Vec::new(),
            new_window: false,
            wait: true,
        };
        cx.update(|cx| {
            gv_ui::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
            open(request, Waiter::Remote(reporter), cx).expect("a window");
        });
        assert_eq!(events.try_iter().collect::<Vec<_>>(), [WaitEvent::Done]);
    }
}
