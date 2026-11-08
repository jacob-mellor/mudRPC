//! The wire.
//!
//! Contract assumed here:
//! ```text
//!   codec::Writer::new(&mut [u8]) · .varint(field, u64) · .bytes(field, &[u8]) · .len()          -> Result<(), codec::Error>
//!   codec::Reader::new(&[u8]) · .next() -> Result<Option<codec::Field>, codec::Error>
//!   codec::Field { Varint(u32, u64), Bytes(u32, &[u8]), Fixed32(u32, u32), Fixed64(u32, u64) }
//!   codec::Error { Truncated, Overflow, Tag, Full, Utf8 }
//!   envelope::{Id([u8;12]), Body::{Call{method,reply_to,payload}, Reply{status,detail,payload}, Event{payload}}}
//!   envelope::Envelope { id: Option<Id>, ttl_secs, seq, end, body } · .encode(&mut [u8]) -> Result<usize, codec::Error>   (id absent on a unary call)
//!   envelope::Envelope::decode(&[u8]) -> Result<Envelope, envelope::Error>;  Error { Codec(codec::Error), Missing, Id }   (bad UTF-8 is Codec(Utf8))
//!   seal::{KEY, NONCE, TAG} · block(key, counter, nonce) -> [u8;64] · poly1305(key, msg) -> [u8;16]
//!   seal::seal(key, nonce, ad, plain, out) -> Result<usize, seal::Error>   writes nonce ‖ ciphertext ‖ tag
//!   seal::open(key, ad, wire, out) -> Result<usize, seal::Error>;           Error { Short, Tag, Full }
//!   seal::check(key) -> [u8;12] · seal::Sealer { seal(nonce, ad, plain, n, out), open(ad, wire, out) -> Opened { len, nonce, slot } }
//!   seal::Key::new(key) · .stage(next) · .rotate() · .retire()   slots Current, Previous, Next; header = nonce XOR mask(slot key, the 12 bytes after it)
//! ```
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
    clippy::many_single_char_names, // test scripts: `a` and `b` are the peers, `w` a wire, `e` an envelope
)]

use mudrpc_core::codec::{self, Field, Reader, Writer};
use mudrpc_core::envelope::{self, Body, Envelope, Id};
use mudrpc_core::seal::{self, Key, Opened, Sealer, Slot};

mod support;
use support::{hex, Rng};

const FUZZ: usize = 1_000_000;

fn key32(s: &str) -> [u8; 32] { hex(s).try_into().unwrap() }

// ───────────────────────── codec ─────────────────────────

#[test]
fn varint_round_trip_and_known_bytes() {
    for v in [0u64, 1, 127, 128, 300, u64::from(u32::MAX), u64::MAX] {
        let mut buf = [0u8; 16];
        let mut w = Writer::new(&mut buf);
        w.varint(1, v).unwrap();
        let n = w.len();
        let mut r = Reader::new(&buf[..n]);
        assert_eq!(r.next().unwrap(), Some(Field::Varint(1, v)));
        assert_eq!(r.next().unwrap(), None);
    }
    let mut buf = [0u8; 4];
    assert_eq!(enc(&mut buf, |w| w.varint(1, 300).unwrap()), &[0x08, 0xAC, 0x02]);
}

#[test]
fn bytes_round_trip_including_empty() {
    for b in [&b""[..], b"x", b"hello mesh"] {
        let mut buf = [0u8; 32];
        let mut w = Writer::new(&mut buf);
        w.bytes(2, b).unwrap();
        let n = w.len();
        let mut r = Reader::new(&buf[..n]);
        assert_eq!(r.next().unwrap(), Some(Field::Bytes(2, b)));
        assert_eq!(r.next().unwrap(), None);
    }
}

#[test]
fn fixed_fields_and_bare_varints_round_trip() {
    let mut buf = [0u8; 32];
    let mut w = Writer::new(&mut buf);
    w.fixed32(1, 0x0403_0201).unwrap();
    w.fixed64(2, 0x0807_0605_0403_0201).unwrap();
    let n = w.len();
    assert_eq!(&buf[..n], &[0x0D, 1, 2, 3, 4, 0x11, 1, 2, 3, 4, 5, 6, 7, 8]);
    let mut r = Reader::new(&buf[..n]);
    assert_eq!(r.next().unwrap(), Some(Field::Fixed32(1, 0x0403_0201)));
    assert_eq!(r.next().unwrap(), Some(Field::Fixed64(2, 0x0807_0605_0403_0201)));
    assert_eq!(codec::varint(&[0xAC, 0x02, 0x07]).unwrap(), (300, &[0x07][..]));
    assert_eq!(codec::varint(&[0x80]), Err(codec::Error::Truncated));
    let mut small = [0u8; 4];
    assert_eq!(Writer::new(&mut small).fixed32(1, 1), Err(codec::Error::Full));
}

#[test]
fn writer_refuses_when_full() {
    let mut buf = [0u8; 1];
    assert_eq!(Writer::new(&mut buf).varint(1, 300), Err(codec::Error::Full));
    let mut buf = [0u8; 3];
    assert_eq!(Writer::new(&mut buf).bytes(1, b"abc"), Err(codec::Error::Full));
    let mut buf = [0u8; 0];
    assert_eq!(Writer::new(&mut buf).varint(1, 0), Err(codec::Error::Full));
}

#[test]
fn reader_rejects_malformed_without_panic() {
    let cases: &[(&[u8], codec::Error)] = &[
        (&[0x08, 0x80], codec::Error::Truncated),                           // varint cut short
        (&[0x08], codec::Error::Truncated),                                 // tag with no value
        (&[0x12, 0x05, 0x61], codec::Error::Truncated),                     // length past buffer
        (&[0x12, 0x80], codec::Error::Truncated),                           // length varint cut short
        (&[0x08, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01], codec::Error::Overflow), // 11-byte varint
        (&[0x00], codec::Error::Tag),                                       // field 0
        (&[0x0B], codec::Error::Tag),                                       // wire type 3 (group)
        (&[0x0C], codec::Error::Tag),                                       // wire type 4
        (&[0x0E], codec::Error::Tag),                                       // wire type 6
        (&[0x0F], codec::Error::Tag),                                       // wire type 7
        (&[0x80], codec::Error::Truncated),                                 // tag varint cut short
    ];
    for (bytes, err) in cases {
        assert_eq!(Reader::new(bytes).next(), Err(*err), "{bytes:02x?}");
    }
}

#[test]
fn reader_returns_fixed_fields_so_callers_can_skip_them() {
    let mut r = Reader::new(&[0x0D, 1, 2, 3, 4, 0x11, 1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(r.next().unwrap(), Some(Field::Fixed32(1, 0x0403_0201)));
    assert_eq!(r.next().unwrap(), Some(Field::Fixed64(2, 0x0807_0605_0403_0201)));
    assert_eq!(r.next().unwrap(), None);
    assert_eq!(Reader::new(&[]).next().unwrap(), None);
}

#[test]
fn reader_never_panics_on_random_input() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut buf = [0u8; 64];
    for _ in 0..FUZZ {
        let n = rng.below(buf.len() + 1);
        rng.fill(&mut buf[..n]);
        let mut r = Reader::new(&buf[..n]);
        for _ in 0..=n {
            if !matches!(r.next(), Ok(Some(_))) { break; }
        }
    }
}

// ───────────────────────── envelope ─────────────────────────

const ID: Id = Id([7u8; 12]);

fn bodies() -> Vec<Body<'static>> {
    vec![
        Body::Call { method: "SetWaypoint", reply_to: "drone-7", payload: b"\x08\x01" },
        Body::Call { method: "", reply_to: "", payload: b"" },
        Body::Reply { status: 5, detail: "not found", payload: b"\x01" },
        Body::Reply { status: 0, detail: "", payload: b"" },
        Body::Event { payload: b"position" },
        Body::Event { payload: b"" },
    ]
}

#[test]
fn envelope_round_trips_every_body_with_fields_set_and_defaulted() {
    for (body, id) in bodies().into_iter().flat_map(|b| [(b, None), (b, Some(ID))]) {
        for (ttl, seq, end) in [(0u32, 0u32, false), (3600, 17, true), (u32::MAX, u32::MAX, true)] {
            let e = Envelope { id, ttl_secs: ttl, seq, end, body };
            let mut buf = [0u8; 128];
            let n = e.encode(&mut buf).unwrap();
            assert_eq!(Envelope::decode(&buf[..n]).unwrap(), e);
        }
    }
}

#[test]
fn unknown_fields_are_skipped_at_both_levels() {
    let e = Envelope { id: Some(ID), ttl_secs: 9, seq: 2, end: true, body: Body::Event { payload: b"p" } };
    let mut buf = [0u8; 128];
    let n = e.encode(&mut buf).unwrap();
    let mut w = Writer::new(&mut buf[n..]);
    w.varint(15, 7).unwrap();
    w.bytes(16, b"future").unwrap();
    let m = n + w.len();
    buf[m..m + 5].copy_from_slice(&[0x8D, 0x01, 1, 2, 3]); // field 17, fixed32: tag 0x8d 0x01 then... 
    buf[m + 5] = 4;
    let m = m + 6;
    assert_eq!(Envelope::decode(&buf[..m]).unwrap(), e);

    let mut inner = [0u8; 64];
    let mut w = Writer::new(&mut inner);
    w.bytes(1, b"m").unwrap();
    w.bytes(2, b"inbox").unwrap();
    w.bytes(3, b"p").unwrap();
    w.varint(9, 1).unwrap(); // unknown inside Call
    let il = w.len();
    let mut outer = [0u8; 128];
    let mut w = Writer::new(&mut outer);
    w.bytes(1, &ID.0).unwrap();
    w.bytes(5, &inner[..il]).unwrap();
    let ol = w.len();
    let d = Envelope::decode(&outer[..ol]).unwrap();
    assert_eq!(d.id, Some(ID));
    assert_eq!(d.body, Body::Call { method: "m", reply_to: "inbox", payload: b"p" });
    assert_eq!((d.ttl_secs, d.seq, d.end), (0, 0, false));
}

fn enc(buf: &mut [u8], f: impl FnOnce(&mut Writer)) -> &[u8] {
    let mut w = Writer::new(buf);
    f(&mut w);
    let n = w.len();
    &buf[..n]
}

#[test]
fn envelope_rejects_structural_errors() {
    let mut buf = [0u8; 128];
    let mut inner = [0u8; 16];
    let no_id = enc(&mut buf, |w| w.bytes(7, b"").unwrap());
    assert_eq!(Envelope::decode(no_id).map(|e| e.id), Ok(None), "an absent id is the header's business, not an error");

    let no_body = enc(&mut buf, |w| w.bytes(1, &ID.0).unwrap());
    assert_eq!(Envelope::decode(no_body), Err(envelope::Error::Missing));

    let short_id = enc(&mut buf, |w| { w.bytes(1, &ID.0[..11]).unwrap(); w.bytes(7, b"").unwrap() });
    assert_eq!(Envelope::decode(short_id), Err(envelope::Error::Id));

    let bad_method = enc(&mut inner, |w| w.bytes(1, &[0xFF, 0xFE]).unwrap());
    let bad_utf8 = enc(&mut buf, |w| { w.bytes(1, &ID.0).unwrap(); w.bytes(5, bad_method).unwrap() });
    assert_eq!(Envelope::decode(bad_utf8), Err(envelope::Error::Codec(codec::Error::Utf8)), "UTF-8 is the codec's business");

    let big_ttl = enc(&mut buf, |w| { w.bytes(1, &ID.0).unwrap(); w.varint(2, u64::from(u32::MAX) + 1).unwrap(); w.bytes(7, b"").unwrap() });
    assert_eq!(Envelope::decode(big_ttl), Err(envelope::Error::Codec(codec::Error::Overflow)));

    assert_eq!(Envelope::decode(&[]), Err(envelope::Error::Missing));
    assert_eq!(Envelope::decode(&[0x08]), Err(envelope::Error::Codec(codec::Error::Truncated)));
}

#[test]
fn envelope_encode_refuses_small_buffer() {
    let e = Envelope { id: Some(ID), ttl_secs: 1, seq: 0, end: false, body: Body::Event { payload: &[0u8; 100] } };
    for n in [0usize, 1, 17, 19, 50, 118] {
        let mut buf = vec![0u8; n];
        assert_eq!(e.encode(&mut buf), Err(codec::Error::Full), "buffer {n}");
    }
}

#[test]
fn envelope_decode_never_panics_on_random_or_mutated_input() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut buf = [0u8; 200];
    for _ in 0..FUZZ {
        let n = rng.below(buf.len() + 1);
        rng.fill(&mut buf[..n]);
        let _ = Envelope::decode(&buf[..n]);
    }
    let e = Envelope { id: Some(ID), ttl_secs: 60, seq: 3, end: false, body: Body::Call { method: "Arm", reply_to: "gcs-1", payload: b"\x08\x01\x12\x03abc" } };
    let n = e.encode(&mut buf).unwrap();
    let valid = buf[..n].to_vec();
    for _ in 0..FUZZ {
        let mut m = valid.clone();
        for _ in 0..=rng.below(3) {
            let bit = rng.below(m.len() * 8);
            m[bit / 8] ^= 1 << (bit % 8);
        }
        let _ = Envelope::decode(&m);
    }
}

// ───────────────────────── seal ─────────────────────────

const RFC_KEY: &str = "00:01:02:03:04:05:06:07:08:09:0a:0b:0c:0d:0e:0f:10:11:12:13:14:15:16:17:18:19:1a:1b:1c:1d:1e:1f";

#[test]
fn rfc8439_chacha20_block() {
    let nonce: [u8; 12] = hex("00:00:00:09:00:00:00:4a:00:00:00:00").try_into().unwrap();
    let out = seal::block(&key32(RFC_KEY), 1, &nonce);
    let want = hex("10 f1 e7 e4 d1 3b 59 15 50 0f dd 1f a3 20 71 c4 c7 d1 f4 c7 33 c0 68 03 04 22 aa 9a c3 d4 6c 4e \
                    d2 82 64 46 07 9f aa 09 14 c2 d7 05 d9 8b 02 a2 b5 12 9c d1 de 16 4e b9 cb d0 83 e8 a2 50 3c 4e");
    assert_eq!(&out[..], &want[..]);
}

#[test]
fn rfc8439_poly1305() {
    let key = key32("85:d6:be:78:57:55:6d:33:7f:44:52:fe:42:d5:06:a8:01:03:80:8a:fb:0d:b2:fd:4a:bf:f6:af:41:49:f5:1b");
    let tag = seal::poly1305(&key, b"Cryptographic Forum Research Group");
    assert_eq!(&tag[..], &hex("a8:06:1d:c1:30:51:36:c6:c2:2b:8b:af:0c:01:27:a9")[..]);
}

const RFC_PLAIN: &[u8] = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
const RFC_AEAD_KEY: &str = "80 81 82 83 84 85 86 87 88 89 8a 8b 8c 8d 8e 8f 90 91 92 93 94 95 96 97 98 99 9a 9b 9c 9d 9e 9f";
const RFC_AAD: &str = "50 51 52 53 c0 c1 c2 c3 c4 c5 c6 c7";
const RFC_NONCE: &str = "07 00 00 00 40 41 42 43 44 45 46 47";
const RFC_CT: &str = "d3 1a 8d 34 64 8e 60 db 7b 86 af bc 53 ef 7e c2 a4 ad ed 51 29 6e 08 fe a9 e2 b5 a7 36 ee 62 d6 \
                      3d be a4 5e 8c a9 67 12 82 fa fb 69 da 92 72 8b 1a 71 de 0a 9e 06 0b 29 05 d6 a5 b6 7e cd 3b 36 \
                      92 dd bd 7f 2d 77 8b 8c 98 03 ae e3 28 09 1b 58 fa b3 24 e4 fa d6 75 94 55 85 80 8b 48 31 d7 bc \
                      3f f4 de f0 8e 4b 7a 9d e5 76 d2 65 86 ce c6 4b 61 16";
const RFC_TAG: &str = "1a:e1:0b:59:4f:09:e2:6a:7e:90:2e:cb:d0:60:06:91";

#[test]
fn rfc8439_aead_seal_and_open() {
    let key = key32(RFC_AEAD_KEY);
    let nonce: [u8; 12] = hex(RFC_NONCE).try_into().unwrap();
    let aad = hex(RFC_AAD);
    let mut wire = [0u8; 256];
    let n = seal::seal(&key, &nonce, &aad, RFC_PLAIN, &mut wire).unwrap();
    assert_eq!(n, seal::NONCE + RFC_PLAIN.len() + seal::TAG);
    assert_eq!(&wire[..12], &nonce[..]);
    assert_eq!(&wire[12..12 + 114], &hex(RFC_CT)[..]);
    assert_eq!(&wire[126..142], &hex(RFC_TAG)[..]);
    let mut plain = [0u8; 256];
    let m = seal::open(&key, &aad, &wire[..n], &mut plain).unwrap();
    assert_eq!(&plain[..m], RFC_PLAIN);
}

#[test]
fn open_fails_on_wrong_ad_wrong_key_or_any_flipped_bit() {
    let key = [1u8; 32];
    let nonce = [2u8; 12];
    let mut wire = [0u8; 64];
    let n = seal::seal(&key, &nonce, b"nav", b"waypoint", &mut wire).unwrap();
    let mut out = [0u8; 64];
    assert_eq!(seal::open(&key, b"nav2", &wire[..n], &mut out), Err(seal::Error::Tag));
    assert_eq!(seal::open(&[3u8; 32], b"nav", &wire[..n], &mut out), Err(seal::Error::Tag));
    for bit in 0..n * 8 {
        let mut w = wire;
        w[bit / 8] ^= 1 << (bit % 8);
        assert_eq!(seal::open(&key, b"nav", &w[..n], &mut out), Err(seal::Error::Tag), "bit {bit}");
    }
    let hits = (0..=255u8).filter(|&k| seal::open(&[k; 32], b"nav", &wire[..n], &mut out).is_ok()).count();
    assert_eq!(hits, 1, "a captured wire is an offline check of a candidate key: one of 256 opens it");
    assert_eq!(seal::open(&key, b"nav", &wire[..n], &mut out), Ok(8));
    assert_eq!(&out[..8], b"waypoint");
}

#[test]
fn seal_edges_short_wire_small_buffers_empty_plaintext_random_bytes() {
    let key = [9u8; 32];
    let mut out = [0u8; 64];
    for n in 0..seal::NONCE + seal::TAG {
        assert_eq!(seal::open(&key, b"", &[0u8; 64][..n], &mut out), Err(seal::Error::Short));
    }
    let mut small = [0u8; 27];
    assert_eq!(seal::seal(&key, &[0u8; 12], b"", b"", &mut small), Err(seal::Error::Full));
    let mut wire = [0u8; 64];
    assert_eq!(seal::seal(&key, &[0u8; 12], b"t", b"", &mut wire), Ok(28));
    assert_eq!(seal::open(&key, b"t", &wire[..28], &mut out), Ok(0));
    let mut tiny = [0u8; 0];
    assert_eq!(seal::open(&key, b"t", &wire[..28], &mut tiny), Ok(0));
    let n = seal::seal(&key, &[0u8; 12], b"t", b"abc", &mut wire).unwrap();
    let mut two = [0u8; 2];
    assert_eq!(seal::open(&key, b"t", &wire[..n], &mut two), Err(seal::Error::Full));

    let mut rng = Rng(0xCAFE_F00D_DEAD_BEEF);
    let mut junk = [0u8; 96];
    for _ in 0..FUZZ / 10 {
        let n = rng.below(junk.len() + 1);
        rng.fill(&mut junk[..n]);
        assert!(seal::open(&key, b"t", &junk[..n], &mut out).is_err());
    }
}

#[test]
fn envelope_sealed_to_a_topic_opens_only_on_that_topic() {
    let key = [4u8; 32];
    let e = Envelope { id: Some(ID), ttl_secs: 30, seq: 0, end: true, body: Body::Call { method: "Arm", reply_to: "gcs-1", payload: b"\x08\x01" } };
    let mut plain = [0u8; 128];
    let n = e.encode(&mut plain).unwrap();
    let nonce = [5u8; 12];
    let mut wire = [0u8; 160];
    let w = seal::seal(&key, &nonce, b"nav.Flight", &plain[..n], &mut wire).unwrap();
    let mut out = [0u8; 128];
    assert_eq!(seal::open(&key, b"nav.Other", &wire[..w], &mut out), Err(seal::Error::Tag));
    let m = seal::open(&key, b"nav.Flight", &wire[..w], &mut out).unwrap();
    assert_eq!(Envelope::decode(&out[..m]).unwrap(), e);
    assert_eq!(w, n + seal::NONCE + seal::TAG);
}

// ───────────────────────── earlier lessons ─────────────────────────

#[test]
fn open_refuses_an_oversize_wire_before_any_tag_work() {
    let key = [1u8; 32];
    let mut wire = [0u8; 160];
    let n = seal::seal(&key, &[2u8; 12], b"t", &[7u8; 100], &mut wire).unwrap();
    wire[n - 1] ^= 1; // a bad tag as well: size must win, because the size check is free and the tag is not
    assert_eq!(seal::open(&key, b"t", &wire[..n], &mut [0u8; 50]), Err(seal::Error::Full));
    wire[n - 1] ^= 1;
    assert_eq!(seal::open(&key, b"t", &wire[..n], &mut [0u8; 100]), Ok(100));
}

#[test]
fn ok_reply_with_empty_payload_is_sixteen_bytes_and_a_unary_call_carries_no_id() {
    let e = Envelope { id: Some(ID), ttl_secs: 0, seq: 0, end: false, body: Body::Reply { status: 0, detail: "", payload: b"" } };
    let mut buf = [0u8; 64];
    assert_eq!(e.encode(&mut buf).unwrap(), 16); // id: 1+1+12 · reply: 1+1+0
    let c = Envelope { id: None, ttl_secs: 0, seq: 0, end: true, body: Body::Call { method: "", reply_to: "", payload: b"" } };
    assert_eq!(c.encode(&mut buf).unwrap(), 4); // end: 1+1 · call: 1+1; the id is the header nonce
    assert_eq!(Envelope::decode(&buf[..4]).unwrap(), c);
}

#[test]
fn the_key_sealer_masks_the_header_rotates_through_three_slots_and_reports_which_opened() {
    let (k1, k2, k3) = ([1u8; 32], [2u8; 32], [3u8; 32]);
    let mut a = Key::new(k1);
    let nonce = [0x11u8, 0x22, 0x33, 0x44, 0, 0, 0, 7, 0, 0, 0, 9];
    let mut w = [0u8; 64];
    let n = a.seal(&nonce, b"nav", &mut b"waypoint".to_vec(), 8, &mut w).unwrap();
    assert_eq!(n, 12 + 8 + 16);
    let (mut room, mut w0) = (*b"waypoint and room", [0u8; 64]);
    assert_eq!(a.seal(&nonce, b"nav", &mut room, 8, &mut w0), Ok(n), "a trailer of 0: the room after the message is not sealed");
    assert_eq!((w0, room), (w, *b"waypoint and room"), "nor touched");
    assert_eq!(a.seal(&nonce, b"nav", &mut room, 18, &mut w0), Err(seal::Error::Full), "n past the buffer");
    assert_eq!(a.seal(&nonce, b"nav", &mut room, usize::MAX, &mut w0), Err(seal::Error::Full));
    assert_ne!(&w[..12], &nonce[..], "the header is masked");
    assert_ne!(w[..12].iter().zip(seal::check(&k1)).map(|(h, m)| h ^ m).collect::<Vec<_>>(), nonce.to_vec(), "and not with a per-key constant");
    let mut out = [0u8; 64];
    let o = a.open(b"nav", &w[..n], &mut out).unwrap();
    assert_eq!((o.len, o.nonce, o.slot), (8, nonce, Slot::Current));
    assert_eq!(&out[..8], b"waypoint");
    assert_eq!(a.open(b"other", &w[..n], &mut out), Err(seal::Error::Tag));
    let mut b = Key::new(k2);
    assert_eq!(b.open(b"nav", &w[..n], &mut out), Err(seal::Error::Tag), "a key never held");
    b.stage(k1);
    assert_eq!(b.open(b"nav", &w[..n], &mut out).map(|o| o.slot), Ok(Slot::Next), "a unit that has not rotated yet opens the newcomer's wire");
    a.stage(k2);
    a.rotate();
    assert_eq!(a.open(b"nav", &w[..n], &mut out).map(|o| o.slot), Ok(Slot::Previous), "after rotation the old key is previous");
    let mut w2 = [0u8; 64];
    let n2 = a.seal(&nonce, b"nav", &mut b"again".to_vec(), 5, &mut w2).unwrap();
    assert_ne!(&w2[..12], &w[..12], "a new key, a new mask");
    assert_eq!(Key::new(k2).open(b"nav", &w2[..n2], &mut out).map(|o| o.slot), Ok(Slot::Current));
    a.retire();
    assert_eq!(a.open(b"nav", &w[..n], &mut out), Err(seal::Error::Tag), "retired: the old key opens nothing");
    a.stage(k3);
    a.rotate();
    assert_eq!(a.open(b"nav", &w2[..n2], &mut out).map(|o| o.slot), Ok(Slot::Previous));
    assert_eq!(a.open(b"nav", &w[..27], &mut out), Err(seal::Error::Short));
    assert_eq!(a.open(b"nav", &w2[..n2], &mut [0u8; 2]), Err(seal::Error::Full), "size before any tag work, under every slot");
    let _: Opened = o;
}

#[test]
fn a_sealer_keyed_by_topic_needs_nothing_from_core() {
    struct ByTopic { nav: Key, cam: Key }
    impl Sealer for ByTopic {
        const VERSION: u8 = 1;
        const TRAILER: usize = 0;
        const ROSTER: usize = 0;
        fn verify(&mut self, _: u32, _: &[u8], _: &[u8; 12], _: &mut [u8]) -> Result<(), seal::Error> { Ok(()) }
        fn resume(&mut self) {}
        fn seal(&mut self, nonce: &[u8; 12], ad: &[u8], plain: &mut [u8], n: usize, out: &mut [u8]) -> Result<usize, seal::Error> {
            match &ad[1..] { b"nav" => self.nav.seal(nonce, ad, plain, n, out), b"cam" => self.cam.seal(nonce, ad, plain, n, out), _ => Err(seal::Error::Tag) } // refuse, never default
        }
        fn open(&mut self, ad: &[u8], wire: &[u8], out: &mut [u8]) -> Result<Opened, seal::Error> {
            match &ad[1..] { b"nav" => self.nav.open(ad, wire, out), b"cam" => self.cam.open(ad, wire, out), _ => Err(seal::Error::Tag) }
        }
        fn intact(&self) -> bool { self.nav.intact() && self.cam.intact() }
    }
    let mut s = ByTopic { nav: Key::new([1; 32]), cam: Key::new([2; 32]) };
    let (mut w, mut out) = ([0u8; 64], [0u8; 64]);
    let n = s.seal(&[9; 12], b"\x01nav", &mut [b'x'], 1, &mut w).unwrap();
    assert!(s.open(b"\x01nav", &w[..n], &mut out).is_ok());
    assert_eq!(s.open(b"\x01cam", &w[..n], &mut out), Err(seal::Error::Tag));
    assert_eq!(s.seal(&[9; 12], b"\x01gcs-7", &mut [b'x'], 1, &mut w), Err(seal::Error::Tag), "an unknown inbox is refused, not sealed under a default");
}

#[test]
fn the_header_mask_is_per_wire_so_nothing_of_the_nonce_reaches_the_air() {
    let mut k = Key::new([5u8; 32]);
    let (mut headers, mut nonces) = (Vec::new(), Vec::new());
    for counter in 0..1000u32 {
        let mut nonce = [0u8; 12];
        nonce[0] = 7; // sender 7, epoch 0: the high bytes are zero, as in every real fleet
        nonce[8..].copy_from_slice(&counter.to_le_bytes());
        let mut w = [0u8; 64];
        k.seal(&nonce, b"nav", &mut b"same payload".to_vec(), 12, &mut w).unwrap();
        headers.push(w[..12].to_vec());
        nonces.push(nonce);
    }
    for i in 0..12 { assert!(headers.iter().any(|h| h[i] != headers[0][i]), "header byte {i} is constant: a pad reused over a zero"); }
    for (i, (a, b)) in headers.iter().zip(&headers[1..]).enumerate() {
        let xor: Vec<u8> = a.iter().zip(b).map(|(x, y)| x ^ y).collect();
        let delta: Vec<u8> = nonces[i].iter().zip(&nonces[i + 1]).map(|(x, y)| x ^ y).collect();
        assert_ne!(xor, delta, "two headers XOR to the counter step at wire {i}");
    }
    let mut out = [0u8; 64];
    let mut w = [0u8; 64];
    let n = k.seal(&nonces[3], b"nav", &mut [b'x'], 1, &mut w).unwrap();
    assert_eq!(k.open(b"nav", &w[..n], &mut out).map(|o| o.nonce), Ok(nonces[3]), "the receiver has the sample before it needs the mask");
}
