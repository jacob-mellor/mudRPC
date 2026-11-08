//! Hardening. Four properties: no mutation of a saved block traps `resume` or the peer after it,
//! key bytes do not outlive their slot, every counter holds at its edge under overflow checks, and a soak across the
//! counter's edge accepts no nonce twice.
//! Seeded: `MUDRPC_SEED` picks the random mutations, `MUDRPC_FUZZ` how many (the single-byte sweep is always exhaustive).
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
    clippy::many_single_char_names, // test scripts: `a` and `b` are the peers, `w` a wire, `e` an envelope
)]
use core::mem::{align_of, size_of, MaybeUninit};
use mudrpc_core::rpc::{Counts, Dropped, Error, Incoming, MAX_SEQ, MAX_TTL, SEND_PER_SEC, VERSION};
use mudrpc_core::seal::{self, Key, Sealer};

mod support;
use support::{env, from_block, sends, Rng, KEY, N};

const SIZE: usize = size_of::<N>();
fn peer(name: &str, sender: u32, epoch: u32) -> N { N::new(name, Key::new(KEY), sender, epoch).unwrap() }
const fn bytes<T>(t: &T) -> &[u8] { unsafe { std::slice::from_raw_parts(std::ptr::from_ref(t).cast::<u8>(), size_of::<T>()) } }
fn has(hay: &[u8], needle: &[u8]) -> bool { hay.windows(needle.len()).any(|w| w == needle) }

/// (sender, epoch, counter) of a wire sealed under KEY.
fn nonce((topic, wire): &(String, Vec<u8>)) -> (u32, u32, u32) {
    let mut ad = vec![VERSION];
    ad.extend_from_slice(topic.as_bytes());
    let n = Key::new(KEY).open(&ad, &wire[1..], &mut [0u8; 256]).unwrap().nonce;
    let u = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap());
    (u(&n[0..4]), u(&n[4..8]), u(&n[8..12]))
}

/// `p` with its next counter set, as a restored block would hold it.
fn at_counter(p: &N, next: u64) -> N {
    let mut m = bytes(p).to_vec();
    let i = epoch_at().end.next_multiple_of(align_of::<u64>());
    m[i..i + 8].copy_from_slice(&next.to_le_bytes());
    from_block(&m)
}

/// Where the epoch lives in the block: the only bytes two otherwise identical peers disagree on.
fn epoch_at() -> std::ops::Range<usize> {
    let (x, y) = (peer("gcs", 1, 1), peer("gcs", 1, 2));
    let i = bytes(&x).iter().zip(bytes(&y)).position(|(a, b)| a != b).unwrap();
    i..i + 4
}

#[test]
fn no_mutation_of_a_saved_block_traps_resume_or_the_peer_after_it() {
    // a busy peer: a resolved call, a pending one, a seen event, a known sender, a staged key, a send still queued
    let (mut a, mut b) = (peer("gcs", 1, 1), peer("drone", 2, 1));
    a.call(0, "nav", "Arm", b"", 60).unwrap();
    let (t, w) = sends(&mut a, 0).remove(0);
    let mut buf = [0u8; 256];
    let Incoming::Call(c) = b.deliver(1, &t, &w, &mut buf) else { panic!() };
    b.reply(1, &c, 0, 0, "", b"ok", true).unwrap();
    b.publish(1, "gcs", b"evt", 60).unwrap();
    for (t, w) in sends(&mut b, 1) { assert!(!matches!(a.deliver(2, &t, &w, &mut [0u8; 256]), Incoming::Dropped(_))); }
    a.call(3, "nav", "Land", b"", 60).unwrap();
    a.sealer_mut().stage([9; 32]);
    a.publish(3, "nav", b"queued", 60).unwrap();
    let saved = bytes(&a).to_vec();
    b.publish(4, "gcs", b"fresh", 60).unwrap();
    let fresh = sends(&mut b, 4).remove(0);
    let epoch = epoch_at();
    let check = |writes: &[(usize, u8)]| {
        let mut m = saved.clone();
        for &(i, v) in writes { m[i] = v; }
        let mut p = from_block(&m);
        match p.resume(u32::MAX) {
            Ok(()) => {}
            Err(Error::Corrupt) => return,
            Err(Error::Epoch) if writes.iter().any(|(i, _)| epoch.contains(i)) => return,
            e => panic!("{writes:?}: {e:?}"),
        }
        let r = p.call(0, "nav", "M", b"", 10);
        assert!(matches!(r, Ok(_) | Err(Error::Unprovisioned | Error::Full)), "{writes:?}: {r:?}");
        sends(&mut p, 0);
        let _ = p.deliver(1, &fresh.0, &fresh.1, &mut [0u8; 256]);
        let _ = p.deliver(1, &fresh.0, &fresh.1, &mut [0u8; 256]);
        while p.tick(u32::MAX).is_some() {}
    };
    check(&[]);
    let mut rng = Rng(env("MUDRPC_SEED", 1));
    for (i, &b) in saved.iter().enumerate() {
        for v in [0, 0xFF, b ^ 1, b ^ 0x80, b.wrapping_add(1), rng.next() as u8] { check(&[(i, v)]); }
    }
    for _ in 0..env("MUDRPC_FUZZ", 20_000) {
        let writes: Vec<(usize, u8)> = match rng.below(3) {
            0 => vec![(rng.below(SIZE), rng.next() as u8), (rng.below(SIZE), rng.next() as u8)],
            1 => { let (s, fill) = (rng.below(SIZE), [0, 0xFF][rng.below(2)]); (s..(s + 1 + rng.below(16)).min(SIZE)).map(|i| (i, fill)).collect() }
            _ => { let s = rng.below(SIZE); (s..(s + 1 + rng.below(16)).min(SIZE)).map(|i| (i, rng.next() as u8)).collect() }
        };
        check(&writes);
    }
}

#[test]
fn key_bytes_do_not_outlive_their_slot() {
    let mut k = Key::new([1; 32]);
    k.stage([2; 32]);
    k.rotate();
    k.stage([3; 32]);
    k.rotate();
    let once = |needle: &[u8]| bytes(&k).windows(needle.len()).filter(|w| *w == needle).count() == 1;
    assert!(!has(bytes(&k), &[1; 32]) && !has(bytes(&k), &seal::check(&[1; 32])), "overwritten by the second rotate");
    assert!(once(&[3; 32]) && once(&seal::check(&[3; 32])), "the vacated next slot is zeroed, not left as a copy");
    k.retire();
    assert!(!has(bytes(&k), &[2; 32]) && !has(bytes(&k), &seal::check(&[2; 32])), "retire zeroes the previous slot");
    assert!(has(bytes(&k), &[3; 32]), "the current key is untouched");
    let mut slot = MaybeUninit::new(k);
    unsafe { slot.assume_init_drop() };
    assert!(bytes(&slot).iter().all(|&x| x == 0), "a dropped Key is all zero");
    let mut slot = MaybeUninit::<N>::uninit(); // placed the way ffi.rs places a peer: ptr::write into the caller's memory
    unsafe { slot.as_mut_ptr().write(peer("gcs", 1, 1)); slot.assume_init_drop() };
    assert!(!has(bytes(&slot), &KEY) && !has(bytes(&slot), &seal::check(&KEY)), "a dropped Peer leaves no key behind");
}

#[test]
fn every_counter_holds_at_its_edge() {
    // epoch at u32::MAX: it sends, and nothing is above it
    let mut a = peer("gcs", 1, u32::MAX);
    sends(&mut a, 0);
    a.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce(&sends(&mut a, 0)[0]), (1, u32::MAX, 0));
    assert_eq!(a.renew(u32::MAX), Err(Error::Epoch));
    assert_eq!(from_block(bytes(&a)).resume(u32::MAX), Err(Error::Epoch), "no epoch is above u32::MAX");
    // the counter one below exhaustion: its last value is used, the next send is refused, renew starts over
    let mut a = peer("gcs", 1, 1);
    sends(&mut a, 0);
    let mut a = at_counter(&a, u64::from(u32::MAX));
    a.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce(&sends(&mut a, 0)[0]), (1, 1, u32::MAX), "the last counter");
    assert_eq!(a.publish(0, "t", b"x", 10), Err(Error::Exhausted));
    assert_eq!(a.call(0, "t", "M", b"", 10), Err(Error::Exhausted));
    assert_eq!(a.listen("u"), Ok(()), "listening needs no nonce");
    a.renew(2).unwrap();
    a.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce(&sends(&mut a, 0)[0]), (1, 2, 0));
    // a stream of MAX_SEQ items: the last seq completes it
    let (mut a, mut b) = (peer("gcs", 1, 1), peer("drone", 2, 1));
    a.call(0, "cam", "Frames", b"", 60).unwrap();
    let (t, w) = sends(&mut a, 0).remove(0);
    let mut buf = [0u8; 256];
    let Incoming::Call(c) = b.deliver(0, &t, &w, &mut buf) else { panic!() };
    assert_eq!(b.reply(0, &c, MAX_SEQ, 0, "", b"", true), Err(Error::TooLarge));
    for seq in 0..MAX_SEQ {
        let now = seq / SEND_PER_SEC;
        b.reply(now, &c, seq, 0, "", b"", seq + 1 == MAX_SEQ).unwrap();
        let (t, w) = sends(&mut b, now).remove(0);
        assert!(matches!(a.deliver(1, &t, &w, &mut [0u8; 256]), Incoming::Reply { seq: s, .. } if s == seq), "item {seq}");
    }
    assert_eq!(a.tick(60), None, "complete: nothing left to expire");
    // ttl at MAX_TTL and now at u32::MAX: expiry saturates to now
    let mut a = peer("gcs", 1, 1);
    let id = a.call(u32::MAX, "nav", "M", b"", MAX_TTL).unwrap();
    assert_eq!(a.tick(u32::MAX), Some(Incoming::Expired { id }), "the end of time: expired on arrival");
    assert!(sends(&mut a, u32::MAX).is_empty(), "and dead at the exit");
    b.publish(u32::MAX, "gcs", b"x", MAX_TTL).unwrap();
    let (t, w) = sends(&mut b, 0).remove(0); // taken before the end of time, or the exit would drop it too
    assert!(matches!(a.deliver(u32::MAX, &t, &w, &mut buf), Incoming::Event { .. }));
    assert_eq!(a.deliver(u32::MAX, &t, &w, &mut buf), Incoming::Dropped(Dropped::Duplicate), "delivered again at the end of time: judged as in any other second");
    // the last second of uptime reads no deadline: a resumed sender's held wire inside its window lands, and no sender heard is evicted
    let (mut r, mut d) = (peer("gcs", 1, 1), peer("drone", 2, 1));
    let held: Vec<_> = (0..3).map(|_| { d.publish(0, "gcs", b"x", 60).unwrap(); sends(&mut d, 0).remove(0) }).collect();
    assert!(matches!(r.deliver(10, &held[0].0, &held[0].1, &mut buf), Incoming::Event { .. }));
    let mut r = from_block(bytes(&r));
    r.resume(2).unwrap();
    assert!(matches!(r.deliver(u32::MAX - 1, &held[2].0, &held[2].1, &mut buf), Incoming::Event { .. }));
    assert!(matches!(r.deliver(u32::MAX, &held[1].0, &held[1].1, &mut buf), Incoming::Event { .. }), "one behind the newest, at 2³² − 1: not a floor risen");
    let mut fleet: Vec<N> = (3..2 + 16).map(|s| peer("drone", s, 1)).collect(); // with drone 2, every window heard in the second before
    for x in &mut fleet { x.publish(0, "gcs", b"x", 60).unwrap(); let (t, w) = sends(x, 0).remove(0); assert!(matches!(r.deliver(u32::MAX - 1, &t, &w, &mut buf), Incoming::Event { .. })); }
    let mut newcomer = peer("drone", 99, 1);
    newcomer.publish(0, "gcs", b"x", 60).unwrap();
    let (t, w) = sends(&mut newcomer, 0).remove(0);
    assert!(matches!(r.deliver(u32::MAX, &t, &w, &mut buf), Incoming::Event { .. }));
    assert_eq!(r.deliver(u32::MAX, &held[1].0, &held[1].1, &mut buf), Incoming::Dropped(Dropped::Duplicate), "drone 2 kept its window: no slot is silent a week at the end of time");
}

/// A soak: an honest sender publishes to one receiver from half a run below the counter's edge, across it with `renew`
/// into the last epoch; the receiver calls it every 64 wires and two in three are answered; one wire in four is a replay
/// from the last 4096 or from an archive across the run; the clock jumps a week three times, so the floor rises over
/// every wire before. Beside the peer an oracle: every nonce accepted, by emission order; the pending calls and their
/// expiry; the counters. Then time per wire, in tenths of the run.
#[test]
fn a_soak_across_the_counter_edge_accepts_no_nonce_twice_and_time_per_wire_does_not_rise() {
    let wires = env("MUDRPC_FUZZ", 10_000) * 1000; // ten million on every push, a billion before a release
    let (mut rng, first) = (Rng(env("MUDRPC_SEED", 1) ^ 5), (1u64 << 32) - wires / 2);
    let (mut r, mut s) = (peer("gcs", 1, 1), peer("drone", 2, u32::MAX - 1));
    sends(&mut r, 0);
    sends(&mut s, 0);
    let mut s = at_counter(&s, first);
    let mut accepted = vec![0u64; (wires + wires / 16) as usize / 64 + 1];
    let mut accept = |w: &(String, Vec<u8>)| {
        let (_, e, c) = nonce(w);
        let i = (if e == u32::MAX { (1 << 32) - first + u64::from(c) } else { u64::from(c) - first }) as usize;
        assert_eq!(accepted[i / 64] >> (i % 64) & 1, 0, "{e}:{c} accepted twice");
        accepted[i / 64] |= 1 << (i % 64);
    };
    let renewed = |s: &mut N, send: &dyn Fn(&mut N) -> Result<(), Error>| match send(s) { Err(Error::Exhausted) => { s.renew(u32::MAX).unwrap(); send(s).unwrap(); } r => r.unwrap() };
    let (mut pending, mut counts, mut pool, mut archive) = (std::collections::HashMap::new(), Counts::default(), Vec::new(), Vec::new());
    let (mut buf, mut now, mut tenths, mut clock) = ([0u8; 256], 0, Vec::new(), std::time::Instant::now());
    for i in 0..wires {
        if i > 0 && i % (wires / 4) == 0 { now += MAX_TTL; }
        if i % 16 == 0 { now += 1; } // the publishes and a reply under the send cap
        while let Some(v) = r.tick(now) { let Incoming::Expired { id } = v else { panic!("{v:?}") }; assert!(pending.remove(&id.0).is_some_and(|at| at <= now)); }
        if i % 64 == 0 {
            let ttl = 1 + rng.below(8) as u32;
            match r.call(now, "drone", "Ping", b"", ttl) { Ok(id) => { pending.insert(id.0, now + ttl); } Err(e) => assert_eq!((e, pending.len()), (Error::Full, 4)) }
            for (t, w) in sends(&mut r, now) {
                let mut b = [0u8; 256];
                let Incoming::Call(c) = s.deliver(now, &t, &w, &mut b) else { panic!() };
                if i % 192 != 0 { renewed(&mut s, &|s| s.reply(now, &c, 0, 0, "", b"pong", true)); }
            }
        }
        let (ttl, bytes) = (1 + rng.below(120) as u32, i.to_le_bytes());
        renewed(&mut s, &|s| s.publish(now, "telemetry", &bytes, ttl));
        for w in sends(&mut s, now) {
            match r.deliver(now, &w.0, &w.1, &mut buf) { Incoming::Event { sender: 2, .. } => {} Incoming::Reply { id, .. } => assert!(pending.remove(&id.0).is_some()), v => panic!("wire {i}: {v:?}") }
            accept(&w);
            if i % (wires / 64).max(1) == 0 { archive.push(w.clone()); }
            if pool.len() < 4096 { pool.push(w); } else { pool[rng.below(4096)] = w; }
        }
        if i % 4 == 0 {
            let w = if rng.below(8) == 0 { &archive[rng.below(archive.len())] } else { &pool[rng.below(pool.len())] };
            match r.deliver(now, &w.0, &w.1, &mut buf) {
                Incoming::Rollback { sender: 2 } => counts.rollbacks += 1,
                Incoming::Dropped(Dropped::Duplicate) => {}
                Incoming::Dropped(Dropped::Replay | Dropped::NoPending) => counts.dropped += 1,
                v => panic!("wire {i}: a replay was {v:?}"),
            }
        }
        if (i + 1) % (wires / 10).max(1) == 0 { tenths.push(clock.elapsed().as_nanos() / u128::from(wires / 10)); clock = std::time::Instant::now(); }
    }
    assert_eq!(r.counts(), counts, "every drop and rollback counted, nothing saturated");
    s.publish(now, "telemetry", b"last", 1).unwrap();
    assert!(counts.rollbacks > 0 && nonce(&sends(&mut s, now)[0]).1 == u32::MAX, "the floor's epoch passed the old one, and the sender ends in the last epoch");
    eprintln!("{wires} wires, ns per wire by tenth: {tenths:?}");
    assert!(tenths.last() <= tenths.first().map(|t| 3 * t).as_ref(), "time per wire does not rise: {tenths:?}");
}
