// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The output stream: cpal on the default device, or a silent stand-in that
//! never opens a device.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use cpal::{FromSample, SampleFormat, SizedSample};

use super::AudioError;

/// The device's frame format. Samples are `f32` on our side; the stream
/// converts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFormat {
    pub sample_rate: u32,
    pub channels: usize,
}

/// Fills one buffer of interleaved samples in the given format. Runs on the
/// output thread: it must not block for long or allocate.
pub type RenderFn = Box<dyn FnMut(&mut [f32], OutputFormat) + Send>;

/// Opens output streams.
pub trait AudioBackend: Send + Sync {
    fn open(&self, render: RenderFn) -> Result<Box<dyn OutputStream>, AudioError>;
}

/// An open output stream. Created suspended.
pub trait OutputStream: Send {
    fn format(&self) -> OutputFormat;
    fn resume(&mut self) -> Result<(), AudioError>;
    fn suspend(&mut self);
    /// The device went away or failed: the engine opens a new stream.
    fn is_broken(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Null output

#[derive(Default)]
struct NullState {
    render: Option<RenderFn>,
    running: bool,
    opens: usize,
}

/// An output that never touches a device: [`NullOutput::pull`] runs the
/// render callback on demand. For tests, and for machines without sound.
#[derive(Clone)]
pub struct NullBackend {
    state: Arc<Mutex<NullState>>,
    format: OutputFormat,
    fail: bool,
}

impl std::fmt::Debug for NullBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NullBackend")
            .field("format", &self.format)
            .field("fail", &self.fail)
            .finish_non_exhaustive()
    }
}

/// The test side of a [`NullBackend`].
#[derive(Clone)]
pub struct NullOutput {
    state: Arc<Mutex<NullState>>,
    format: OutputFormat,
}

impl std::fmt::Debug for NullOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NullOutput")
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

impl NullBackend {
    /// A silent output in `format`, and the handle that drives it.
    pub fn new(format: OutputFormat) -> (Self, NullOutput) {
        let state = Arc::new(Mutex::new(NullState::default()));
        (
            Self {
                state: Arc::clone(&state),
                format,
                fail: false,
            },
            NullOutput { state, format },
        )
    }

    /// An output whose device is always missing.
    pub fn without_device() -> (Self, NullOutput) {
        let (mut backend, output) = Self::new(OutputFormat {
            sample_rate: 48_000,
            channels: 2,
        });
        backend.fail = true;
        (backend, output)
    }
}

impl AudioBackend for NullBackend {
    fn open(&self, render: RenderFn) -> Result<Box<dyn OutputStream>, AudioError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if self.fail {
            return Err(AudioError::NoOutputDevice);
        }
        state.opens += 1;
        state.render = Some(render);
        state.running = false;
        Ok(Box::new(NullStream {
            state: Arc::clone(&self.state),
            format: self.format,
        }))
    }
}

struct NullStream {
    state: Arc<Mutex<NullState>>,
    format: OutputFormat,
}

impl OutputStream for NullStream {
    fn format(&self) -> OutputFormat {
        self.format
    }

    fn resume(&mut self) -> Result<(), AudioError> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running = true;
        Ok(())
    }

    fn suspend(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running = false;
    }
}

impl Drop for NullStream {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.render = None;
        state.running = false;
    }
}

impl NullOutput {
    pub fn format(&self) -> OutputFormat {
        self.format
    }

    /// Renders `frames` frames as the device would; silence while the stream
    /// is suspended or not open.
    pub fn pull(&self, frames: usize) -> Vec<f32> {
        let mut buf = vec![0.0; frames * self.format.channels];
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.running
            && let Some(render) = state.render.as_mut()
        {
            render(&mut buf, self.format);
        }
        buf
    }

    /// How many times a stream was opened.
    pub fn opens(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .opens
    }

    /// Whether the stream is open and running.
    pub fn is_running(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.running && state.render.is_some()
    }
}

// ---------------------------------------------------------------------------
// cpal output

/// The system's default output device through cpal. The cpal stream lives on
/// its own thread, so the handle can move between threads on every
/// platform.
#[derive(Debug, Default, Clone, Copy)]
pub struct CpalBackend;

enum Command {
    Resume(mpsc::Sender<Result<(), AudioError>>),
    Suspend,
}

struct CpalStream {
    commands: mpsc::Sender<Command>,
    format: OutputFormat,
    broken: Arc<AtomicBool>,
}

impl AudioBackend for CpalBackend {
    fn open(&self, render: RenderFn) -> Result<Box<dyn OutputStream>, AudioError> {
        let (opened_tx, opened_rx) = mpsc::channel();
        let (commands, commands_rx) = mpsc::channel();
        let broken = Arc::new(AtomicBool::new(false));
        let thread_broken = Arc::clone(&broken);
        std::thread::Builder::new()
            .name("gv-audio-output".into())
            .spawn(move || run_output(render, &opened_tx, &commands_rx, thread_broken))
            .map_err(|e| AudioError::Output(e.to_string()))?;
        let format = opened_rx
            .recv()
            .map_err(|_| AudioError::Output("the output thread stopped".into()))??;
        Ok(Box::new(CpalStream {
            commands,
            format,
            broken,
        }))
    }
}

impl OutputStream for CpalStream {
    fn format(&self) -> OutputFormat {
        self.format
    }

    fn resume(&mut self) -> Result<(), AudioError> {
        let (tx, rx) = mpsc::channel();
        self.commands
            .send(Command::Resume(tx))
            .map_err(|_| AudioError::Output("the output thread stopped".into()))?;
        rx.recv()
            .map_err(|_| AudioError::Output("the output thread stopped".into()))?
    }

    fn suspend(&mut self) {
        let _ = self.commands.send(Command::Suspend);
    }

    fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }
}

/// The output thread: opens the device, reports the format, then plays and
/// pauses on command until the handle is dropped.
fn run_output(
    render: RenderFn,
    opened: &mpsc::Sender<Result<OutputFormat, AudioError>>,
    commands: &mpsc::Receiver<Command>,
    broken: Arc<AtomicBool>,
) {
    let stream = match open_default(render, broken) {
        Ok((stream, format)) => {
            let _ = opened.send(Ok(format));
            stream
        }
        Err(e) => {
            let _ = opened.send(Err(e));
            return;
        }
    };
    while let Ok(command) = commands.recv() {
        match command {
            Command::Resume(reply) => {
                let result = stream.play().map_err(|e| AudioError::Output(e.to_string()));
                let _ = reply.send(result);
            }
            Command::Suspend => {
                if let Err(e) = stream.pause() {
                    log::debug!("audio output: pause failed: {e}");
                }
            }
        }
    }
}

fn open_default(
    render: RenderFn,
    broken: Arc<AtomicBool>,
) -> Result<(cpal::Stream, OutputFormat), AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(AudioError::NoOutputDevice)?;
    let supported = device
        .default_output_config()
        .map_err(|_| AudioError::NoOutputDevice)?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let format = OutputFormat {
        sample_rate: config.sample_rate,
        channels: usize::from(config.channels),
    };
    if format.channels == 0 || format.sample_rate == 0 {
        return Err(AudioError::Output("the device reports no channels".into()));
    }
    let stream = match sample_format {
        SampleFormat::F32 => build::<f32>(&device, config, format, render, broken),
        SampleFormat::I16 => build::<i16>(&device, config, format, render, broken),
        SampleFormat::I32 => build::<i32>(&device, config, format, render, broken),
        SampleFormat::U16 => build::<u16>(&device, config, format, render, broken),
        SampleFormat::U8 => build::<u8>(&device, config, format, render, broken),
        SampleFormat::I8 => build::<i8>(&device, config, format, render, broken),
        SampleFormat::F64 => build::<f64>(&device, config, format, render, broken),
        other => Err(AudioError::Output(format!(
            "unsupported device sample format {other}"
        ))),
    }?;
    // Streams may start playing when built; start suspended.
    let _ = stream.pause();
    Ok((stream, format))
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    format: OutputFormat,
    mut render: RenderFn,
    broken: Arc<AtomicBool>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    // Grown once to the largest buffer the device asks for, then reused.
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                if scratch.len() < data.len() {
                    scratch.resize(data.len(), 0.0);
                }
                let buf = &mut scratch[..data.len()];
                buf.fill(0.0);
                render(buf, format);
                for (out, sample) in data.iter_mut().zip(buf.iter()) {
                    *out = T::from_sample(*sample);
                }
            },
            move |err: cpal::Error| match err.kind() {
                cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied => {
                    log::debug!("audio output: {err}");
                }
                _ => {
                    log::warn!("audio output failed: {err}");
                    broken.store(true, Ordering::Relaxed);
                }
            },
            None,
        )
        .map_err(|e| AudioError::Output(e.to_string()))
}
