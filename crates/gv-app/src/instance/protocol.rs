// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The messages between a second instance (the client) and the first (the
//! server).
//!
//! Every message is one frame: the 4 bytes `GLYV`, the length of the body
//! as a little-endian `u32` (at most [`MAX_BODY`]), then the body, a JSON
//! object with a `version` field. JSON keeps the protocol readable and lets
//! later versions add fields that older ones ignore.
//!
//! The client sends one request and reads one response:
//!
//! ```json
//! {"version":1,"command":"open","cwd":{"utf8":"/home/me"},
//!  "files":[{"path":{"utf8":"/home/me/a.md"},"line":12,"column":3}],
//!  "folders":[],"new_window":false,"wait":false}
//! {"version":1,"status":"accepted"}
//! ```
//!
//! A path is `{"utf8": "..."}` when it is Unicode, else its raw form:
//! `{"unix": [bytes]}` or `{"windows": [UTF-16 units]}`. Every path is
//! absolute. A server that does not speak the request's version answers
//! `{"version":N,"status":"rejected","reason":"..."}` with its own version,
//! and the client then starts on its own.
//!
//! `wait` (FR-CLI-02) is reserved: a server that supports it keeps the
//! connection open after `accepted` and sends `{"version":1,"event":"closed",
//! "path":{...}}` for each file whose tab closes, then `{"version":1,
//! "event":"done"}`. Until then the server rejects `wait`.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use gv_ui::{OpenTarget, TextPosition};
use serde::{Deserialize, Serialize};

use crate::cli::OpenRequest;

/// The protocol version this build speaks.
pub const VERSION: u32 = 1;

/// The start of every frame.
const MAGIC: [u8; 4] = *b"GLYV";

/// The largest body accepted: thousands of paths.
pub const MAX_BODY: usize = 1 << 20;

/// The most files and folders one request may name.
const MAX_PATHS: usize = 10_000;

/// Writes one frame.
pub fn write_frame(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len())
        .ok()
        .filter(|&len| len as usize <= MAX_BODY)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "message too large"))?;
    let mut frame = Vec::with_capacity(8 + body.len());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(body);
    writer.write_all(&frame)?;
    writer.flush()
}

/// Reads one frame and returns its body.
pub fn read_frame(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut header = [0; 8];
    reader.read_exact(&mut header)?;
    if header[..4] != MAGIC {
        return Err(invalid("not a Glyphvane message"));
    }
    let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if len > MAX_BODY {
        return Err(invalid("message too large"));
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    Ok(body)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// A path as it travels: Unicode when it can be, else the platform's raw
/// form, so no path is lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WirePath {
    Utf8(String),
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}

impl WirePath {
    fn new(path: &Path) -> Self {
        if let Some(text) = path.to_str() {
            return Self::Utf8(text.to_owned());
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            Self::Unix(path.as_os_str().as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt as _;
            Self::Windows(path.as_os_str().encode_wide().collect())
        }
    }

    /// The path, which must be absolute.
    fn into_path(self) -> io::Result<PathBuf> {
        let path = match self {
            Self::Utf8(text) => PathBuf::from(text),
            #[cfg(unix)]
            Self::Unix(bytes) => {
                use std::os::unix::ffi::OsStringExt as _;
                PathBuf::from(OsString::from_vec(bytes))
            }
            #[cfg(windows)]
            Self::Windows(units) => {
                use std::os::windows::ffi::OsStringExt as _;
                PathBuf::from(OsString::from_wide(&units))
            }
            #[allow(unreachable_patterns)]
            _ => return Err(invalid("path from another platform")),
        };
        if path.is_absolute() {
            Ok(path)
        } else {
            Err(invalid(format!("relative path {}", path.display())))
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WireFile {
    path: WirePath,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    column: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct WireRequest {
    version: u32,
    command: String,
    cwd: WirePath,
    #[serde(default)]
    files: Vec<WireFile>,
    #[serde(default)]
    folders: Vec<WirePath>,
    #[serde(default)]
    new_window: bool,
    #[serde(default)]
    wait: bool,
}

/// Only the version, read before the rest so that any later version gets a
/// clear answer.
#[derive(Debug, Deserialize)]
struct Header {
    version: u32,
}

/// The server's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Accepted,
    Rejected,
}

impl Response {
    pub fn accepted() -> Self {
        Self {
            version: VERSION,
            status: Status::Accepted,
            reason: None,
        }
    }

    pub fn rejected(reason: impl Into<String>) -> Self {
        Self {
            version: VERSION,
            status: Status::Rejected,
            reason: Some(reason.into()),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        // Serializing these plain structs cannot fail.
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn decode(body: &[u8]) -> io::Result<Self> {
        serde_json::from_slice(body).map_err(|err| invalid(format!("bad response: {err}")))
    }
}

/// The request body for `request`.
pub fn encode_request(request: &OpenRequest) -> Vec<u8> {
    let wire = WireRequest {
        version: VERSION,
        command: "open".to_owned(),
        cwd: WirePath::new(&request.cwd),
        files: request
            .files
            .iter()
            .map(|file| WireFile {
                path: WirePath::new(&file.path),
                line: file.position.map(|p| p.line),
                column: file.position.and_then(|p| p.column),
            })
            .collect(),
        folders: request.folders.iter().map(|p| WirePath::new(p)).collect(),
        new_window: request.new_window,
        wait: request.wait,
    };
    serde_json::to_vec(&wire).unwrap_or_default()
}

/// Reads a request body. The error is the reason sent back to the client.
pub fn decode_request(body: &[u8]) -> Result<OpenRequest, String> {
    let header: Header =
        serde_json::from_slice(body).map_err(|err| format!("bad request: {err}"))?;
    if header.version != VERSION {
        return Err(format!(
            "protocol version {} is not supported (this instance speaks {VERSION})",
            header.version
        ));
    }
    let wire: WireRequest =
        serde_json::from_slice(body).map_err(|err| format!("bad request: {err}"))?;
    if wire.command != "open" {
        return Err(format!("unknown command `{}`", wire.command));
    }
    if wire.files.len() + wire.folders.len() > MAX_PATHS {
        return Err("too many paths".to_owned());
    }
    let path = |p: WirePath| p.into_path().map_err(|err| err.to_string());
    let files = wire
        .files
        .into_iter()
        .map(|file| {
            Ok(OpenTarget {
                path: path(file.path)?,
                position: file.line.map(|line| TextPosition {
                    line: line.max(1),
                    column: file.column.map(|c| c.max(1)),
                }),
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(OpenRequest {
        cwd: path(wire.cwd)?,
        files,
        folders: wire
            .folders
            .into_iter()
            .map(path)
            .collect::<Result<_, _>>()?,
        new_window: wire.new_window,
        wait: wire.wait,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        std::env::temp_dir()
    }

    fn sample() -> OpenRequest {
        let root = root();
        OpenRequest {
            cwd: root.clone(),
            files: vec![
                OpenTarget::new(root.join("a.md")).at(TextPosition {
                    line: 12,
                    column: Some(3),
                }),
                OpenTarget::new(root.join("b c.md")),
            ],
            folders: vec![root.join("vault")],
            new_window: true,
            wait: false,
        }
    }

    #[test]
    fn requests_survive_a_round_trip() {
        let request = sample();
        assert_eq!(decode_request(&encode_request(&request)), Ok(request));
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_paths_survive() {
        use std::os::unix::ffi::OsStringExt as _;
        let mut request = sample();
        let name = OsString::from_vec(b"/tmp/\xff.md".to_vec());
        request.files = vec![OpenTarget::new(PathBuf::from(name))];
        let body = encode_request(&request);
        assert!(String::from_utf8_lossy(&body).contains("\"unix\":"));
        assert_eq!(decode_request(&body), Ok(request));
    }

    #[test]
    fn frames_round_trip_and_reject_garbage() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, b"{}").expect("frame written");
        assert_eq!(&buffer[..4], b"GLYV");
        assert_eq!(
            read_frame(&mut buffer.as_slice()).expect("frame read"),
            b"{}"
        );

        let mut wrong = buffer.clone();
        wrong[0] = b'X';
        assert!(read_frame(&mut wrong.as_slice()).is_err());

        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&(MAX_BODY as u32 + 1).to_le_bytes());
        let err = read_frame(&mut huge.as_slice()).expect_err("too large");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        // A short frame is an error, not a hang.
        let short = &buffer[..buffer.len() - 1];
        assert!(read_frame(&mut &short[..]).is_err());
    }

    #[test]
    fn other_versions_and_bad_requests_are_rejected() {
        let later = br#"{"version":2,"command":"open","cwd":{"utf8":"/"},"shiny":true}"#;
        let err = decode_request(later).expect_err("version 2");
        assert!(err.contains("version 2"), "{err}");

        assert!(decode_request(b"[1,2]").is_err());
        let relative = br#"{"version":1,"command":"open","cwd":{"utf8":"rel"}}"#;
        assert!(decode_request(relative).is_err());
        let command = br#"{"version":1,"command":"rm","cwd":{"utf8":"/"}}"#;
        assert!(decode_request(command).is_err());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&encode_request(&sample())).expect("json");
        value["later_flag"] = serde_json::Value::Bool(true);
        let body = serde_json::to_vec(&value).expect("json");
        assert_eq!(decode_request(&body), Ok(sample()));
    }

    #[test]
    fn responses_round_trip() {
        for response in [Response::accepted(), Response::rejected("no")] {
            assert_eq!(Response::decode(&response.encode()).ok(), Some(response));
        }
        assert_eq!(
            Response::accepted().encode(),
            br#"{"version":1,"status":"accepted"}"#
        );
    }
}
