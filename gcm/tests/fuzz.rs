//! The seeded mutator over the GCM open path: wires flipped, truncated, extended or replaced, under associated data and
//! buffers of any size, through `open` and through a peer's `deliver` under `Gcm`. `MUDRPC_SEED` and `MUDRPC_FUZZ` as in
//! core/tests/harden.rs; a release runs it long. The property: a code every time, never a trap, a changed wire never
//! opens, and the honest one always does.
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
    clippy::many_single_char_names, // test scripts: `a` and `b` are the peers, `w` a wire, `t` a topic
)]
use mudrpc_core::rpc::{Incoming, Outgoing, Peer};
use mudrpc_core::seal::Error;
use mudrpc_gcm::{open, seal, Gcm};

struct Rng(u64);
impl Rng {
    const fn next(&mut self) -> u64 { let mut x = self.0; x ^= x << 13; x ^= x >> 7; x ^= x << 17; self.0 = x; x }
    const fn below(&mut self, n: usize) -> usize { if n == 0 { 0 } else { (self.next() % n as u64) as usize } }
}
fn env(name: &str, default: u64) -> u64 { std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default) }
fn junk(rng: &mut Rng, max: usize) -> Vec<u8> { (0..rng.below(max + 1)).map(|_| rng.next() as u8).collect() }

#[test]
fn gcm_open_refuses_every_mutated_wire_and_still_opens_the_honest_one() {
    let mut rng = Rng(env("MUDRPC_SEED", 1) ^ 4);
    let key = [7u8; 32];
    for r in 0..env("MUDRPC_FUZZ", 20_000) {
        let mut nonce = [0u8; 12];
        nonce[8..].copy_from_slice(&(r as u32).to_le_bytes());
        let (plain, ad) = (junk(&mut rng, 100), junk(&mut rng, 20));
        let mut honest = vec![0u8; 12 + plain.len() + 16];
        assert_eq!(seal(&key, &nonce, &ad, &plain, &mut honest), Ok(honest.len()));
        let mut w = honest.clone();
        match rng.below(5) {
            0 => for _ in 0..=rng.below(3) { let i = rng.below(w.len()); w[i] ^= 1 << rng.below(8); },
            1 => w.truncate(rng.below(w.len() + 1)),
            2 => w.push(rng.next() as u8),
            3 => w = junk(&mut rng, 150),
            _ => {}
        }
        let mut out = vec![0u8; rng.below(130)];
        let got = open(&key, &ad, &w, &mut out);
        match got {
            Ok(n) => { assert_eq!((&w, &out[..n]), (&honest, plain.as_slice()), "round {r}: only the honest wire opens, to its plaintext"); }
            Err(Error::Short) => assert!(w.len() < 28, "round {r}"),
            Err(Error::Full) => assert!(out.len() < w.len() - 28, "round {r}"),
            Err(e) => assert!(e == Error::Tag && w != honest, "round {r}: {e:?}"),
        }
    }
}

type N = Peer<Gcm, 256, 4, 8, 16, 4>;
fn send(a: &mut N, now: u32, bytes: &[u8]) -> (String, Vec<u8>) {
    a.publish(now, "nav", bytes, 60).unwrap();
    let mut v = Vec::new();
    while let Some(o) = a.outgoing(now) {
        if let Outgoing::Send { topic, wire, .. } = o { v.push((topic.to_string(), wire.to_vec())); }
        a.sent();
    }
    v.remove(0)
}

#[test]
fn gcm_deliver_survives_mutated_and_random_wires_and_topics_and_still_carries_an_honest_one() {
    let mut rng = Rng(env("MUDRPC_SEED", 1) ^ 5);
    let (mut a, mut b) = (N::new("gcs", Gcm::new([7; 32]), 1, 1).unwrap(), N::new("drone", Gcm::new([7; 32]), 2, 1).unwrap());
    b.listen("nav").unwrap();
    let rounds = env("MUDRPC_FUZZ", 20_000);
    for r in 0..rounds {
        let now = r as u32 + 1;
        let (t, mut w) = send(&mut a, now, &[r as u8]);
        let honest = w.clone();
        match rng.below(5) {
            0 => for _ in 0..=rng.below(3) { let i = rng.below(w.len()); w[i] ^= 1 << rng.below(8); },
            1 => w.truncate(rng.below(w.len() + 1)),
            2 => w.push(rng.next() as u8),
            3 => { w = junk(&mut rng, 300); if let Some(v) = w.first_mut() { *v = 3; } } // past the version check, into the cipher
            _ => {}
        }
        let topic = match rng.below(3) { 0 => t.clone(), 1 => "x".repeat(rng.below(80)), _ => String::from_utf8_lossy(&junk(&mut rng, 20)).into_owned() };
        if let Incoming::Event { bytes, .. } = b.deliver(now, &topic, &w, &mut vec![0u8; rng.below(300)]) { assert!(w == honest && topic == t && bytes == [r as u8], "round {r}: a changed wire or topic landed"); }
        let _ = b.tick(now);
    }
    let now = rounds as u32 + 2;
    let (t, w) = send(&mut a, now, b"alive");
    assert!(matches!(b.deliver(now, &t, &w, &mut [0u8; 256]), Incoming::Event { .. }), "the peer carries an honest wire after the storm");
}
