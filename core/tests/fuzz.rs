//! The seeded mutator over the entry points `harden.rs` (resume) and `wire.rs` (the codec) do not cover: `deliver` on
//! mutated and random wires and topics, `outgoing` on every buffer size, `reply` on fabricated calls. `MUDRPC_SEED` and
//! `MUDRPC_FUZZ` as in harden.rs; a release runs it long. The property is the same each time: no trap, and the peer
//! still carries an honest wire afterwards.
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
    clippy::cast_sign_loss, // as above
    clippy::many_single_char_names, // test scripts: `a` and `b` are the peers, `w` a wire, `e` an envelope
)]
use mudrpc_core::envelope::Id;
use mudrpc_core::rpc::{Call, Error, Incoming, Outgoing};
mod support;
use support::{env, peer, sends, Rng};

fn junk(rng: &mut Rng, max: usize) -> Vec<u8> { (0..rng.below(max + 1)).map(|_| rng.next() as u8).collect() }

#[test]
fn deliver_survives_mutated_and_random_wires_and_topics_and_still_carries_an_honest_one() {
    let mut rng = Rng(env("MUDRPC_SEED", 1));
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    let rounds = env("MUDRPC_FUZZ", 20_000);
    for r in 0..rounds {
        let now = r as u32 + 1;
        a.publish(now, "nav", &[r as u8], 60).unwrap();
        let (t, mut w) = sends(&mut a, now).remove(0);
        match rng.below(4) {
            0 => for _ in 0..=rng.below(3) { let i = rng.below(w.len()); w[i] ^= 1 << rng.below(8); },
            1 => w.truncate(rng.below(w.len() + 1)),
            2 => w = junk(&mut rng, 300),
            _ => {}
        }
        let topic = match rng.below(3) { 0 => t, 1 => "x".repeat(rng.below(80)), _ => String::from_utf8_lossy(&junk(&mut rng, 20)).into_owned() };
        let mut buf = vec![0u8; rng.below(300)];
        let _ = b.deliver(now, &topic, &w, &mut buf);
        let _ = b.tick(now);
    }
    let now = rounds as u32 + 2;
    a.publish(now, "nav", b"alive", 60).unwrap();
    let (t, w) = sends(&mut a, now).remove(0);
    assert!(matches!(b.deliver(now, &t, &w, &mut [0u8; 256]), Incoming::Event { .. }), "the peer carries an honest wire after the storm");
}

#[test]
fn outgoing_and_sent_in_any_order_leave_the_outbox_draining() {
    let mut rng = Rng(env("MUDRPC_SEED", 1) ^ 2);
    let mut a = peer("gcs", 1);
    for r in 0..env("MUDRPC_FUZZ", 20_000) / 4 {
        let now = r as u32;
        let _ = a.publish(now, "nav", &[1], 60);
        if rng.below(2) == 0 { let _ = a.outgoing(now); }
        if rng.below(2) == 0 { a.sent(); }
    }
    while a.outgoing(u32::MAX - 1).is_some() { a.sent(); }
    a.publish(u32::MAX - 1, "nav", b"one", 1).unwrap();
    assert!(matches!(a.outgoing(u32::MAX - 1), Some(Outgoing::Send { .. })), "exactly what was queued, nothing stuck");
}

#[test]
fn reply_takes_any_fabricated_call_and_answers_a_code() {
    let mut rng = Rng(env("MUDRPC_SEED", 1) ^ 3);
    let mut b = peer("drone", 2);
    let big = "x".repeat(300);
    let bytes: Vec<u8> = (0..600).map(|i| i as u8).collect();
    for r in 0..env("MUDRPC_FUZZ", 20_000) {
        let now = r as u32;
        let mut id = [0u8; 12];
        for x in &mut id { *x = rng.next() as u8; }
        let call = Call { id: Id(id), sender: rng.next() as u32, topic: &big[..rng.below(70)], method: &big[..rng.below(70)], reply_to: &big[..rng.below(70)], ttl_secs: rng.next() as u32, seq: rng.next() as u32 % 70, end: rng.next() & 1 == 1, bytes: &bytes[..rng.below(601)] };
        let detail = &big[..rng.below(301)];
        let got = b.reply(now, &call, rng.next() as u32 % 70, rng.next() as u32, detail, &bytes[..rng.below(601)], rng.next() & 1 == 1);
        assert!(matches!(got, Ok(()) | Err(Error::Full | Error::TooLarge | Error::Empty)), "round {r}: {got:?}");
        sends(&mut b, now);
    }
}
