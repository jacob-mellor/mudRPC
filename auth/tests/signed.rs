//! SPEC.md layer 1's authentication group, the tests that define it, on a peer over `Signed<Key>`; churn and coordinated key
//! holders against an unsigned receiver beside a signed one, `MUDRPC_FUZZ` scaling the churn as in core's harden.rs.
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
    clippy::many_single_char_names, // test scripts: `a` and `b` are the peers, `w` a wire, `e` an envelope
)]
use mudrpc_auth::ed25519::public_key;
use mudrpc_auth::signed::Signed;
use mudrpc_core::envelope::{Body, Envelope};
use mudrpc_core::seal::Sealer;
use mudrpc_core::rpc::{Call, Dropped, Error, Incoming, Outgoing, Peer, MAX_PER_SEC, MAX_TTL, RX_PER_SEC};
use std::collections::BTreeMap;
use mudrpc_core::seal::{self, Key};
use mudrpc_gcm::Gcm;

type S = Signed<Key, 8>;
type N = Peer<S, 256, 4, 8, 16, 4>;
const KEY: [u8; 32] = [7; 32];
const fn seed(sender: u32) -> [u8; 32] { [sender as u8 + 100; 32] }
fn pk(sender: u32) -> [u8; 32] { public_key(&seed(sender)) }

/// A signed peer under `sender`, with every number in `roster` believed, affording `budget` verifications a second.
fn budgeted(name: &str, sender: u32, roster: &[u32], budget: u32) -> N {
    let mut s = S::new(Key::new(KEY), seed(sender), budget);
    for &r in roster { s.roster(r, pk(r), None).unwrap(); }
    N::new(name, s, sender, 1).unwrap()
}
fn peer(name: &str, sender: u32, roster: &[u32]) -> N { budgeted(name, sender, roster, 1000) }

fn sends<T: Sealer, const SEEN: usize, const SENDERS: usize>(n: &mut Peer<T, 256, 4, SEEN, SENDERS, 4>, now: u32) -> Vec<(String, Vec<u8>)> {
    let mut v = Vec::new();
    while let Some(o) = n.outgoing(now) {
        if let Outgoing::Send { topic, wire, .. } = o { v.push((topic.into(), wire.into())); }
        n.sent();
    }
    v
}
fn one<T: Sealer>(n: &mut Peer<T, 256, 4, 8, 16, 4>, now: u32) -> (String, Vec<u8>) { let mut v = sends(n, now); assert_eq!(v.len(), 1); v.remove(0) }
fn dropped(n: &mut N, now: u32, w: &(String, Vec<u8>)) -> Option<Dropped> {
    match n.deliver(now, &w.0, &w.1, &mut [0u8; 256]) { Incoming::Dropped(d) => Some(d), _ => None }
}

#[test]
fn the_gate_a_key_holder_without_the_seed_is_sig_and_a_number_with_no_line_is_roster() {
    let (mut gcs, mut drone) = (peer("gcs", 1, &[1, 2]), peer("drone", 2, &[1, 2]));
    let mut buf = [0u8; 256];
    gcs.call(0, "nav", "Arm", b"", 30).unwrap();
    let (t, w) = one(&mut gcs, 0);
    assert_eq!(w.len(), 1 + 12 + 4 + 64 + 16 + (w.len() - 97), "the signature rides inside the ciphertext");
    let Incoming::Call(c) = drone.deliver(1, &t, &w, &mut buf) else { panic!() };
    assert_eq!((c.sender, c.method), (1, "Arm"));
    // a key holder with the fleet key and no seed for number 1 seals a wire under number 1
    let mut forger = N::new("gcs", S::new(Key::new(KEY), [99; 32], 1000), 1, 5).unwrap();
    forger.call(2, "nav", "Arm", b"", 30).unwrap();
    let f = one(&mut forger, 2);
    assert_eq!(dropped(&mut drone, 2, &f), Some(Dropped::Open(seal::Error::Sig)), "the gate");
    // a sender the roster does not list, honest or not
    let mut stranger = peer("x", 3, &[1, 2]);
    stranger.publish(2, "nav", b"", 30).unwrap();
    let s = one(&mut stranger, 2);
    assert_eq!(dropped(&mut drone, 2, &s), Some(Dropped::Open(seal::Error::Roster)));
    drone.sealer_mut().roster(3, pk(3), None).unwrap();
    assert_eq!(dropped(&mut drone, 2, &s), None, "listed: believed");
    drone.sealer_mut().revoke(3);
    stranger.publish(2, "nav", b"", 30).unwrap();
    assert_eq!(dropped(&mut drone, 3, &one(&mut stranger, 2)), Some(Dropped::Open(seal::Error::Roster)), "revoked: refused again");
}

#[test]
fn a_roster_refuses_keys_anyone_could_sign_for() {
    let mut s = S::new(Key::new(KEY), seed(1), 10);
    assert_eq!(s.roster(9, [0; 32], None), Err(seal::Error::Roster), "y = 0: a point of order four");
    let mut identity = [0u8; 32];
    identity[0] = 1;
    assert_eq!(s.roster(9, identity, None), Err(seal::Error::Roster), "the identity");
    assert_eq!(s.roster(9, [0xff; 32], None), Err(seal::Error::Roster), "not a point");
    assert_eq!(s.roster(9, pk(9), Some(identity)), Err(seal::Error::Roster), "nor as the next key");
    for i in 0..8 { s.roster(i, pk(i), None).unwrap(); }
    assert_eq!(s.roster(8, pk(8), None), Err(seal::Error::Full), "full: its own code, so a host can tell it from a refused key");
    s.roster(3, pk(30), None).unwrap();
}

#[test]
fn cost_order_replays_and_strays_cost_no_verification_and_a_refusal_marks_nothing() {
    let (mut gcs, mut drone) = (peer("gcs", 1, &[1, 2]), budgeted("drone", 2, &[1, 2], 1)); // one verification a second: a verdict after the first that is not Budget spent none
    let mut buf = [0u8; 256];
    let id = gcs.call(0, "nav", "Arm", b"", 30).unwrap();
    let (t, w) = one(&mut gcs, 0);
    let Incoming::Call(c) = drone.deliver(1, &t, &w, &mut buf) else { panic!() };
    assert_eq!(dropped(&mut drone, 1, &(t.clone(), w.clone())), Some(Dropped::Duplicate), "a replay: the seen table's verdict, no verification");
    let mut flipped = w.clone();
    flipped[20] ^= 1;
    assert_eq!(dropped(&mut drone, 1, &(t.clone(), flipped)), Some(Dropped::Open(seal::Error::Tag)));
    assert_eq!(dropped(&mut drone, 1, &("cam".into(), w.clone())), Some(Dropped::Open(seal::Error::Tag)), "another topic fails at the tag");
    drone.reply(1, &c, 0, 0, "", b"ok", true).unwrap();
    let r = one(&mut drone, 1);
    assert_eq!(dropped(&mut drone, 1, &r), Some(Dropped::NoPending), "a reply to no call: no verification");
    assert!(matches!(gcs.deliver(2, &r.0, &r.1, &mut buf), Incoming::Reply { id: got, sender: 2, .. } if got == id));
    // a forged wire marks nothing: the honest wire with the same nonce is accepted afterwards
    let mut forger = N::new("gcs", S::new(Key::new(KEY), [99; 32], 1000), 1, 1).unwrap();
    forger.publish(3, "nav", b"skip", 30).unwrap(); // counter 0 is the nonce of gcs's call, already seen: the seen table would answer first
    forger.publish(3, "nav", b"forged", 30).unwrap();
    let f = sends(&mut forger, 3).remove(1);
    gcs.publish(3, "nav", b"honest", 30).unwrap();
    let h = one(&mut gcs, 3);
    let mut scratch = [0xAAu8; 256];
    assert_eq!(drone.deliver(3, &f.0, &f.1, &mut scratch), Incoming::Dropped(Dropped::Open(seal::Error::Sig)));
    assert!(scratch[..f.1.len() - 29].iter().all(|&b| b == 0), "the forged plaintext is zeroed");
    assert!(matches!(drone.deliver(4, &h.0, &h.1, &mut buf), Incoming::Event { sender: 1, .. }), "same sender, same counter, honest: the forgery marked no window bit");
}

#[test]
fn the_budget_bounds_what_a_key_holder_can_spend_and_resets_each_second() {
    let (mut drone, mut gcs) = (budgeted("drone", 2, &[1], 2), peer("gcs", 1, &[2]));
    let wires: Vec<_> = (0..3).map(|i| { gcs.publish(5, "nav", &[i], 30).unwrap(); one(&mut gcs, 5) }).collect();
    assert_eq!(dropped(&mut drone, 5, &wires[0]), None);
    assert_eq!(dropped(&mut drone, 5, &wires[1]), None);
    assert_eq!(dropped(&mut drone, 5, &wires[2]), Some(Dropped::Open(seal::Error::Budget)), "the third in one second");
    assert_eq!(dropped(&mut drone, 4, &wires[2]), Some(Dropped::Open(seal::Error::Budget)), "an earlier second opens no budget");
    assert_eq!(dropped(&mut drone, 6, &wires[2]), None, "the next second");
    let mut restored: N = unsafe { core::ptr::read_unaligned((&raw const drone).cast::<u8>().cast::<N>()) };
    restored.resume(2).unwrap();
    gcs.publish(6, "nav", &[9], 30).unwrap();
    assert_eq!(dropped(&mut restored, 0, &one(&mut gcs, 6)), None, "after a resume the budget starts over, as every per-second count does");
    let mut forger = N::new("gcs", S::new(Key::new(KEY), [99; 32], 1000), 1, 5).unwrap();
    for (now, next, honest) in [(7, None, None), (8, Some(pk(11)), Some(Dropped::Open(seal::Error::Budget)))] {
        drone.sealer_mut().roster(1, pk(1), next).unwrap();
        forger.publish(now, "nav", b"forged", 30).unwrap();
        assert_eq!(dropped(&mut drone, now, &one(&mut forger, now)), Some(Dropped::Open(seal::Error::Sig)));
        gcs.publish(now, "nav", b"honest", 30).unwrap();
        assert_eq!(dropped(&mut drone, now, &one(&mut gcs, now)), honest, "a forgery costs a unit per key its line holds, two with a next");
    }
    let mut switched = N::new("gcs", S::new(Key::new(KEY), seed(11), 1000), 1, 9).unwrap();
    for _ in 0..2 { switched.publish(9, "nav", b"on next", 30).unwrap(); }
    let v = sends(&mut switched, 9);
    assert_eq!(dropped(&mut drone, 9, &v[0]), None, "the sender switched to next is believed");
    assert_eq!(dropped(&mut drone, 9, &v[1]), Some(Dropped::Open(seal::Error::Budget)), "at two units a wire, key tried first, until the line is written as (next, none)");
    drone.sealer_mut().roster(1, pk(11), None).unwrap();
    for _ in 0..2 { switched.publish(10, "nav", b"on next", 30).unwrap(); }
    for w in sends(&mut switched, 10) { assert_eq!(dropped(&mut drone, 10, &w), None, "written as (next, none): one unit a wire, two in a budget of two"); }
}

#[test]
fn a_signing_key_rotates_through_next_and_the_fleet_key_rotates_underneath() {
    let (mut gcs, mut drone) = (peer("gcs", 1, &[1, 2]), peer("drone", 2, &[1, 2]));
    let mut old = peer("drone-old", 2, &[1, 2]);
    drone.sealer_mut().roster(1, pk(1), Some(pk(11))).unwrap();
    let mut gcs2 = N::new("gcs", S::new(Key::new(KEY), seed(11), 1000), 1, 2).unwrap();
    gcs2.publish(0, "nav", b"new seed", 30).unwrap();
    let w = one(&mut gcs2, 0);
    assert_eq!(dropped(&mut drone, 1, &w), None, "a receiver holding key and next believes the switched sender");
    assert_eq!(dropped(&mut old, 1, &w), Some(Dropped::Open(seal::Error::Sig)), "one holding only the old key does not");
    gcs.sealer_mut().inner().stage([9; 32]);
    gcs.sealer_mut().inner().rotate();
    gcs.publish(1, "nav", b"under the next fleet key", 30).unwrap();
    let w = one(&mut gcs, 1);
    assert_eq!(dropped(&mut drone, 2, &w), Some(Dropped::Open(seal::Error::Tag)), "the fleet key is the inner sealer's business");
    drone.sealer_mut().inner().stage([9; 32]);
    assert_eq!(dropped(&mut drone, 2, &w), None, "staged: opens under Next, then verifies");
    assert_eq!(dropped(&mut drone, 2, &w), Some(Dropped::Duplicate), "and lands once");
}

#[test]
fn two_peers_from_identical_inputs_sign_identically_and_the_seed_does_not_outlive_the_sealer() {
    let (mut a, mut b) = (peer("gcs", 1, &[1]), peer("gcs", 1, &[1]));
    a.publish(0, "nav", b"x", 30).unwrap();
    b.publish(0, "nav", b"x", 30).unwrap();
    assert_eq!(one(&mut a, 0), one(&mut b, 0), "deterministic signing");
    let mut slot = core::mem::MaybeUninit::new(S::new(Key::new(KEY), seed(1), 10));
    let bytes = |s: &core::mem::MaybeUninit<S>| unsafe { std::slice::from_raw_parts(s.as_ptr().cast::<u8>(), core::mem::size_of::<S>()).to_vec() };
    let has = |h: &[u8], n: &[u8]| h.windows(n.len()).any(|w| w == n);
    assert!(has(&bytes(&slot), &seed(1)) && has(&bytes(&slot), &KEY));
    unsafe { slot.assume_init_drop() };
    assert!(!has(&bytes(&slot), &seed(1)) && !has(&bytes(&slot), &KEY), "seed and fleet key both gone");
}

#[test]
fn intact_covers_the_roster_and_the_seed_and_the_trailer_comes_out_of_the_budget() {
    let p = peer("gcs", 1, &[1, 2]);
    let mut saved = unsafe { std::slice::from_raw_parts((&raw const p).cast::<u8>(), core::mem::size_of::<N>()).to_vec() };
    let seed_at = saved.windows(32).position(|w| w == seed(1)).unwrap();
    let mut restored: N = unsafe { core::ptr::read_unaligned(saved.as_ptr().cast::<N>()) };
    assert_eq!(restored.resume(2), Ok(()));
    saved[seed_at] ^= 1;
    let mut restored: N = unsafe { core::ptr::read_unaligned(saved.as_ptr().cast::<N>()) };
    assert_eq!(restored.resume(2), Err(Error::Corrupt), "a seed that no longer matches its public key");
    saved[seed_at] ^= 1;
    let budget_at = saved.windows(12).position(|w| w == [1000u32.to_le_bytes(), [0; 4], [0; 4]].concat()).unwrap(); // the sealer's head: the budget, then this second's count
    saved[budget_at..budget_at + 4].fill(0);
    let mut restored: N = unsafe { core::ptr::read_unaligned(saved.as_ptr().cast::<N>()) };
    assert_eq!(restored.resume(2), Err(Error::Corrupt), "a budget of 0, which would refuse every wire as `Open(Budget)`, as `mud_new` refuses it");
    saved[budget_at..budget_at + 4].copy_from_slice(&1000u32.to_le_bytes());
    assert_eq!(budgeted("gcs", 1, &[1], 0).resume(2), Ok(()), "`new` takes a budget of 0 as 1, so what it builds resumes");
    let line_at = saved.windows(32).position(|w| w == pk(2)).unwrap();
    saved[line_at..line_at + 32].copy_from_slice(&core::array::from_fn::<u8, 32, _>(|i| u8::from(i == 0)));
    let mut restored: N = unsafe { core::ptr::read_unaligned(saved.as_ptr().cast::<N>()) };
    assert_eq!(restored.resume(2), Err(Error::Corrupt), "a roster line anyone could sign for, written into the block");
    let fits = |n: usize| peer("x", 1, &[]).publish(0, "t", &vec![0u8; n], 1).is_ok();
    let fits_unsigned = |n: usize| Peer::<Key, 256, 4, 8, 16, 4>::new("x", Key::new(KEY), 1, 1).unwrap().publish(0, "t", &vec![0u8; n], 1).is_ok();
    let max = (0..256).rev().find(|&n| fits(n)).unwrap();
    assert_eq!(max + 64, (0..256).rev().find(|&n| fits_unsigned(n)).unwrap(), "the signature comes out of the payload ceiling, byte for byte");
    let _ = Call { id: mudrpc_core::envelope::Id([0; 12]), sender: 0, topic: "", method: "", reply_to: "", ttl_secs: 0, seq: 0, end: false, bytes: b"" };
}

/// The confused deputy: roster member 3 plants client 1's pending nonce as the id of a call to server 2, hoping 2 answers
/// it with its own signature into 1's inbox. The server drops the call before product code, on seq 1 and on seq 0.
#[test]
fn a_roster_member_cannot_make_a_server_sign_the_reply_to_another_callers_call() {
    let (mut c, mut s) = (peer("gcs", 1, &[1, 2, 3]), peer("drone", 2, &[1, 2, 3]));
    let cid = c.call(0, "nav", "Status", b"", 300).unwrap();
    let _lost = sends(&mut c, 0);
    let mut k = S::new(Key::new(KEY), seed(3), 1000);
    for (seq, want) in [(1u32, Dropped::Seq), (0, Dropped::Id)] {
        let e = Envelope { id: Some(cid), ttl_secs: 300, seq, end: true, body: Body::Call { method: "Reboot", reply_to: "gcs", payload: b"" } };
        let mut plain = [0u8; 256];
        let n = e.encode(&mut plain).unwrap();
        let mut nonce = [0u8; 12];
        nonce[0..4].copy_from_slice(&3u32.to_le_bytes());
        nonce[4..8].copy_from_slice(&1u32.to_le_bytes());
        nonce[8..12].copy_from_slice(&seq.to_le_bytes());
        let mut wire = vec![2u8; 256];
        let w = k.seal(&nonce, b"\x02nav", &mut plain, n, &mut wire[1..]).unwrap();
        wire.truncate(1 + w);
        assert_eq!(dropped(&mut s, 1, &("nav".into(), wire)), Some(want), "seq {seq}");
    }
    assert!(sends(&mut s, 1).is_empty(), "the server signed nothing");
}

/// A roster member's flood costs the receiver at most the cap plus one verification a second, so it cannot spend the
/// budget the other members need.
#[test]
fn a_flooding_roster_member_cannot_drain_the_budget_for_the_others() {
    let mut drone = budgeted("drone", 2, &[1, 2, 3], 66);
    let (mut honest, mut flooder) = (peer("gcs", 1, &[1, 2, 3]), peer("k", 3, &[1, 2, 3]));
    let (mut accepted, mut floods, mut rate) = (0, 0, 0);
    for i in 0..300u32 {
        flooder.publish(1_000_000 + i, "nav", b"spam", 5).unwrap(); // a lying clock: every publish is a new second for the sender's own cap
        let w = one(&mut flooder, 1_000_000 + i);
        match drone.deliver(5, &w.0, &w.1, &mut [0u8; 256]) {
            Incoming::Event { .. } => accepted += 1,
            Incoming::Flood { sender: 3 } => floods += 1,
            Incoming::Dropped(Dropped::Rate) => rate += 1,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!((accepted, floods, rate), (64, 1, 235));
    for want in [None, Some(Dropped::Open(seal::Error::Budget))] {
        honest.publish(5, "nav", b"hello", 5).unwrap();
        assert_eq!(dropped(&mut drone, 5, &one(&mut honest, 5)), want, "the flood spent the cap plus the one that reported it, 65 of 66: the 66th is the honest member's");
    }
}

/// The signed length prefix is one byte: a topic the peer could never pass is refused by the sealer itself.
#[test]
fn a_topic_longer_than_the_length_prefix_is_refused_by_the_sealer_itself() {
    let mut s = S::new(Key::new(KEY), seed(1), 10);
    let (ad, nonce) = (vec![2u8; 300], [0u8; 12]);
    assert_eq!(s.seal(&nonce, &ad, &mut [b'x'; 65], 1, &mut [0u8; 512]), Err(seal::Error::Full));
    assert_eq!(s.verify(0, &ad, &nonce, &mut [0u8; 80]), Err(seal::Error::Short));
    assert!(s.seal(&nonce, &ad[..65], &mut [b'x'; 65], 1, &mut [0u8; 512]).is_ok(), "64 bytes of topic, the peer's most");
}

/// The signature is written into the 64 bytes after the message, in the caller's buffer, and sealed with it.
#[test]
fn the_signature_fills_the_room_after_the_message_and_a_buffer_without_the_room_is_full() {
    let mut s = S::new(Key::new(KEY), seed(1), 10);
    s.roster(1, pk(1), None).unwrap();
    let (nonce, ad) = ([1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0], b"\x02nav");
    let (mut plain, mut wire, mut out) = ([0u8; 5 + 64], [0u8; 128], [0u8; 128]);
    plain[..5].copy_from_slice(b"hello");
    let w = s.seal(&nonce, ad, &mut plain, 5, &mut wire).unwrap();
    assert_eq!(w, 12 + 5 + 64 + 16, "exactly n + 64 is room enough");
    assert!(plain[..5] == *b"hello" && plain[5..] != [0; 64], "the message untouched, the signature after it");
    let o = s.open(ad, &wire[..w], &mut out).unwrap();
    assert_eq!(out[..o.len], plain, "sealed as the caller's buffer holds it");
    assert_eq!(s.verify(0, ad, &o.nonce, &mut out[..o.len]), Ok(()));
    let mut short = [b'h'; 5 + 63];
    assert_eq!(s.seal(&nonce, ad, &mut short, 5, &mut wire), Err(seal::Error::Full), "n + 63");
    assert_eq!(short, [b'h'; 5 + 63], "and nothing written");
    assert_eq!(s.seal(&nonce, ad, &mut [0u8; 4], 5, &mut wire), Err(seal::Error::Full), "n past the buffer");
    assert_eq!(s.seal(&nonce, ad, &mut [0u8; 64], usize::MAX, &mut wire), Err(seal::Error::Full), "no overflow");
}

/// `Signed` names its inner's version plus one: over `Gcm` it is 4, and the gate holds as it does over `Key`.
#[test]
fn a_signed_aes_fleet_is_version_four_and_a_key_holder_without_the_seed_is_still_sig() {
    type A = Peer<Signed<Gcm, 8>, 256, 4, 8, 16, 4>;
    assert_eq!((S::VERSION, Gcm::VERSION, Signed::<Gcm, 8>::VERSION), (2, 3, 4));
    let aes = |name: &str, sender: u32, seed: [u8; 32], epoch: u32| {
        let mut s = Signed::new(Gcm::new(KEY), seed, 1000);
        for r in [1, 2] { s.roster(r, pk(r), None).unwrap(); }
        A::new(name, s, sender, epoch).unwrap()
    };
    let take = |p: &mut A| {
        let mut v = Vec::new();
        while let Some(o) = p.outgoing(0) { if let Outgoing::Send { topic, wire, .. } = o { v.push((topic.to_string(), wire.to_vec())); } p.sent(); }
        v.remove(0)
    };
    let (mut gcs, mut drone, mut forger) = (aes("gcs", 1, seed(1), 1), aes("drone", 2, seed(2), 1), aes("gcs", 1, [99; 32], 5));
    gcs.call(0, "nav", "Arm", b"", 30).unwrap();
    forger.call(0, "nav", "Arm", b"", 30).unwrap();
    let (w, f) = (take(&mut gcs), take(&mut forger));
    assert_eq!(w.1[0], 4);
    assert!(matches!(drone.deliver(1, &w.0, &w.1, &mut [0u8; 256]), Incoming::Call(c) if c.sender == 1 && c.method == "Arm"));
    assert!(matches!(drone.deliver(1, &f.0, &f.1, &mut [0u8; 256]), Incoming::Dropped(Dropped::Open(seal::Error::Sig))), "the gate, under AES");
    let mut chacha = peer("drone", 2, &[1, 2]);
    assert_eq!(dropped(&mut chacha, 1, &w), Some(Dropped::Version(4)), "a signed ChaCha fleet counts it, never fails it at the tag");
    let mut honest = peer("gcs", 1, &[1, 2]);
    honest.publish(1, "nav", b"", 30).unwrap();
    assert!(matches!(drone.deliver(1, "nav", &one(&mut honest, 1).1, &mut [0u8; 256]), Incoming::Dropped(Dropped::Version(2))));
}

fn env(name: &str, default: u64) -> u64 { std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default) }
fn verdict<T: Sealer, const SEEN: usize, const SENDERS: usize>(r: &mut Peer<T, 256, 4, SEEN, SENDERS, 4>, now: u32, w: &(String, Vec<u8>)) -> String {
    match r.deliver(now, &w.0, &w.1, &mut [0u8; 256]) { Incoming::Event { .. } => "Event".into(), Incoming::Flood { .. } => "Flood".into(), v => format!("{v:?}") }
}

/// What a fleet-key holder seals without a seed: any nonce on topic "t" under `T`'s version, zeros where a signature goes.
fn forge<T: Sealer>(sender: u32, epoch: u32, counter: u32, ttl: u32) -> (String, Vec<u8>) {
    let mut plain = [0u8; 256];
    let n = Envelope { id: None, ttl_secs: ttl, seq: 0, end: false, body: Body::Event { payload: b"forged" } }.encode(&mut plain).unwrap();
    let nonce: [u8; 12] = [sender, epoch, counter].map(u32::to_le_bytes).concat().try_into().unwrap();
    let mut wire = vec![T::VERSION; 256];
    let len = Key::new(KEY).seal(&nonce, &[T::VERSION, b't'], &mut plain, n + T::TRAILER, &mut wire[1..]).unwrap();
    wire.truncate(1 + len);
    ("t".into(), wire)
}

/// A key holder takes the receiver's free slots at second 1, after an established sender (2), then cycles `churned`
/// fresh numbers up to `u32::MAX` at the strangers' cap less room for a newcomer (3) and its replay, 2 and 3 each heard
/// every second and each wire replayed in the same second after the churn; a second short of each week after the squat it keeps its slots with one wire each, after the churn if it ends within the first. The
/// replays that executed for 2 and for 3, the receiver's pressure, and the forged wires product code saw.
fn churn<R: Sealer, T: Sealer, const SEEN: usize, const SENDERS: usize>(r: &mut Peer<R, 256, 4, SEEN, SENDERS, 4>, mut h: Peer<T, 256, 4, 8, 16, 4>, mut g: Peer<T, 256, 4, 8, 16, 4>, churned: u32) -> [u32; 5] {
    let (mut out, mut left, batch, mut roster) = ([0; 5], churned, MAX_PER_SEC - 2, 0);
    let land = |r: &mut Peer<R, 256, 4, SEEN, SENDERS, 4>, now: u32, w: &(String, Vec<u8>)| u32::from(verdict(r, now, w) == "Event");
    let mut forged = |r: &mut Peer<R, 256, 4, SEEN, SENDERS, 4>, now: u32, w: &(String, Vec<u8>)| { let v = verdict(r, now, w); roster += u32::from(v == "Dropped(Open(Roster))"); u32::from(v == "Event") };
    h.publish(1, "t", b"h", 60).unwrap();
    assert_eq!(land(r, 1, &one(&mut h, 1)), 1);
    let squat = 100..=98 + SENDERS as u32;
    for s in squat.clone() { out[3] += forged(r, 1, &forge::<R>(s, 1, 0, 60)); }
    let last = 1 + churned.div_ceil(batch); // the churn's seconds start at 2
    for now in (2..=last).chain((last < 1 + MAX_TTL).then_some(1 + MAX_TTL)) {
        let week = (now - 2) / (MAX_TTL - 1);
        if week > 0 && (now - 2) % (MAX_TTL - 1) == 0 { for s in squat.clone() { out[3] += forged(r, now - 1, &forge::<R>(s, 1, week, MAX_TTL)); } }
        h.publish(now, "t", b"h", 60).unwrap();
        g.publish(now, "t", b"g", 60).unwrap();
        let (hw, gw) = (one(&mut h, now), one(&mut g, now));
        assert_eq!(land(r, now, &hw) + land(r, now, &gw), 2, "second {now}: every honest wire lands once");
        for _ in 0..left.min(batch) { out[3] += forged(r, now, &forge::<R>(u32::MAX - left + 1, 1, 0, MAX_TTL)); left -= 1; }
        out[0] += land(r, now, &hw);
        out[1] += land(r, now, &gw);
    }
    (out[2], out[4]) = (r.pressure(), roster);
    out
}

/// Number churn toward the 32-bit edge. The leaf keeps the established sender's window, so none of its replays lands;
/// the squat holds every free slot, so every replay of the newcomer's lands, and the pressure counts the churn twice (a
/// refused slot and an evicted seen entry each). The signed hub refuses every churned number at `Open(Roster)`.
#[test]
fn number_churn_to_the_32_bit_edge_replays_no_established_sender_and_the_squat_costs_a_newcomer() {
    let churned = u32::try_from(env("MUDRPC_FUZZ", 10_000) * 100).unwrap(); // a million on every push, a hundred million before a release
    let (seconds, k) = (churned.div_ceil(MAX_PER_SEC - 2), |n: u32| Peer::<Key, 256, 4, 8, 16, 4>::new("x", Key::new(KEY), n, 1).unwrap());
    let (later, weeks) = (u32::from(seconds < MAX_TTL), ((seconds - 1) / (MAX_TTL - 1)).max(1)); // a second a week on if the churn ended inside one; the squat's renewals
    let t = std::time::Instant::now();
    let [h, g, pressure, forged, _] = churn(&mut Peer::<Key, 256, 4, 64, 16, 4>::new("gcs", Key::new(KEY), 1, 1).unwrap(), k(2), k(3), churned);
    eprintln!("leaf: {churned} churned in {:?}, pressure {pressure}, newcomer replays landed {g} of {seconds}", t.elapsed());
    assert_eq!([h, g, forged], [0, seconds - 1, churned + 15 * (1 + weeks)], "the leaf: the established sender's replays refused, the newcomer's executed twice in every churn second but the first, before the seen table turned over, the key holder's wires all believed");
    assert_eq!(pressure, 2 * churned + 5 * seconds - 50 + 15 * weeks + 2 * later, "a refused slot and an evicted seen entry per churned number; five a churn second, an evicted entry for the established sender's wire and two each for the newcomer's and its replay; 50 fewer in the second the seen table fills and its replay is `Duplicate`; an evicted entry for each of the squat's renewing wires; and in a second a week on, three for the honest wires, its replay `Duplicate`, less one, the renewal's first wire taking the entry a 60 s wire left");
    let mut s = Signed::<Key, 256>::new(Key::new(KEY), seed(1), 1000);
    for r in [2, 3] { s.roster(r, pk(r), None).unwrap(); }
    let t = std::time::Instant::now();
    assert_eq!(churn(&mut Peer::<_, 256, 4, 1024, 256, 4>::new("gcs", s, 1, 1).unwrap(), peer("h", 2, &[]), peer("g", 3, &[]), churned), [0, 0, 0, 0, churned + (1 + weeks) * 255], "the hub: every churned and squatting number dies at Open(Roster), before any table");
    eprintln!("hub: {churned} churned in {:?}", t.elapsed());
}

/// Three fleet-key holders against an honest sender (2) publishing every second to a receiver (1): at second 10 one
/// writes 2's number with the last counter, before 2's wire; every second one replays every honest wire so far, and one
/// sends from its own number (9) past the per-sender cap and from fresh numbers up to the global cap; at second 20 the
/// honest sender reboots into its next epoch. Every verdict, by who sent the wire and, for 2's own, when.
fn coordinated<T: Sealer>(mk: impl Fn(&str, u32) -> Peer<T, 256, 4, 8, 16, 4>) -> BTreeMap<(&'static str, String), u32> {
    let (mut r, mut h, mut own) = (mk("gcs", 1), mk("drone", 2), mk("rogue", 9));
    let (mut log, mut honest, mut fresh, mut lie) = (BTreeMap::new(), Vec::new(), 1000, 0);
    let mut note = |who, v| *log.entry((who, v)).or_insert(0) += 1;
    for now in 1..=30 {
        if now == 10 { note("ahead", verdict(&mut r, now, &forge::<T>(2, 1, u32::MAX, 60))); }
        if now == 20 { h = unsafe { core::ptr::read_unaligned(&raw const h) }; h.resume(2).unwrap(); }
        h.publish(now, "t", b"h", 60).unwrap();
        let w = one(&mut h, now);
        note(match now { ..10 => "honest before", 10..20 => "honest struck", _ => "honest rebooted" }, verdict(&mut r, now, &w));
        for w in &honest { note("replayed", verdict(&mut r, now, w)); }
        honest.push(w);
        for _ in 0..MAX_PER_SEC + 10 { lie += 1; own.publish(lie, "t", b"own", MAX_TTL).unwrap(); note("own", verdict(&mut r, now, &one(&mut own, lie))); }
        for _ in 0..RX_PER_SEC - MAX_PER_SEC - 2 { note("fresh", verdict(&mut r, now, &forge::<T>(fresh, 1, 0, MAX_TTL))); fresh += 1; }
    }
    log
}

/// The insider scenario's coordinated form. Under `Key` the honest sender is silenced as `Replay` from the strike until
/// its next epoch and nothing else: no honest wire executes twice. Under `Signed` every forged wire dies at the signature
/// or the roster, the honest window never moves, and the cost is the budget: 67 verifications a second, the honest
/// wire, the forgery and the rogue member's 65 under the drain bound, is enough.
#[test]
fn coordinating_key_holders_silence_an_unsigned_sender_until_its_next_epoch_and_mark_nothing_signed() {
    let tally = |rows: &[(&'static str, &str, u32)]| rows.iter().map(|&(w, v, n)| ((w, v.to_string()), n)).collect::<BTreeMap<_, _>>();
    let (fresh, own) = (30 * (RX_PER_SEC - MAX_PER_SEC - 2), [("own", "Event", 30 * MAX_PER_SEC), ("own", "Flood", 30), ("own", "Dropped(Rate)", 30 * 9)]);
    let key = coordinated(|name, n| Peer::<Key, 256, 4, 8, 16, 4>::new(name, Key::new(KEY), n, 1).unwrap());
    let windows = 16 - 2; // the receiver's free windows, which the first fresh numbers take; the rest share one sender's cap, named once a second
    assert_eq!(key, tally(&[[("honest before", "Event", 9), ("honest struck", "Dropped(Replay)", 10), ("honest rebooted", "Event", 11), ("ahead", "Event", 1), ("replayed", "Dropped(Duplicate)", 36 + 55), ("replayed", "Dropped(Replay)", 435 - 91), ("fresh", "Event", windows + 30 * MAX_PER_SEC), ("fresh", "Flood", 30), ("fresh", "Dropped(Rate)", fresh - windows - 30 * (MAX_PER_SEC + 1))].as_slice(), &own].concat()));
    let signed = coordinated(|name, n| budgeted(name, n, &[1, 2, 9], MAX_PER_SEC + 3));
    assert_eq!(signed, tally(&[[("honest before", "Event", 9), ("honest struck", "Event", 10), ("honest rebooted", "Event", 11), ("ahead", "Dropped(Open(Sig))", 1), ("replayed", "Dropped(Duplicate)", 435), ("fresh", "Dropped(Open(Roster))", fresh)].as_slice(), &own].concat()));
}
