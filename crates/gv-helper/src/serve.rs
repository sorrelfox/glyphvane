// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The helper's side: answers requests on standard input until it ends.

use std::io::{self, BufReader, BufWriter, ErrorKind};
use std::time::Duration;

use crate::protocol::{self, Fault, Hello, Job, Output, Request, Response};

/// Serves requests from standard input until it closes.
pub fn serve() -> io::Result<()> {
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = BufWriter::new(io::stdout().lock());
    loop {
        let frame = match protocol::read_frame(&mut input) {
            Ok(frame) => frame,
            // The parent closed our input: time to go.
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        let response = match protocol::decode::<Request>(&frame)? {
            Request::Hello(_) => Response::Hello(Hello::ours()),
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
        protocol::send(&mut output, &response)?;
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
                return Err("the page image is too large".to_owned());
            }
            Ok(Output::Page(image))
        }
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
