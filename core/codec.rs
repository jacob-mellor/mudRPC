//! Protobuf wire format (protobuf.dev/programming-guides/encoding): varints, fixed widths, length-delimited fields.
//!
//! No heap. Every index is checked by construction, so decoding and encoding are total: a value or an `Err`, never a trap.

/// What stops a decode or an encode.
///
/// `Truncated`: the bytes ran out; `Overflow`: a varint past 64 bits, or past a 32-bit field; `Tag`: field 0 or a wire type
/// this codec has no use for; `Full`: the output buffer is; `Utf8`: a string that is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub enum Error { Truncated, Overflow, Tag, Full, Utf8 }

/// One field as read: its number, then its value by wire type (0 varint, 2 bytes, 5 fixed32, 1 fixed64).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub enum Field<'a> { Varint(u32, u64), Bytes(u32, &'a [u8]), Fixed32(u32, u32), Fixed64(u32, u64) }

/// Bytes a varint occupies: one per 7 bits, at least one.
const fn vlen(v: u64) -> usize { (70 - (v | 1).leading_zeros() as usize) / 7 }

/// One bare varint from the front of a slice; the rest follows. For packed repeated fields.
pub fn varint(b: &[u8]) -> Result<(u64, &[u8]), Error> {
    let mut r = Reader::new(b);
    let v = r.raw()?;
    Ok((v, b.get(r.pos..).unwrap_or(&[])))
}

/// Encodes fields into a caller's buffer, front to back.
#[allow(clippy::len_without_is_empty)] // `len` is the bytes written so far
pub struct Writer<'a> { buf: &'a mut [u8], len: usize }

impl<'a> Writer<'a> {
    /// Writes from the front of `buf`.
    pub const fn new(buf: &'a mut [u8]) -> Self { Self { buf, len: 0 } }
    /// Bytes written so far.
    #[must_use] pub const fn len(&self) -> usize { self.len }

    /// Wire type 0: `Full` when the buffer is.
    pub fn varint(&mut self, field: u32, v: u64) -> Result<(), Error> { self.tag(field, 0)?; self.raw(v) }
    /// Wire type 5.
    pub fn fixed32(&mut self, field: u32, v: u32) -> Result<(), Error> { self.tag(field, 5)?; self.put(&v.to_le_bytes()) }
    /// Wire type 1.
    pub fn fixed64(&mut self, field: u32, v: u64) -> Result<(), Error> { self.tag(field, 1)?; self.put(&v.to_le_bytes()) }
    /// Wire type 2: a string, bytes or an already-encoded message.
    pub fn bytes(&mut self, field: u32, b: &[u8]) -> Result<(), Error> { self.tag(field, 2)?; self.raw(b.len() as u64)?; self.put(b) }

    /// Nested message: write the fields after a one-byte length, then widen the length if it needs more bytes.
    pub fn message(&mut self, field: u32, f: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        self.tag(field, 2)?;
        let at = self.len;
        self.put(&[0])?;
        f(self)?;
        let n = (self.len - at - 1) as u64;
        let extra = vlen(n) - 1;
        let end = self.len.checked_add(extra).filter(|&e| e <= self.buf.len()).ok_or(Error::Full)?;
        for i in (at + 1..self.len).rev() { if let (Some(b), Some(d)) = (self.buf.get(i).copied(), self.buf.get_mut(i + extra)) { *d = b; } }
        self.len = at;
        self.raw(n)?;
        self.len = end;
        Ok(())
    }

    fn tag(&mut self, field: u32, wire_type: u64) -> Result<(), Error> { self.raw(u64::from(field) << 3 | wire_type) }

    fn put(&mut self, b: &[u8]) -> Result<(), Error> {
        let end = self.len.checked_add(b.len()).ok_or(Error::Full)?;
        self.buf.get_mut(self.len..end).ok_or(Error::Full)?.copy_from_slice(b);
        self.len = end;
        Ok(())
    }

    fn raw(&mut self, mut v: u64) -> Result<(), Error> {
        loop {
            let byte = self.buf.get_mut(self.len).ok_or(Error::Full)?;
            self.len += 1;
            if v < 0x80 { *byte = v as u8; return Ok(()); }
            *byte = v as u8 | 0x80;
            v >>= 7;
        }
    }
}

/// Walks the fields of an encoded message, front to back.
pub struct Reader<'a> { buf: &'a [u8], pos: usize }

impl<'a> Reader<'a> {
    /// Reads from the front of `buf`.
    #[must_use] pub const fn new(buf: &'a [u8]) -> Self { Self { buf, pos: 0 } }

    /// Named like an iterator on purpose; it returns `Err` where an iterator could not.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Field<'a>>, Error> {
        if self.pos >= self.buf.len() { return Ok(None); }
        let tag = self.raw()?;
        let field = u32::try_from(tag >> 3).ok().filter(|&f| f != 0).ok_or(Error::Tag)?;
        Ok(Some(match tag & 7 {
            0 => Field::Varint(field, self.raw()?),
            1 => Field::Fixed64(field, le(self.take(8)?)),
            2 => { let n = self.raw()?; Field::Bytes(field, self.take(n)?) }
            5 => Field::Fixed32(field, le(self.take(4)?) as u32),
            _ => return Err(Error::Tag), // groups (3, 4) and the undefined types
        }))
    }

    fn take(&mut self, n: u64) -> Result<&'a [u8], Error> {
        let end = (self.pos as u64).checked_add(n).filter(|&e| e <= self.buf.len() as u64).ok_or(Error::Truncated)? as usize;
        let s = self.buf.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn raw(&mut self) -> Result<u64, Error> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.buf.get(self.pos).ok_or(Error::Truncated)?;
            self.pos += 1;
            v |= u64::from(b & 0x7F) << shift;
            if b < 0x80 { return Ok(v); }
        }
        Err(Error::Overflow)
    }
}

/// Little-endian bytes as a number, the first eight at most: the one reader core and auth share.
#[must_use]
pub fn le(b: &[u8]) -> u64 { b.iter().take(8).rev().fold(0, |a, &x| a << 8 | u64::from(x)) }
