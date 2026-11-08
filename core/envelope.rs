//! The message on the wire, protobuf-encoded with `codec`.
//!
//! Schema:
//!   `Envelope { bytes id=1; uint32 ttl_secs=2; uint32 seq=3; bool end=4; oneof body { Call=5; Reply=6; Event=7 } }`
//!   `Call  { string method=1; string reply_to=2; bytes payload=3 }`
//!   `Reply { uint32 status=1; string detail=2;   bytes payload=3 }`
//!   `Event { bytes payload=1 }`
//! The id is the nonce of the call it belongs to; a unary call omits it, since it is the call's own header.
use crate::codec::{self, Field, Reader, Writer};

/// A message's id: the nonce it was sealed under, `sender ‖ epoch ‖ counter`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub struct Id(pub [u8; 12]);

/// Why bytes are not an envelope: a codec error, no body, or an id that is not twelve bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub enum Error { Codec(codec::Error), Missing, Id }

impl From<codec::Error> for Error { fn from(e: codec::Error) -> Self { Self::Codec(e) } }

/// The oneof: a call, a reply to one, or an event on a topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // the schema above
pub enum Body<'a> {
    Call { method: &'a str, reply_to: &'a str, payload: &'a [u8] },
    Reply { status: u32, detail: &'a str, payload: &'a [u8] },
    Event { payload: &'a [u8] },
}

/// The header fields and the body, as the schema above lays them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // the schema above
pub struct Envelope<'a> { pub id: Option<Id>, pub ttl_secs: u32, pub seq: u32, pub end: bool, pub body: Body<'a> }

impl<'a> Envelope<'a> {
    /// Writes the envelope into `buf`; proto3 style, defaults omitted. `Full` when it does not fit.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, codec::Error> {
        let mut w = Writer::new(buf);
        if let Some(id) = self.id { w.bytes(1, &id.0)?; }
        put_u32(&mut w, 2, self.ttl_secs)?;
        put_u32(&mut w, 3, self.seq)?;
        put_u32(&mut w, 4, u32::from(self.end))?;
        match self.body {
            Body::Call { method, reply_to, payload } => w.message(5, |w| { put_bytes(w, 1, method.as_bytes())?; put_bytes(w, 2, reply_to.as_bytes())?; put_bytes(w, 3, payload) })?,
            Body::Reply { status, detail, payload } => w.message(6, |w| { put_u32(w, 1, status)?; put_bytes(w, 2, detail.as_bytes())?; put_bytes(w, 3, payload) })?,
            Body::Event { payload } => w.message(7, |w| put_bytes(w, 1, payload))?,
        }
        Ok(w.len())
    }

    /// Reads an envelope from `buf`, skipping unknown fields; the body must be present.
    pub fn decode(buf: &'a [u8]) -> Result<Self, Error> {
        let (mut id, mut ttl_secs, mut seq, mut end, mut body) = (None, 0, 0, false, None);
        fields(buf, |f| {
            match f {
                Field::Bytes(1, b) => id = Some(Id(b.try_into().map_err(|_| Error::Id)?)),
                Field::Varint(2, v) => ttl_secs = narrow(v)?,
                Field::Varint(3, v) => seq = narrow(v)?,
                Field::Varint(4, v) => end = v != 0,
                Field::Bytes(5, b) => body = Some(call(b)?),
                Field::Bytes(6, b) => body = Some(reply(b)?),
                Field::Bytes(7, b) => body = Some(event(b)?),
                _ => {} // unknown fields are skipped: compatibility within a version
            }
            Ok(())
        })?;
        Ok(Self { id, ttl_secs, seq, end, body: body.ok_or(Error::Missing)? })
    }
}

/// proto3 omits default values.
fn put_u32(w: &mut Writer, field: u32, v: u32) -> Result<(), codec::Error> { if v == 0 { Ok(()) } else { w.varint(field, u64::from(v)) } }
fn put_bytes(w: &mut Writer, field: u32, b: &[u8]) -> Result<(), codec::Error> { if b.is_empty() { Ok(()) } else { w.bytes(field, b) } }

fn fields<'a>(buf: &'a [u8], mut f: impl FnMut(Field<'a>) -> Result<(), Error>) -> Result<(), Error> {
    let mut r = Reader::new(buf);
    while let Some(field) = r.next()? { f(field)?; }
    Ok(())
}

fn narrow(v: u64) -> Result<u32, Error> { u32::try_from(v).map_err(|_| Error::Codec(codec::Error::Overflow)) }
fn utf8(b: &[u8]) -> Result<&str, Error> { core::str::from_utf8(b).map_err(|_| Error::Codec(codec::Error::Utf8)) }

fn call(b: &[u8]) -> Result<Body<'_>, Error> {
    let (mut method, mut reply_to, mut payload) = ("", "", &[][..]);
    fields(b, |f| { match f { Field::Bytes(1, x) => method = utf8(x)?, Field::Bytes(2, x) => reply_to = utf8(x)?, Field::Bytes(3, x) => payload = x, _ => {} } Ok(()) })?;
    Ok(Body::Call { method, reply_to, payload })
}

fn reply(b: &[u8]) -> Result<Body<'_>, Error> {
    let (mut status, mut detail, mut payload) = (0, "", &[][..]);
    fields(b, |f| { match f { Field::Varint(1, v) => status = narrow(v)?, Field::Bytes(2, x) => detail = utf8(x)?, Field::Bytes(3, x) => payload = x, _ => {} } Ok(()) })?;
    Ok(Body::Reply { status, detail, payload })
}

fn event(b: &[u8]) -> Result<Body<'_>, Error> {
    let mut payload = &[][..];
    fields(b, |f| { if let Field::Bytes(1, x) = f { payload = x } Ok(()) })?;
    Ok(Body::Event { payload })
}
