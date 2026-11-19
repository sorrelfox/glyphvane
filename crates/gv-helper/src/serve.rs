// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The helper's side: answers requests on standard input until it ends.
//!
//! Requests are answered one at a time, in order. A [`Job::Open`] starts a
//! session: the PDF and hayro's caches are kept while the next requests
//! draw pages of it and read their text ([`Job::Render`] and [`Job::Text`]
//! with its key); any other request ends the session and is answered as
//! usual. A [`Job::Render`] or [`Job::Text`] outside its session is
//! answered with [`Output::NoDocument`].

use std::io::{self, BufReader, BufWriter, ErrorKind, Read, Write};
use std::time::Duration;

use crate::protocol::{self, Fault, Hello, Job, Output, Request, Response};

/// Serves requests from standard input until it closes.
pub fn serve() -> io::Result<()> {
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = BufWriter::new(io::stdout().lock());
    serve_on(&mut input, &mut output)
}

/// Serves requests from `input` until it ends, answering on `output`.
pub fn serve_on(input: &mut impl Read, output: &mut impl Write) -> io::Result<()> {
    let mut pending: Option<Request> = None;
    loop {
        let request = match pending.take() {
            Some(request) => request,
            None => match next(input)? {
                Some(request) => request,
                // The parent closed our input: time to go.
                None => return Ok(()),
            },
        };
        let response = match request {
            Request::Hello(_) => Response::Hello(Hello::ours()),
            Request::Job {
                id,
                job: Job::Open(open),
                ..
            } => match crate::raster::open(open.pdf) {
                Ok(pdf) => {
                    let info = crate::raster::info(&pdf);
                    protocol::send(
                        output,
                        &Response::Done {
                            id,
                            result: Ok(Output::Document(info)),
                        },
                    )?;
                    match session(&pdf, open.key, input, output)? {
                        Some(next) => {
                            pending = Some(next);
                            continue;
                        }
                        None => return Ok(()),
                    }
                }
                Err(message) => Response::Done {
                    id,
                    result: Err(message),
                },
            },
            Request::Job {
                id,
                job: Job::Render(_) | Job::Text(_),
                ..
            } => Response::Done {
                id,
                result: Ok(Output::NoDocument),
            },
            Request::Job {
                id,
                max_output,
                job,
                ..
            } => Response::Done {
                id,
                result: run(job, max_output),
            },
        };
        protocol::send(output, &response)?;
    }
}

/// The next request; `None` once the input ends.
fn next(input: &mut impl Read) -> io::Result<Option<Request>> {
    match protocol::read_frame(input) {
        Ok(frame) => protocol::decode::<Request>(&frame).map(Some),
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

/// Draws pages of `pdf` (kept as `key`) and reads their text until a
/// request for something else comes, which is returned; `None` once the input ends.
fn session(
    pdf: &hayro::hayro_syntax::Pdf,
    key: u64,
    input: &mut impl Read,
    output: &mut impl Write,
) -> io::Result<Option<Request>> {
    let mut session = crate::raster::Session::new(pdf);
    loop {
        let Some(request) = next(input)? else {
            return Ok(None);
        };
        match request {
            Request::Job {
                id,
                max_output,
                job: Job::Render(job),
                ..
            } if job.key == key && !session.broken => {
                let result = session.render(&job).and_then(|image| {
                    if image.rgba.len() as u64 > max_output {
                        Err("The page image is too large.".to_owned())
                    } else {
                        Ok(Output::Page(image))
                    }
                });
                protocol::send(output, &Response::Done { id, result })?;
            }
            Request::Job {
                id,
                max_output,
                job: Job::Text(job),
                ..
            } if job.key == key && !session.broken => {
                let result = session.text(&job).and_then(|text| {
                    if text.bytes() > max_output {
                        Err("The page has too much text.".to_owned())
                    } else {
                        Ok(Output::Text(text))
                    }
                });
                protocol::send(output, &Response::Done { id, result })?;
            }
            other => return Ok(Some(other)),
        }
    }
}

fn run(job: Job, max_output: u64) -> Result<Output, String> {
    match job {
        Job::Pdf(job) => {
            let pdf = crate::engine::pdf(&job)?;
            if pdf.pdf.len() as u64 > max_output {
                return Err(format!(
                    "the PDF is larger than {} MB",
                    max_output / (1024 * 1024)
                ));
            }
            Ok(Output::Pdf(pdf))
        }
        Job::Page(job) => {
            let image = crate::raster::page(&job)?;
            if image.rgba.len() as u64 > max_output {
                return Err("The page image is too large.".to_owned());
            }
            Ok(Output::Page(image))
        }
        // Answered by `serve_on` and `session`.
        Job::Open(_) | Job::Render(_) | Job::Text(_) => Ok(Output::NoDocument),
        Job::Fault(Fault::Sleep { ms }) => {
            std::thread::sleep(Duration::from_millis(ms));
            Ok(Output::Done)
        }
        Job::Fault(Fault::Exit) => std::process::exit(3),
        Job::Fault(Fault::Allocate { mb }) => {
            let mut blocks: Vec<Vec<u8>> = Vec::new();
            for _ in 0..mb {
                blocks.push(vec![1u8; 1 << 20]);
            }
            std::hint::black_box(&blocks);
            Ok(Output::Done)
        }
    }
}

/// Limits the process's address space to `mb` MiB (Linux; elsewhere the
/// parent's deadline is the only limit for now).
pub fn limit_memory(mb: u64) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use rustix::process::{Resource, Rlimit, setrlimit};
        let bytes = mb.saturating_mul(1024 * 1024);
        setrlimit(
            Resource::As,
            Rlimit {
                current: Some(bytes),
                maximum: Some(bytes),
            },
        )?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = mb;
    Ok(())
}
