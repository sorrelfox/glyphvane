// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The app's side: finding, starting and supervising `glyphvane-helper`.
//!
//! - The helper is started on demand, kept warm as a pool of one, and
//!   stopped after [`HelperConfig::idle`] without work.
//! - Every job has a deadline. The parent enforces it: on timeout, on
//!   cancellation, or when the helper dies or answers nonsense, the parent
//!   kills its own child ([`Child::kill`], the PID it spawned), reaps it,
//!   and the next job starts a new one. A cancelled job that ends within
//!   [`CANCEL_GRACE`] keeps the helper (and the document it holds open):
//!   waiting is cheaper than starting a new helper and opening it again.
//! - The helper runs with a bounded thread pool and malloc arena count, so
//!   its address-space limit (Linux) measures real use.

use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use crate::protocol::{
    self, DocInfo, Hello, Job, OpenJob, Output, PageImage, PageJob, PageText, PdfJob, PdfOutput,
    RenderJob, Request, Response, TextJob,
};

/// The helper executable's name.
pub const HELPER_NAME: &str = "glyphvane-helper";

/// How long a new helper may take to answer the handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);

/// How often a waiting job looks at its cancel flag.
const POLL: Duration = Duration::from_millis(25);

/// How long a cancelled job may still take to end before the helper is
/// killed.
pub const CANCEL_GRACE: Duration = Duration::from_millis(500);

/// Finds the helper next to the running executable, never through `PATH`:
///
/// 1. in debug builds, the `GLYPHVANE_HELPER` environment variable;
/// 2. next to the executable (symlinks resolved, so a macOS
///    `/usr/local/bin/glyphvane` link finds the bundle's helper);
/// 3. on Linux, `<exe_dir>/../libexec/glyphvane/` (deb, rpm, Flatpak)
///    or `<exe_dir>/../lib/glyphvane/` (Arch Linux);
/// 4. the build-time `GLYPHVANE_HELPER_PATH`, for distribution packagers.
pub fn locate() -> Option<PathBuf> {
    if cfg!(debug_assertions)
        && let Some(path) = std::env::var_os("GLYPHVANE_HELPER")
    {
        return Some(PathBuf::from(path));
    }
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let name = format!("{HELPER_NAME}{}", std::env::consts::EXE_SUFFIX);
    let dir = exe.parent()?;
    let mut candidates = vec![dir.join(&name)];
    if cfg!(target_os = "linux") {
        candidates.push(dir.join("../libexec/glyphvane").join(&name));
        candidates.push(dir.join("../lib/glyphvane").join(&name));
    }
    if let Some(path) = option_env!("GLYPHVANE_HELPER_PATH") {
        candidates.push(PathBuf::from(path));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// How a helper is started.
#[derive(Debug, Clone)]
pub struct HelperConfig {
    /// The helper executable.
    pub program: PathBuf,
    /// The helper's address-space limit in MiB (enforced on Linux).
    pub memory_limit_mb: Option<u64>,
    /// Typst's worker threads.
    pub threads: u32,
    /// A helper without work for this long is stopped.
    pub idle: Duration,
}

impl HelperConfig {
    /// The defaults of the plan: 2 GiB, 4 threads, stopped after a minute
    /// without work.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            memory_limit_mb: Some(2048),
            threads: 4,
            idle: Duration::from_secs(60),
        }
    }
}

/// Why a job did not produce its output.
#[derive(Debug)]
pub enum HelperError {
    /// No helper executable was found.
    NotFound,
    /// The helper could not be started.
    Start(io::Error),
    /// The helper is from another build.
    Mismatch(Hello),
    /// The deadline passed; the helper was stopped.
    Timeout(Duration),
    /// The job was cancelled; the helper was stopped.
    Cancelled,
    /// The helper died (a crash, or its memory limit).
    Crashed(String),
    /// The engine reported an error.
    Failed(String),
    /// The helper answered something unexpected.
    Protocol(String),
}

impl std::fmt::Display for HelperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "the PDF engine ({HELPER_NAME}) is not installed"),
            Self::Start(e) => write!(f, "the PDF engine could not be started: {e}"),
            Self::Mismatch(hello) => write!(
                f,
                "the PDF engine is from another version ({}); restart Glyphvane",
                hello.version
            ),
            Self::Timeout(d) => write!(f, "the PDF engine took longer than {} s", d.as_secs()),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Crashed(how) => write!(f, "the PDF engine stopped unexpectedly ({how})"),
            Self::Failed(message) => write!(f, "{message}"),
            Self::Protocol(message) => write!(f, "the PDF engine answered badly: {message}"),
        }
    }
}

impl std::error::Error for HelperError {}

/// A started helper.
struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: Receiver<io::Result<Vec<u8>>>,
}

impl Running {
    fn spawn(config: &HelperConfig) -> Result<Self, HelperError> {
        let mut cmd = Command::new(&config.program);
        cmd.arg("--serve")
            .env("RAYON_NUM_THREADS", config.threads.to_string())
            .env("MALLOC_ARENA_MAX", "2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(mb) = config.memory_limit_mb {
            cmd.arg("--memory-limit-mb").arg(mb.to_string());
        }
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == ErrorKind::NotFound {
                HelperError::NotFound
            } else {
                HelperError::Start(e)
            }
        })?;
        let (Some(stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(HelperError::Start(io::Error::other("no pipes")));
        };
        let (tx, frames) = mpsc::channel();
        std::thread::Builder::new()
            .name("helper-reader".into())
            .spawn(move || {
                loop {
                    let frame = protocol::read_frame(&mut stdout);
                    let stop = frame.is_err();
                    if tx.send(frame).is_err() || stop {
                        break;
                    }
                }
            })
            .map_err(HelperError::Start)?;
        let mut running = Self {
            child,
            stdin: Some(stdin),
            frames,
        };
        running.handshake()?;
        Ok(running)
    }

    fn handshake(&mut self) -> Result<(), HelperError> {
        self.send(&Request::Hello(Hello::ours()))?;
        match self.frames.recv_timeout(HANDSHAKE) {
            Ok(Ok(frame)) => match protocol::decode::<Response>(&frame) {
                Ok(Response::Hello(hello)) if hello == Hello::ours() => Ok(()),
                Ok(Response::Hello(hello)) => Err(HelperError::Mismatch(hello)),
                Ok(_) => Err(HelperError::Protocol("no handshake".into())),
                Err(e) => Err(HelperError::Protocol(e.to_string())),
            },
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                Err(HelperError::Crashed(self.reap()))
            }
            Err(RecvTimeoutError::Timeout) => Err(HelperError::Timeout(HANDSHAKE)),
        }
    }

    fn send(&mut self, request: &Request) -> Result<(), HelperError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| HelperError::Protocol("closed".into()))?;
        protocol::send(stdin, request).map_err(|e| {
            if e.kind() == ErrorKind::BrokenPipe {
                HelperError::Crashed("its input closed".into())
            } else {
                HelperError::Protocol(e.to_string())
            }
        })
    }

    /// Waits for the child to exit and says how it did.
    fn reap(&mut self) -> String {
        match self.child.wait() {
            Ok(status) => status.to_string(),
            Err(e) => e.to_string(),
        }
    }

    /// Kills and reaps the child.
    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Asks the child to exit (end of input) and reaps it.
    fn stop(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(POLL);
        }
        self.kill();
    }
}

struct State {
    running: Option<Running>,
    next_id: u64,
    last_use: Instant,
    watchdog: bool,
}

struct Shared {
    config: HelperConfig,
    state: Mutex<State>,
}

/// A supervised helper process, started on demand. Jobs run one at a time.
#[derive(Clone)]
pub struct Helper {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Helper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Helper")
            .field("config", &self.shared.config)
            .finish_non_exhaustive()
    }
}

impl Helper {
    pub fn new(config: HelperConfig) -> Self {
        Self {
            shared: Arc::new(Shared {
                config,
                state: Mutex::new(State {
                    running: None,
                    next_id: 1,
                    last_use: Instant::now(),
                    watchdog: false,
                }),
            }),
        }
    }

    /// The helper [`locate`] finds, with the default limits.
    pub fn installed() -> Result<Self, HelperError> {
        locate()
            .map(|path| Self::new(HelperConfig::new(path)))
            .ok_or(HelperError::NotFound)
    }

    /// The process id of the running helper, if one runs.
    pub fn pid(&self) -> Option<u32> {
        self.lock().running.as_ref().map(|r| r.child.id())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Compiles a Typst document to PDF.
    pub fn pdf(
        &self,
        job: PdfJob,
        deadline: Duration,
        max_output: u64,
        cancel: &AtomicBool,
    ) -> Result<PdfOutput, HelperError> {
        match self.run(Job::Pdf(job), deadline, max_output, cancel)? {
            Output::Pdf(pdf) => Ok(pdf),
            _ => Err(HelperError::Protocol("no PDF".into())),
        }
    }

    /// Draws a page of a PDF.
    pub fn page(
        &self,
        job: PageJob,
        deadline: Duration,
        cancel: &AtomicBool,
    ) -> Result<PageImage, HelperError> {
        let max = u64::from(job.max_width.min(crate::protocol::MAX_PAGE_SIDE))
            * u64::from(job.max_height.min(crate::protocol::MAX_PAGE_SIDE))
            * 4;
        match self.run(Job::Page(job), deadline, max, cancel)? {
            Output::Page(image) => Ok(image),
            _ => Err(HelperError::Protocol("no page image".into())),
        }
    }

    /// Reads `pdf` and keeps it (as `key`) for [`Helper::render`];
    /// returns its pages.
    pub fn open(
        &self,
        key: u64,
        pdf: Vec<u8>,
        deadline: Duration,
        cancel: &AtomicBool,
    ) -> Result<DocInfo, HelperError> {
        match self.run(Job::Open(OpenJob { key, pdf }), deadline, 0, cancel)? {
            Output::Document(info) => Ok(info),
            _ => Err(HelperError::Protocol("no page sizes".into())),
        }
    }

    /// Draws a page of the PDF [`Helper::open`] kept as `job.key`;
    /// `None` when the helper does not hold it (it was restarted, or
    /// another PDF was opened since): open it again.
    pub fn render(
        &self,
        job: RenderJob,
        deadline: Duration,
        cancel: &AtomicBool,
    ) -> Result<Option<PageImage>, HelperError> {
        let max = job.max_pixels.clamp(1, protocol::MAX_RENDER_PIXELS) * 4;
        match self.run(Job::Render(job), deadline, max, cancel)? {
            Output::Page(image) => Ok(Some(image)),
            Output::NoDocument => Ok(None),
            _ => Err(HelperError::Protocol("no page image".into())),
        }
    }

    /// The text of a page of the PDF [`Helper::open`] kept as `job.key`,
    /// with each character's box; `None` when the helper does not hold it
    /// (open it again).
    pub fn text(
        &self,
        job: TextJob,
        deadline: Duration,
        cancel: &AtomicBool,
    ) -> Result<Option<PageText>, HelperError> {
        match self.run(Job::Text(job), deadline, protocol::MAX_TEXT_BYTES, cancel)? {
            Output::Text(text) => Ok(Some(text)),
            Output::NoDocument => Ok(None),
            _ => Err(HelperError::Protocol("no page text".into())),
        }
    }

    /// Runs `job` in the helper (started if needed) within `deadline`.
    pub fn run(
        &self,
        job: Job,
        deadline: Duration,
        max_output: u64,
        cancel: &AtomicBool,
    ) -> Result<Output, HelperError> {
        let mut state = self.lock();
        if cancel.load(Ordering::Relaxed) {
            return Err(HelperError::Cancelled);
        }
        let mut running = match state.running.take() {
            Some(running) => running,
            None => Running::spawn(&self.shared.config)?,
        };
        let id = state.next_id;
        state.next_id += 1;
        let request = Request::Job {
            id,
            deadline_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
            max_output,
            job,
        };
        let started = Instant::now();
        let result = Self::exchange(&mut running, &request, id, deadline, cancel);
        state.last_use = Instant::now();
        match &result {
            Ok(_) | Err(HelperError::Failed(_)) => state.running = Some(running),
            Err(HelperError::Crashed(_)) => {
                let _ = running.child.wait();
            }
            Err(HelperError::Cancelled) => {
                let left = deadline.saturating_sub(started.elapsed());
                if Self::drain(&running, id, CANCEL_GRACE.min(left)) {
                    state.running = Some(running);
                } else {
                    running.kill();
                }
            }
            Err(_) => running.kill(),
        }
        if state.running.is_some() && !state.watchdog {
            state.watchdog = self.start_watchdog();
        }
        result
    }

    fn exchange(
        running: &mut Running,
        request: &Request,
        id: u64,
        deadline: Duration,
        cancel: &AtomicBool,
    ) -> Result<Output, HelperError> {
        running.send(request)?;
        let end = Instant::now() + deadline;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(HelperError::Cancelled);
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(HelperError::Timeout(deadline));
            }
            match running.frames.recv_timeout(left.min(POLL)) {
                Ok(Ok(frame)) => {
                    return match protocol::decode::<Response>(&frame) {
                        Ok(Response::Done { id: got, result }) if got == id => {
                            result.map_err(HelperError::Failed)
                        }
                        Ok(_) => Err(HelperError::Protocol("an unexpected answer".into())),
                        Err(e) => Err(HelperError::Protocol(e.to_string())),
                    };
                }
                Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(HelperError::Crashed(running.reap()));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Waits up to `grace` for the answer to cancelled job `id`: whether
    /// it came (the helper is free again; else it is to be killed).
    fn drain(running: &Running, id: u64, grace: Duration) -> bool {
        let end = Instant::now() + grace;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match running.frames.recv_timeout(left) {
                Ok(Ok(frame)) => {
                    return matches!(
                        protocol::decode::<Response>(&frame),
                        Ok(Response::Done { id: got, .. }) if got == id
                    );
                }
                Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => return false,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Stops the helper after the idle time. Returns whether it started.
    fn start_watchdog(&self) -> bool {
        let weak: Weak<Shared> = Arc::downgrade(&self.shared);
        let idle = self.shared.config.idle;
        let tick = idle.min(Duration::from_secs(5)).max(POLL);
        std::thread::Builder::new()
            .name("helper-idle".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(tick);
                    let Some(shared) = weak.upgrade() else {
                        return;
                    };
                    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if state.running.is_none() {
                        state.watchdog = false;
                        return;
                    }
                    if state.last_use.elapsed() >= idle
                        && let Some(running) = state.running.take()
                    {
                        running.stop();
                        state.watchdog = false;
                        return;
                    }
                }
            })
            .is_ok()
    }

    /// Stops the helper now (it starts again on the next job).
    pub fn shutdown(&self) {
        if let Some(running) = self.lock().running.take() {
            running.stop();
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = state.running.take() {
            running.stop();
        }
    }
}
