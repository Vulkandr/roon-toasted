// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Protocol details from arthursoares/roon-api-reverse-engineering (MIT), see NOTICE.

//! The primitive encodings of Roon's remoting protocol.
//!
//! Everything is built on two varints: "flexInt" (32-bit) and "flexLong"
//! (64-bit), both big-endian base-128 with the 0x80 continuation bit set on
//! every byte except the last. `143` is `81 0f`. A -1 length (the "null"
//! sentinel) is written as its unsigned 32-bit pattern, five bytes.
//!
//! On top of that: strings and byte arrays are `integer(len) + bytes`
//! (null = -1), Sooids (Roon's opaque ids) are `integer(len) + bytes`, GUIDs
//! are 16 raw bytes in .NET order, booleans one byte, doubles little-endian.

use std::fmt;

/// Reading ran past the end of the data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError;

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("read past the end of the buffer")
    }
}

impl std::error::Error for ReadError {}

pub type ReadResult<T> = Result<T, ReadError>;

pub fn write_flex_int(out: &mut Vec<u8>, value: u32) {
    let u = value;
    if u <= 0x7f {
        out.push(u as u8);
    } else if u <= 0x3fff {
        out.push(0x80 | (u >> 7) as u8);
        out.push((u & 0x7f) as u8);
    } else if u <= 0x1f_ffff {
        out.push(0x80 | (u >> 14) as u8);
        out.push(0x80 | ((u >> 7) & 0x7f) as u8);
        out.push((u & 0x7f) as u8);
    } else if u <= 0xfff_ffff {
        out.push(0x80 | (u >> 21) as u8);
        out.push(0x80 | ((u >> 14) & 0x7f) as u8);
        out.push(0x80 | ((u >> 7) & 0x7f) as u8);
        out.push((u & 0x7f) as u8);
    } else {
        out.push(0x80 | (u >> 28) as u8);
        out.push(0x80 | ((u >> 21) & 0x7f) as u8);
        out.push(0x80 | ((u >> 14) & 0x7f) as u8);
        out.push(0x80 | ((u >> 7) & 0x7f) as u8);
        out.push((u & 0x7f) as u8);
    }
}

pub fn write_flex_long(out: &mut Vec<u8>, value: u64) {
    if value == 0 {
        out.push(0);
        return;
    }
    let mut groups = Vec::with_capacity(10);
    let mut u = value;
    while u > 0 {
        groups.push((u & 0x7f) as u8);
        u >>= 7;
    }
    groups.reverse();
    let last = groups.len() - 1;
    for (i, g) in groups.into_iter().enumerate() {
        out.push(if i == last { g } else { 0x80 | g });
    }
}

/// Reads a flexInt at `pos`; returns the value and the position after it.
pub fn read_flex_int(buf: &[u8], mut pos: usize) -> ReadResult<(u32, usize)> {
    let mut num: u32 = 0;
    loop {
        let b = *buf.get(pos).ok_or(ReadError)?;
        pos += 1;
        num = (num << 7) | (b & 0x7f) as u32;
        if b & 0x80 == 0 {
            return Ok((num, pos));
        }
    }
}

/// Reads a flexLong at `pos`; returns the value and the position after it.
pub fn read_flex_long(buf: &[u8], mut pos: usize) -> ReadResult<(u64, usize)> {
    let mut num: u64 = 0;
    loop {
        let b = *buf.get(pos).ok_or(ReadError)?;
        pos += 1;
        num = (num << 7) | (b & 0x7f) as u64;
        if b & 0x80 == 0 {
            return Ok((num, pos));
        }
    }
}

/// A growable byte writer with the protocol's primitives. Every method
/// returns `&mut Self` so calls chain.
#[derive(Default, Debug, Clone)]
pub struct Writer {
    out: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.out.len()
    }

    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.out
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.out
    }

    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.out.extend_from_slice(b);
        self
    }

    pub fn byte(&mut self, b: u8) -> &mut Self {
        self.out.push(b);
        self
    }

    /// A signed 32-bit integer as a flexInt (-1 becomes 0xFFFFFFFF).
    pub fn integer(&mut self, v: i32) -> &mut Self {
        write_flex_int(&mut self.out, v as u32);
        self
    }

    pub fn flex_int(&mut self, v: u32) -> &mut Self {
        write_flex_int(&mut self.out, v);
        self
    }

    pub fn long(&mut self, v: u64) -> &mut Self {
        write_flex_long(&mut self.out, v);
        self
    }

    pub fn boolean(&mut self, v: bool) -> &mut Self {
        self.out.push(u8::from(v));
        self
    }

    pub fn string(&mut self, v: Option<&str>) -> &mut Self {
        match v {
            None => self.integer(-1),
            Some(s) => {
                self.integer(s.len() as i32);
                self.bytes(s.as_bytes())
            }
        }
    }

    pub fn sooid(&mut self, v: &[u8]) -> &mut Self {
        self.integer(v.len() as i32);
        self.bytes(v)
    }

    pub fn guid(&mut self, v: &[u8; 16]) -> &mut Self {
        self.bytes(v)
    }

    pub fn byte_array(&mut self, v: Option<&[u8]>) -> &mut Self {
        match v {
            None => self.integer(-1),
            Some(b) => {
                self.integer(b.len() as i32);
                self.bytes(b)
            }
        }
    }
}

/// A cursor over received bytes with the protocol's primitives.
pub struct Reader<'a> {
    buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    pub fn flex_int(&mut self) -> ReadResult<u32> {
        let (v, pos) = read_flex_int(self.buf, self.pos)?;
        self.pos = pos;
        Ok(v)
    }

    /// A flexInt read as a signed 32-bit number (0xFFFFFFFF is -1).
    pub fn integer(&mut self) -> ReadResult<i32> {
        Ok(self.flex_int()? as i32)
    }

    pub fn flex_long(&mut self) -> ReadResult<u64> {
        let (v, pos) = read_flex_long(self.buf, self.pos)?;
        self.pos = pos;
        Ok(v)
    }

    pub fn long(&mut self) -> ReadResult<u64> {
        self.flex_long()
    }

    pub fn byte(&mut self) -> ReadResult<u8> {
        let b = *self.buf.get(self.pos).ok_or(ReadError)?;
        self.pos += 1;
        Ok(b)
    }

    pub fn boolean(&mut self) -> ReadResult<bool> {
        Ok(self.byte()? != 0)
    }

    pub fn bytes(&mut self, n: usize) -> ReadResult<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(ReadError);
        }
        let b = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(b)
    }

    pub fn guid(&mut self) -> ReadResult<[u8; 16]> {
        let b = self.bytes(16)?;
        let mut out = [0u8; 16];
        out.copy_from_slice(b);
        Ok(out)
    }

    pub fn double(&mut self) -> ReadResult<f64> {
        let b = self.bytes(8)?;
        Ok(f64::from_le_bytes(b.try_into().unwrap()))
    }

    pub fn float(&mut self) -> ReadResult<f32> {
        let b = self.bytes(4)?;
        Ok(f32::from_le_bytes(b.try_into().unwrap()))
    }

    /// A length-prefixed value: -1 means null.
    fn length_prefixed(&mut self) -> ReadResult<Option<&'a [u8]>> {
        let len = self.integer()?;
        if len < 0 {
            return Ok(None);
        }
        Ok(Some(self.bytes(len as usize)?))
    }

    pub fn sooid(&mut self) -> ReadResult<Vec<u8>> {
        Ok(self.length_prefixed()?.unwrap_or(&[]).to_vec())
    }

    pub fn string(&mut self) -> ReadResult<Option<String>> {
        Ok(self
            .length_prefixed()?
            .map(|b| String::from_utf8_lossy(b).into_owned()))
    }

    pub fn byte_array(&mut self) -> ReadResult<Option<Vec<u8>>> {
        Ok(self.length_prefixed()?.map(|b| b.to_vec()))
    }

    pub fn optional_integer(&mut self) -> ReadResult<Option<i32>> {
        Ok(if self.boolean()? { Some(self.integer()?) } else { None })
    }

    pub fn optional_long(&mut self) -> ReadResult<Option<u64>> {
        Ok(if self.boolean()? { Some(self.long()?) } else { None })
    }

    pub fn optional_boolean(&mut self) -> ReadResult<Option<bool>> {
        Ok(match self.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        })
    }

    pub fn optional_guid(&mut self) -> ReadResult<Option<[u8; 16]>> {
        Ok(if self.boolean()? { Some(self.guid()?) } else { None })
    }

    pub fn optional_sooid(&mut self) -> ReadResult<Option<Vec<u8>>> {
        Ok(self.length_prefixed()?.map(|b| b.to_vec()))
    }

    pub fn optional_double(&mut self) -> ReadResult<Option<f64>> {
        Ok(if self.boolean()? { Some(self.double()?) } else { None })
    }

    pub fn optional_float(&mut self) -> ReadResult<Option<f32>> {
        Ok(if self.boolean()? { Some(self.float()?) } else { None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn flex_ints_match_the_wire() {
        for (value, expected) in [
            (0u32, "00"),
            (127, "7f"),
            (128, "8100"),
            (143, "810f"),
            (231, "8167"),
            (16383, "ff7f"),
            (16384, "818000"),
            (3969842, "81f2a632"),
            (u32::MAX, "8fffffff7f"),
        ] {
            let mut out = Vec::new();
            write_flex_int(&mut out, value);
            assert_eq!(hex(&out), expected, "write {value}");
            let (back, pos) = read_flex_int(&out, 0).unwrap();
            assert_eq!(back, value);
            assert_eq!(pos, out.len());
        }
    }

    #[test]
    fn flex_longs_round_trip() {
        for value in [0u64, 1, 72, 2417256, 2555352, u64::MAX] {
            let mut out = Vec::new();
            write_flex_long(&mut out, value);
            assert_eq!(read_flex_long(&out, 0).unwrap().0, value);
        }
        assert_eq!(read_flex_long(&[0x81, 0x93, 0xc4, 0x5d], 0).unwrap().0, 2417245);
        assert_eq!(read_flex_long(&[0x81, 0x90, 0xfe, 0x0f], 0).unwrap().0, 2375439);
    }

    #[test]
    fn primitives() {
        let mut w = Writer::new();
        w.string(Some("en")).string(None).boolean(true).sooid(&[0x3f, 0x01]);
        assert_eq!(hex(w.as_bytes()), "02656e8fffffff7f01023f01");
        let mut r = Reader::new(w.as_bytes());
        assert_eq!(r.string().unwrap().as_deref(), Some("en"));
        assert_eq!(r.string().unwrap(), None);
        assert!(r.boolean().unwrap());
        assert_eq!(r.sooid().unwrap(), vec![0x3f, 0x01]);
        assert_eq!(r.remaining(), 0);
        assert_eq!(r.byte(), Err(ReadError));
    }
}
