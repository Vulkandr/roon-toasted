// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Protocol details from arthursoares/roon-api-reverse-engineering (MIT), see NOTICE.

//! Remoting frames: how requests, responses and the Core's pushes are packaged
//! on the TCP stream once the handshake is done.
//!
//! ```text
//! [header byte] [flexInt request id, sometimes] [flexInt body length] [body]
//! ```
//!
//! Header bit 0x80 clear: a request. Low 6 bits = command; bit 0x40 means a
//! response is expected and a request id follows. Header bit 0x80 set: a
//! response. Bit 0x40 = final chunk; a request id always follows.

use crate::wire::{read_flex_int, write_flex_int};

/// Commands we send.
pub mod cmd {
    pub const PING: u8 = 1;
    pub const GETSVC: u8 = 2;
    pub const CALL: u8 = 3;
    pub const DEFTYPE: u8 = 5;
    pub const DEFMETHOD: u8 = 6;
    pub const SENDMSG: u8 = 7;
}

/// Pushes the Core sends us (same field as `cmd`, different meaning).
pub mod push {
    pub const EVENT: u8 = 2;
    pub const PUSHOBJ: u8 = 3;
    pub const PUSHSTUB: u8 = 4;
    pub const UPDATEOBJ: u8 = 5;
    pub const FLUSH: u8 = 6;
    pub const DEFTYPE: u8 = 7;
    pub const DEFEVENT: u8 = 8;
    pub const FLUSHRESUME: u8 = 9;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub is_response: bool,
    /// The command or push kind (requests and pushes only).
    pub cmd: u8,
    /// Request id: on every response, and on requests that want one.
    pub rid: Option<u32>,
    /// Responses only: whether this is the last chunk.
    pub is_final: bool,
    pub body: Vec<u8>,
}

pub fn encode_request(cmd: u8, body: &[u8], rid: Option<u32>) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    match rid {
        Some(rid) => {
            out.push((cmd & 0x3f) | 0x40);
            write_flex_int(&mut out, rid);
        }
        None => out.push(cmd & 0x3f),
    }
    write_flex_int(&mut out, body.len() as u32);
    out.extend_from_slice(body);
    out
}

pub fn encode_response(rid: u32, body: &[u8], is_final: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    out.push(0x80 | if is_final { 0x40 } else { 0 });
    write_flex_int(&mut out, rid);
    write_flex_int(&mut out, body.len() as u32);
    out.extend_from_slice(body);
    out
}

/// Turns socket chunks into complete frames. A frame may arrive split over
/// several reads, and one read may hold several frames.
#[derive(Default)]
pub struct FrameParser {
    buf: Vec<u8>,
}

impl FrameParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<Frame> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        let mut consumed = 0;
        while let Some((frame, end)) = Self::parse_one(&self.buf[consumed..]) {
            frames.push(frame);
            consumed += end;
        }
        if consumed > 0 {
            self.buf.drain(..consumed);
        }
        frames
    }

    /// Bytes waiting for the rest of a frame.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// One frame from the start of `u`, with how many bytes it took; None if incomplete.
    fn parse_one(u: &[u8]) -> Option<(Frame, usize)> {
        if u.len() < 2 {
            return None;
        }
        let header = u[0];
        let is_response = header & 0x80 != 0;
        let mut pos = 1;
        let mut cmd = 0;
        let mut rid = None;
        let mut is_final = false;

        if is_response {
            is_final = header & 0x40 != 0;
            let (r, p) = read_flex_int(u, pos).ok()?;
            rid = Some(r);
            pos = p;
        } else {
            cmd = header & 0x3f;
            if header & 0x40 != 0 {
                let (r, p) = read_flex_int(u, pos).ok()?;
                rid = Some(r);
                pos = p;
            }
        }

        let (len, body_start) = read_flex_int(u, pos).ok()?;
        let end = body_start.checked_add(len as usize)?;
        if u.len() < end {
            return None;
        }
        let frame = Frame {
            is_response,
            cmd,
            rid,
            is_final,
            body: u[body_start..end].to_vec(),
        };
        Some((frame, end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn encode_and_parse() {
        let req = encode_request(cmd::CALL, &[1, 2, 3], Some(17));
        assert_eq!(hex(&req), "431103010203");
        let no_reply = encode_request(cmd::DEFMETHOD, &[9], None);
        assert_eq!(hex(&no_reply), "060109");
        let resp = encode_response(17, &[], true);
        assert_eq!(hex(&resp), "c01100");

        let all: Vec<u8> = [req, no_reply, resp].concat();
        // one byte at a time
        let mut parser = FrameParser::new();
        let mut frames = Vec::new();
        for b in &all {
            frames.extend(parser.push(&[*b]));
        }
        assert_eq!(frames.len(), 3);
        assert_eq!((frames[0].cmd, frames[0].rid, frames[0].is_response), (3, Some(17), false));
        assert_eq!(frames[0].body, vec![1, 2, 3]);
        assert_eq!((frames[1].cmd, frames[1].rid), (6, None));
        assert!(frames[2].is_response && frames[2].is_final);
        assert_eq!(frames[2].rid, Some(17));
        assert!(frames[2].body.is_empty());
        // all at once
        assert_eq!(FrameParser::new().push(&all).len(), 3);
        assert_eq!(parser.buffered(), 0);
    }
}
