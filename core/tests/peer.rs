//! The peer; the contract is rpc.rs's.
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
    clippy::many_single_char_names, // test scripts: `a` and `b` are the peers, `w` a wire, `e` an envelope
)]
use mudrpc_core::envelope::{Body, Envelope, Id};
use mudrpc_core::rpc::{Call, Counts, Dropped, Error, Incoming, Outgoing, Peer, MAX_PER_SEC, MAX_SEQ, MAX_TOPIC, MAX_TTL, RX_PER_SEC, SEND_PER_SEC, VERSION, WINDOW};
use mudrpc_core::seal::{self, Key, Opened, Sealer, Slot};

/// Senders the test peer keeps a window for.
const SENDERS: usize = 16;
mod support;
use support::{from_block, peer, KEY, N};

struct Out { listen: bool, topic: String, wire: Vec<u8>, ttl: u32 }

fn drain<const SEEN: usize, const SENDERS: usize>(n: &mut Peer<Key, 256, 4, SEEN, SENDERS, 4>, now: u32) -> Vec<Out> {
    let mut v = Vec::new();
    while let Some(o) = n.outgoing(now) {
        v.push(match o {
            Outgoing::Listen { topic } => Out { listen: true, topic: topic.into(), wire: vec![], ttl: 0 },
            Outgoing::Send { topic, wire, ttl_secs } => Out { listen: false, topic: topic.into(), wire: wire.into(), ttl: ttl_secs },
        });
        n.sent();
    }
    v
}

/// Our own output must be valid: version byte, opens under the key with the topic, decodes. Returns the nonce.
fn valid(o: &Out) -> [u8; 12] {
    assert_eq!(o.wire[0], VERSION);
    let mut ad = vec![VERSION];
    ad.extend_from_slice(o.topic.as_bytes());
    let mut plain = [0u8; 256];
    let opened = Key::new(KEY).open(&ad, &o.wire[1..], &mut plain).expect("our wire opens");
    let e = Envelope::decode(&plain[..opened.len]).expect("our wire decodes");
    assert_eq!(e.ttl_secs, o.ttl);
    opened.nonce
}

fn send(n: &mut N, now: u32) -> Out {
    let mut v: Vec<Out> = drain(n, now).into_iter().filter(|o| !o.listen).collect();
    assert_eq!(v.len(), 1, "expected exactly one send");
    valid(&v[0]);
    v.pop().unwrap()
}

/// The verdict on a wire, when it is a drop.
fn dropped<const SEEN: usize, const SENDERS: usize>(n: &mut Peer<Key, 256, 4, SEEN, SENDERS, 4>, now: u32, o: &Out) -> Option<Dropped> {
    match n.deliver(now, &o.topic, &o.wire, &mut [0u8; 256]) { Incoming::Dropped(d) => Some(d), _ => None }
}

fn call<'a>(n: &mut N, now: u32, o: &'a Out, buf: &'a mut [u8]) -> Call<'a> {
    match n.deliver(now, &o.topic, &o.wire, buf) {
        Incoming::Call(c) => c,
        other => panic!("expected a call, got {other:?}"),
    }
}

#[test]
fn new_peer_listens_on_its_own_inbox_and_nothing_else() {
    let mut a = peer("gcs", 1);
    let out = drain(&mut a, 0);
    assert_eq!(out.len(), 1);
    assert!(out[0].listen);
    assert_eq!(out[0].topic, "gcs");
    assert!(drain(&mut a, 0).is_empty());
    assert_eq!(a.tick(u32::MAX), None);
}

#[test]
fn unary_call_and_reply_round_trip() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    let id = a.call(0, "nav", "Arm", b"\x08\x01", 30).unwrap();
    let o = send(&mut a, 0);
    assert_eq!((o.topic.as_str(), o.ttl), ("nav", 30));
    let mut buf = [0u8; 256];
    let c = call(&mut b, 1, &o, &mut buf);
    assert_eq!((c.id, c.topic, c.method, c.reply_to, c.ttl_secs, c.seq, c.end, c.bytes), (id, "nav", "Arm", "gcs", 30, 0, true, &b"\x08\x01"[..]));
    b.reply(1, &c, 0, 0, "", b"ok", true).unwrap();
    let r = send(&mut b, 1);
    assert_eq!((r.topic.as_str(), r.ttl), ("gcs", 30));
    let mut buf = [0u8; 256];
    assert_eq!(a.deliver(2, &r.topic, &r.wire, &mut buf), Incoming::Reply { id, sender: 2, seq: 0, end: true, status: 0, detail: "", bytes: b"ok" });
    assert_eq!(a.tick(1000), None, "resolved call is no longer pending");
}

#[test]
fn a_reply_carries_its_status_and_detail_and_one_past_the_wire_is_refused_whole() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    let id = a.call(0, "nav", "Goto", b"", 30).unwrap();
    let o = send(&mut a, 0);
    let mut buf = [0u8; 256];
    let c = call(&mut b, 1, &o, &mut buf);
    let queued = b.queued();
    assert_eq!(b.reply(1, &c, 0, 5, &"x".repeat(500), b"", true), Err(Error::TooLarge));
    assert_eq!(b.queued(), queued, "nothing kept of a refusal");
    b.reply(1, &c, 0, 5, "no such waypoint", b"", true).unwrap();
    let r = send(&mut b, 1);
    let mut buf = [0u8; 256];
    assert_eq!(a.deliver(2, &r.topic, &r.wire, &mut buf), Incoming::Reply { id, sender: 2, seq: 0, end: true, status: 5, detail: "no such waypoint", bytes: b"" });
}

#[test]
fn expired_call_reports_once_and_late_reply_is_dropped() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    let id = a.call(0, "nav", "Arm", b"", 10).unwrap();
    let o = send(&mut a, 0);
    assert_eq!(a.tick(9), None);
    assert_eq!(a.tick(10), Some(Incoming::Expired { id }));
    assert_eq!(a.tick(10), None);
    let mut buf = [0u8; 256];
    let c = call(&mut b, 0, &o, &mut buf);
    b.reply(0, &c, 0, 0, "", b"late", true).unwrap();
    let r = send(&mut b, 0);
    assert_eq!(a.deliver(11, &r.topic, &r.wire, &mut [0u8; 256]), Incoming::Dropped(Dropped::NoPending));
}

#[test]
fn redelivered_call_executes_once() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    a.call(0, "nav", "Arm", b"", 60).unwrap();
    let o = send(&mut a, 0);
    let mut buf = [0u8; 256];
    assert!(matches!(b.deliver(1, &o.topic, &o.wire, &mut buf), Incoming::Call(_)));
    assert_eq!(b.deliver(2, &o.topic, &o.wire, &mut buf), Incoming::Dropped(Dropped::Duplicate));
    assert_eq!(b.deliver(3, &o.topic, &o.wire, &mut buf), Incoming::Dropped(Dropped::Duplicate));
}

#[test]
fn two_servers_first_reply_wins_and_both_remember_the_call() {
    let (mut a, mut b, mut c) = (peer("gcs", 1), peer("d1", 2), peer("d2", 3));
    let id = a.call(0, "nav", "Arm", b"", 60).unwrap();
    let o = send(&mut a, 0);
    let (mut bb, mut cb) = ([0u8; 256], [0u8; 256]);
    let (cb_, cc_) = (call(&mut b, 1, &o, &mut bb), call(&mut c, 1, &o, &mut cb));
    b.reply(1, &cb_, 0, 0, "", b"b", true).unwrap();
    c.reply(1, &cc_, 0, 0, "", b"c", true).unwrap();
    let (rb, rc) = (send(&mut b, 1), send(&mut c, 1));
    let mut buf = [0u8; 256];
    assert_eq!(a.deliver(2, &rc.topic, &rc.wire, &mut buf), Incoming::Reply { id, sender: 3, seq: 0, end: true, status: 0, detail: "", bytes: b"c" });
    assert_eq!(a.deliver(2, &rb.topic, &rb.wire, &mut buf), Incoming::Dropped(Dropped::NoPending), "the call is resolved; a second answer has nowhere to land");
    assert_eq!(b.deliver(3, &o.topic, &o.wire, &mut buf), Incoming::Dropped(Dropped::Duplicate), "b has seen the call");
    assert_eq!(c.deliver(3, &o.topic, &o.wire, &mut buf), Incoming::Dropped(Dropped::Duplicate), "c has seen the call");
}

#[test]
fn pending_full_refuses_without_touching_existing_calls() {
    let mut a = peer("gcs", 1);
    drain(&mut a, 0);
    let ids: Vec<Id> = (0..4).map(|_| { let id = a.call(0, "nav", "M", b"", 10).unwrap(); drain(&mut a, 0); id }).collect();
    assert_eq!(a.call(0, "nav", "M", b"", 10), Err(Error::Full));
    let mut expired: Vec<Id> = (0..4).map(|_| match a.tick(10) { Some(Incoming::Expired { id }) => id, o => panic!("{o:?}") }).collect();
    expired.sort_by_key(|i| i.0);
    let mut want = ids;
    want.sort_by_key(|i| i.0);
    assert_eq!(expired, want);
    assert_eq!(a.tick(10), None);
}

#[test]
fn outbox_full_refuses_every_producer() {
    let mut a = peer("gcs", 1);
    drain(&mut a, 0);
    for _ in 0..4 { a.publish(0, "t", b"x", 10).unwrap(); }
    assert_eq!(a.publish(0, "t", b"x", 10), Err(Error::Full));
    assert_eq!(a.listen("u"), Err(Error::Full));
    assert_eq!(a.call(0, "nav", "M", b"", 10), Err(Error::Full));
    assert_eq!(drain(&mut a, 0).len(), 4);
    assert_eq!(a.tick(100), None, "a refused call left no pending entry");
}

#[test]
fn the_window_outlives_seen_eviction_and_only_sender_eviction_lets_a_call_execute_twice() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    let mut wires = Vec::new();
    for _ in 0..9 {
        a.call(0, "nav", "M", b"", 3600).unwrap();
        wires.push(send(&mut a, 0));
        a.tick(3600);
    }
    let mut buf = [0u8; 256];
    for w in &wires { assert!(matches!(b.deliver(1, &w.topic, &w.wire, &mut buf), Incoming::Call(_))); }
    assert_eq!(dropped(&mut b, 1, &wires[0]), Some(Dropped::Duplicate), "evicted from SEEN = 8, remembered by the sender's window");
    for s in 3..3 + SENDERS as u32 {
        let mut other = peer("x", s);
        other.publish(2, "nav", b"", 60).unwrap();
        let w = send(&mut other, 2);
        assert_eq!(dropped(&mut b, 1 + MAX_TTL, &w), None);
    }
    assert!(matches!(b.deliver(2 + MAX_TTL, &wires[0].topic, &wires[0].wire, &mut buf), Incoming::Call(_)), "the cost of SENDERS = 16: sender 1, silent a week, forgotten, and the call held past its ttl executes twice");
}

/// Thirty-nine drones publish twice to a topic one receiver listens on, all inside a ttl; then a ciphertext holder replays
/// every wire. The senders whose replay executed, and the receiver's pressure.
fn replay_after_fleet<const SEEN: usize, const SENDERS: usize>() -> (Vec<u32>, u32) {
    let mut gcs = Peer::<Key, 256, 4, SEEN, SENDERS, 4>::new("gcs", Key::new(KEY), 1, 2).unwrap();
    gcs.listen("telemetry").unwrap();
    drain(&mut gcs, 0);
    let mut drones: Vec<N> = (2..=40).map(|n| N::new(&format!("drone-{n}"), Key::new(KEY), n, 2).unwrap()).collect();
    let mut wires = Vec::new();
    for now in 1..3 { for d in &mut drones { d.publish(now, "telemetry", b"pos", 3600).unwrap(); wires.push(drain(d, now).pop().unwrap()); } }
    for w in &wires { assert!(matches!(gcs.deliver(3, &w.topic, &w.wire, &mut [0u8; 256]), Incoming::Event { .. })); }
    let twice = wires.iter().filter_map(|w| match gcs.deliver(4, &w.topic, &w.wire, &mut [0u8; 256]) { Incoming::Event { sender, .. } => Some(sender), Incoming::Dropped(Dropped::Duplicate) => None, other => panic!("a replay was {other:?}") }).collect();
    (twice, gcs.pressure())
}

/// The hub's tables hold a fleet: 256 windows and 1024 seen entries evict nothing for 39 senders' 78 wires, so every
/// replay is a duplicate. A leaf's 16 and 64, which were once the hub's, do not.
#[test]
fn a_hub_keeps_a_window_and_a_seen_entry_for_a_fleet_so_a_replay_without_the_key_is_a_duplicate() {
    assert_eq!(replay_after_fleet::<1024, 256>(), (vec![], 0), "the hub: nothing evicted, every replay refused");
    let (twice, pressure) = replay_after_fleet::<64, 16>();
    assert!(!twice.is_empty() && twice.iter().all(|&s| s > 17), "a leaf: drones 2 to 17 keep their windows; a later drone's wire past the seen table executes twice, no key needed: {twice:?}");
    assert!(pressure > 0, "and the leaf said so");
}

#[test]
fn server_stream_arrives_out_of_order_and_completes_on_count_not_on_end() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    let id = a.call(0, "cam", "Frames", b"", 300).unwrap();
    let o = send(&mut a, 0);
    let mut buf = [0u8; 256];
    let c = call(&mut b, 1, &o, &mut buf);
    let mut items = Vec::new();
    for seq in 0..6u32 {
        b.reply(1, &c, seq, 0, "", &[seq as u8], seq == 5).unwrap();
        items.push(send(&mut b, 1));
    }
    let mut got = Vec::new();
    for i in [5usize, 3, 1, 0, 2] {
        match a.deliver(2, &items[i].topic, &items[i].wire, &mut [0u8; 256]) {
            Incoming::Reply { seq, end, bytes, .. } => { assert_eq!(bytes, &[seq as u8]); assert_eq!(end, seq == 5); got.push(seq); }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(got, [5, 3, 1, 0, 2]);
    assert_eq!(a.tick(299), None, "end seen but item 4 missing: still pending");
    assert_eq!(a.deliver(3, &items[3].topic, &items[3].wire, &mut [0u8; 256]), Incoming::Dropped(Dropped::Duplicate), "duplicate item dropped");
    assert!(matches!(a.deliver(3, &items[4].topic, &items[4].wire, &mut [0u8; 256]), Incoming::Reply { seq: 4, .. }));
    assert_eq!(a.tick(300), None, "complete: pending removed, nothing to expire");
    assert_eq!(a.deliver(4, &items[2].topic, &items[2].wire, &mut [0u8; 256]), Incoming::Dropped(Dropped::NoPending), "after completion the call is gone");
    b.reply(4, &c, 6, 0, "", b"late", false).unwrap();
    let late = send(&mut b, 4);
    assert_eq!(a.deliver(5, &late.topic, &late.wire, &mut [0u8; 256]), Incoming::Dropped(Dropped::NoPending), "a new item after completion has no call to land on");
    assert_eq!(id, c.id);
}

#[test]
fn peer_state_survives_a_byte_copy_to_a_new_address_and_a_reboot_expires_what_was_in_flight() {
    const SIZE: usize = core::mem::size_of::<N>();
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    let id = a.call(0, "nav", "Arm", b"", 60).unwrap();
    let o = send(&mut a, 0);
    let mut buf = [0u8; 256];
    let c = call(&mut b, 1, &o, &mut buf);
    b.reply(1, &c, 0, 0, "", b"ok", true).unwrap();
    let r = send(&mut b, 1);
    b.publish(1, "gcs", b"evt", 60).unwrap();
    let ev = send(&mut b, 1);
    assert!(matches!(a.deliver(1, &ev.topic, &ev.wire, &mut buf), Incoming::Event { .. }));
    let mut flash = [0u8; SIZE];
    unsafe { core::ptr::copy_nonoverlapping((&raw const a).cast::<u8>(), flash.as_mut_ptr(), SIZE) };
    let _dead = a; // the original is gone; only the flash copy remains
    let mut restored: N = unsafe { core::ptr::read_unaligned(flash.as_ptr().cast::<N>()) };
    restored.resume(2).unwrap();
    assert_eq!(restored.tick(2), Some(Incoming::Expired { id }), "uptime restarted: the call in flight is expired by id on the first tick");
    assert_eq!(restored.deliver(2, &r.topic, &r.wire, &mut [0u8; 256]), Incoming::Dropped(Dropped::NoPending), "and its late reply finds no call");
    assert_eq!(dropped(&mut restored, 2, &ev), Some(Dropped::Duplicate), "what was seen is still seen");
    let out = drain(&mut restored, 2);
    assert!(out.len() == 1 && out[0].listen && out[0].topic == "gcs", "its own listen queued again, for a transport restarted with it");
}

#[test]
fn ttl_is_clamped_and_seen_eviction_prefers_the_soonest_to_expire() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    a.publish(0, "t", b"zero", 0).unwrap();
    let o = send(&mut a, 0);
    assert_eq!(o.ttl, 1, "ttl 0 becomes 1 so the message can leave");
    a.publish(0, "t", b"forever", u32::MAX).unwrap();
    assert_eq!(send(&mut a, 0).ttl, MAX_TTL);
    assert_eq!(b.pressure(), 0);
    for s in 3..3 + SENDERS as u32 { // sixteen others fill the sender table, so sender 1 has no window: the seen table alone
        let mut x = peer("x", s);
        x.publish(0, "t", b"", 1).unwrap();
        assert_eq!(dropped(&mut b, 0, &send(&mut x, 0)), None);
    }
    let before = b.pressure(); // sixteen through a seen table of eight
    let mut wires = Vec::new();
    for i in 0..8u32 {
        a.publish(0, "t", &[i as u8], 100 + i).unwrap();
        wires.push(send(&mut a, 0));
    }
    let mut buf = [0u8; 256];
    for w in &wires { assert!(matches!(b.deliver(1, &w.topic, &w.wire, &mut buf), Incoming::Event { .. })); }
    a.publish(0, "t", b"ninth", 100).unwrap();
    let ninth = send(&mut a, 0);
    assert!(matches!(b.deliver(1, &ninth.topic, &ninth.wire, &mut buf), Incoming::Event { .. }));
    assert_eq!(b.pressure() - before, 9 + 1, "nine refused a window, and a live seen entry had to go");
    assert!(matches!(b.deliver(20, &wires[0].topic, &wires[0].wire, &mut buf), Incoming::Event { .. }), "the soonest-expiring (ttl 100) was the one the seen table evicted");
    assert_eq!(dropped(&mut b, 20, &wires[7]), Some(Dropped::Duplicate), "the longest-lived is still remembered");
}

#[test]
fn a_replay_is_a_duplicate_inside_the_ttl_and_after_it() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    a.call(0, "nav", "Arm", b"", 10).unwrap();
    let o = send(&mut a, 0);
    let mut buf = [0u8; 256];
    assert!(matches!(b.deliver(0, &o.topic, &o.wire, &mut buf), Incoming::Call(_)));
    assert_eq!(dropped(&mut b, 5, &o), Some(Dropped::Duplicate));
    assert_eq!(dropped(&mut b, 9, &o), Some(Dropped::Duplicate));
    assert_eq!(dropped(&mut b, 10, &o), Some(Dropped::Duplicate), "after the ttl the seen table has let go; the sender's window has not");
    assert_eq!(dropped(&mut b, 10 + MAX_TTL, &o), Some(Dropped::Duplicate), "nor ever will, short of 64 newer wires from that sender");
}

#[test]
fn events_publish_once_and_dedupe() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    a.listen("telemetry").unwrap();
    b.publish(0, "telemetry", b"pos", 5).unwrap();
    let o = send(&mut b, 0);
    assert_eq!((o.topic.as_str(), o.ttl), ("telemetry", 5));
    let mut buf = [0u8; 256];
    assert_eq!(a.deliver(1, &o.topic, &o.wire, &mut buf), Incoming::Event { topic: "telemetry", sender: 2, bytes: b"pos" });
    let mut buf = [0xAAu8; 256];
    assert_eq!(a.deliver(2, &o.topic, &o.wire, &mut buf), Incoming::Dropped(Dropped::Duplicate));
    assert!(buf[..o.wire.len() - 29].iter().all(|&x| x == 0), "a wire refused after it opened leaves no plaintext in the buffer");
}

#[test]
fn expired_outgoing_dies_at_the_exit() {
    let mut a = peer("gcs", 1);
    drain(&mut a, 0);
    a.publish(0, "t", b"short", 5).unwrap();
    a.publish(0, "t", b"long", 100).unwrap();
    let out = drain(&mut a, 5);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].ttl, 100);
}

#[test]
fn oversize_and_overlong_are_refused_and_garbage_never_reaches_product() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    drain(&mut a, 0);
    assert_eq!(a.call(0, "nav", "M", &[0u8; 256], 10), Err(Error::TooLarge));
    assert_eq!(a.publish(0, "t", &[0u8; 230], 10), Err(Error::TooLarge));
    let long = "x".repeat(MAX_TOPIC + 1);
    assert_eq!(a.listen(&long), Err(Error::TooLarge));
    assert_eq!(a.call(0, &long, "M", b"", 10), Err(Error::TooLarge));
    assert!(N::new(&long, Key::new(KEY), 1, 1).is_err());
    assert!(drain(&mut a, 0).is_empty(), "refusals queue nothing");
    assert_eq!(a.tick(100), None, "refusals leave nothing pending");
    let mut buf = [0u8; 256];
    assert_eq!(b.deliver(0, "nav", &[0u8; 300], &mut buf), Incoming::Dropped(Dropped::Version(0)), "version is checked first");
    assert_eq!(b.deliver(0, "nav", &[1u8; 300], &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Full)), "too big for the scratch: refused before any tag work");
    assert_eq!(b.deliver(0, "nav", &[1u8; 100], &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Tag)));
    assert_eq!(b.deliver(0, "nav", b"", &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Short)));
    assert_eq!(b.deliver(0, "nav", &[1u8; 28], &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Short)), "27 sealed bytes cannot hold a tag");
    assert_eq!(b.deliver(0, "nav", &[1u8; 29], &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Tag)));
}

#[test]
fn wrong_key_or_wrong_topic_never_reaches_product() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    let mut other = N::new("spy", Key::new([8; 32]), 9, 1).unwrap();
    a.call(0, "nav", "Arm", b"", 60).unwrap();
    let o = send(&mut a, 0);
    let mut buf = [0u8; 256];
    assert_eq!(other.deliver(1, &o.topic, &o.wire, &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Tag)));
    assert_eq!(b.deliver(1, "other-topic", &o.wire, &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Tag)));
    assert!(matches!(b.deliver(1, &o.topic, &o.wire, &mut buf), Incoming::Call(_)));
}

#[test]
fn another_protocol_version_is_named_not_misparsed() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    a.call(0, "nav", "Arm", b"", 60).unwrap();
    let mut o = send(&mut a, 0);
    o.wire[0] = 2;
    let mut buf = [0u8; 256];
    assert_eq!(b.deliver(1, &o.topic, &o.wire, &mut buf), Incoming::Dropped(Dropped::Version(2)));
    assert_eq!(b.deliver(1, &o.topic, &[], &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Short)));
}

#[test]
fn outgoing_is_a_peek_until_sent() {
    let mut a = peer("gcs", 1);
    assert!(matches!(a.outgoing(0), Some(Outgoing::Listen { topic: "gcs" })));
    assert!(matches!(a.outgoing(0), Some(Outgoing::Listen { topic: "gcs" })), "still there: the transport may have refused it");
    a.sent();
    assert_eq!(a.outgoing(0), None);
    a.sent();
    assert_eq!(a.outgoing(0), None, "sent on an empty outbox is harmless");
    a.publish(0, "t", b"x", 10).unwrap();
    a.sent();
    assert!(matches!(a.outgoing(0), Some(Outgoing::Send { .. })), "sent with nothing shown is a no-op: the item is still there");
    a.sent();
    a.sent();
    assert_eq!(a.outgoing(0), None, "one shown, one taken; the second sent found nothing shown");
}

#[test]
fn a_peer_cannot_push_more_than_send_per_sec_into_the_mesh() {
    let mut a = peer("gcs", 1);
    drain(&mut a, 0);
    for _ in 0..SEND_PER_SEC - 4 { a.publish(5, "t", b"x", 10).unwrap(); drain(&mut a, 5); }
    for _ in 0..4 { a.publish(5, "t", b"x", 10).unwrap(); }
    assert_eq!(a.publish(5, "t", b"x", 10), Err(Error::Full), "the second spent with the outbox full: `Full`, which the drain clears");
    drain(&mut a, 5);
    assert_eq!(a.publish(5, "t", b"x", 10), Err(Error::Rate), "a compromised app on this host gets SEND_PER_SEC sends a second, not line rate");
    assert_eq!(a.call(5, "nav", "M", b"", 10), Err(Error::Rate), "the 34th too, and not `Full`: nothing is full");
    assert!(a.publish(6, "t", b"x", 10).is_ok(), "the next second starts fresh");
    assert_eq!(a.tick(1000), None, "refused sends left nothing pending");
}

/// A receiver's second need not line up with the sender's, so one of its seconds can take the end of one sender second and
/// the start of the next: an honest sender at its cap there lands every wire and is never named.
#[test]
fn a_sender_at_its_cap_across_its_second_edge_lands_whole_in_one_receiver_second() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    drain(&mut a, 0);
    let mut wires = Vec::new();
    for now in [5, 6] { while a.publish(now, "t", b"x", 10).is_ok() { wires.push(send(&mut a, now)); } }
    assert_eq!(wires.len() as u32, 2 * SEND_PER_SEC, "two seconds at the cap");
    assert!(wires.iter().all(|w| matches!(b.deliver(9, &w.topic, &w.wire, &mut [0u8; 256]), Incoming::Event { .. })), "every wire lands in the receiver's one second, none `Flood` or `Rate`");
}

#[test]
fn a_stream_longer_than_seen_still_completes_and_redelivery_is_a_duplicate() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    let id = a.call(0, "cam", "Frames", b"", 300).unwrap();
    let o = send(&mut a, 0);
    let mut buf = [0u8; 256];
    let c = call(&mut b, 1, &o, &mut buf);
    let mut items = Vec::new();
    for seq in 0..12u32 { b.reply(1, &c, seq, 0, "", &[seq as u8], seq == 11).unwrap(); items.push(send(&mut b, 1)); }
    assert_eq!(items.iter().map(valid).collect::<std::collections::HashSet<_>>().len(), 12, "each item its own nonce");
    assert!(matches!(a.deliver(2, &items[0].topic, &items[0].wire, &mut [0u8; 256]), Incoming::Reply { seq: 0, .. }));
    for (i, item) in items.iter().enumerate().skip(1) { assert!(matches!(a.deliver(2, &item.topic, &item.wire, &mut [0u8; 256]), Incoming::Reply { .. }), "item {i}"); }
    assert_eq!(a.tick(299), None, "complete: the pending call is gone");
    assert_eq!(a.deliver(3, &items[0].topic, &items[0].wire, &mut [0u8; 256]), Incoming::Dropped(Dropped::NoPending), "after completion nothing lands");
    let id2 = a.call(4, "cam", "Frames", b"", 300).unwrap();
    let o2 = send(&mut a, 4);
    let c2 = call(&mut b, 5, &o2, &mut buf);
    b.reply(5, &c2, 3, 0, "", b"x", false).unwrap();
    let three = send(&mut b, 5);
    assert!(matches!(a.deliver(6, &three.topic, &three.wire, &mut [0u8; 256]), Incoming::Reply { seq: 3, .. }));
    assert_eq!(a.deliver(6, &three.topic, &three.wire, &mut [0u8; 256]), Incoming::Dropped(Dropped::Duplicate), "the bitmap, not the seen table, dedupes stream items");
    assert_eq!(b.reply(5, &c2, MAX_SEQ, 0, "", b"x", false), Err(Error::TooLarge));
    let fake = Call { id: id2, sender: 1, topic: "cam", method: "Frames", reply_to: "gcs", ttl_secs: u32::MAX, seq: 0, end: true, bytes: b"" };
    b.reply(5, &fake, 0, 0, "", b"late", true).unwrap();
    assert_eq!(send(&mut b, 5).ttl, MAX_TTL, "a fabricated Call cannot make a 136-year reply");
    let _ = id;
}

#[test]
fn a_valid_but_oversize_wire_is_dropped_as_full_before_any_tag_work() {
    let mut big = Peer::<Key, 1024, 4, 8, 16, 4>::new("gcs", Key::new(KEY), 1, 1).unwrap();
    let mut small = peer("drone", 2);
    big.publish(0, "t", &[7u8; 600], 10).unwrap();
    let mut wire = Vec::new();
    while let Some(o) = big.outgoing(0) { if let Outgoing::Send { wire: w, .. } = o { wire = w.to_vec(); } big.sent(); }
    assert_eq!(small.deliver(1, "t", &wire, &mut [0u8; 256]), Incoming::Dropped(Dropped::Open(seal::Error::Full)));
}

#[test]
fn a_clock_that_goes_backwards_does_not_reset_the_send_bound_and_a_long_topic_is_named() {
    let mut a = peer("gcs", 1);
    drain(&mut a, 0);
    for _ in 0..SEND_PER_SEC { a.publish(5, "t", b"x", 10).unwrap(); drain(&mut a, 5); }
    assert_eq!(a.publish(4, "t", b"x", 10), Err(Error::Rate), "an earlier second does not open a new budget");
    assert!(a.publish(6, "t", b"x", 10).is_ok());
    let long = "x".repeat(MAX_TOPIC + 1);
    assert_eq!(a.deliver(6, &long, &[1u8; 40], &mut [0u8; 256]), Incoming::Dropped(Dropped::Topic));
}

/// A name is a topic and never empty: a peer named nothing would listen on nothing. Refused where a name or topic enters, as
/// `Empty`, a refused call leaving no pending entry; a wire on an empty topic, one that would open, is `Topic` before the tag,
/// and so is a call whose `reply_to` is empty.
#[test]
fn an_empty_name_or_topic_is_refused_and_a_wire_on_one_is_dropped_before_the_tag() {
    assert_eq!(N::new("", Key::new(KEY), 1, 1).err(), Some(Error::Empty));
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    drain(&mut a, 0);
    drain(&mut b, 0);
    assert_eq!((a.listen(""), a.publish(0, "", b"x", 10), a.call(0, "", "M", b"", 10)), (Err(Error::Empty), Err(Error::Empty), Err(Error::Empty)));
    assert_eq!((a.pending(), a.queued()), (0, 0), "nothing kept of a refusal");
    a.call(0, "nav", "M", b"", 10).unwrap();
    let (o, mut buf) = (send(&mut a, 0), [0u8; 256]);
    let mut c = call(&mut b, 1, &o, &mut buf);
    c.reply_to = "";
    assert_eq!(b.reply(1, &c, 0, 0, "", b"", true), Err(Error::Empty));
    let mut plain = [0u8; 128];
    let n = Envelope { id: None, ttl_secs: 30, seq: 0, end: false, body: Body::Event { payload: b"e" } }.encode(&mut plain).unwrap();
    let mut sealed = [0u8; 256];
    let m = Key::new(KEY).seal(&[9, 0, 0, 0, 1, 0, 0, 0, 5, 0, 0, 0], &[VERSION], &mut plain, n, &mut sealed).unwrap();
    let wire = [&[VERSION][..], &sealed[..m]].concat();
    assert!(Key::new(KEY).open(&[VERSION], &wire[1..], &mut [0u8; 256]).is_ok(), "the wire opens under the empty topic");
    assert_eq!(b.deliver(2, "", &wire, &mut [0u8; 256]), Incoming::Dropped(Dropped::Topic));
    let n = Envelope { id: None, ttl_secs: 30, seq: 0, end: true, body: Body::Call { method: "M", reply_to: "", payload: b"" } }.encode(&mut plain).unwrap();
    let m = Key::new(KEY).seal(&[9, 0, 0, 0, 1, 0, 0, 0, 6, 0, 0, 0], b"\x01drone", &mut plain, n, &mut sealed).unwrap();
    assert_eq!(b.deliver(2, "drone", &[&[VERSION][..], &sealed[..m]].concat(), &mut [0u8; 256]), Incoming::Dropped(Dropped::Topic), "a call nobody could answer");
}

/// The backpressure reads: `pending` and `queued` rise with `call`, `publish` and `reply`, fall with an answer, an `Expired`
/// and the drain, and reach the capacities at which `call` and every producer are `Full`; a wire past its ttl stays queued
/// until `outgoing` reaches it.
#[test]
fn pending_and_queued_rise_and_fall_with_calls_answers_expiry_and_the_drain() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    assert_eq!((a.pending(), a.queued()), (0, 1), "the peer's own listen");
    drain(&mut a, 0);
    drain(&mut b, 0);
    a.call(0, "drone", "M", b"", 10).unwrap();
    a.call(0, "drone", "M", b"", 5).unwrap();
    assert_eq!((a.pending(), a.queued()), (2, 2));
    let calls = drain(&mut a, 0);
    assert_eq!((a.pending(), a.queued()), (2, 0), "drained: sent, still in flight");
    let mut buf = [0u8; 256];
    let c = call(&mut b, 1, &calls[0], &mut buf);
    b.reply(1, &c, 0, 0, "", b"ok", true).unwrap();
    assert_eq!(b.queued(), 1);
    let r = drain(&mut b, 1);
    assert!(matches!(a.deliver(1, &r[0].topic, &r[0].wire, &mut [0u8; 256]), Incoming::Reply { .. }));
    assert_eq!(a.pending(), 1, "the answered call is freed");
    assert!(matches!(a.tick(5), Some(Incoming::Expired { .. })));
    assert_eq!(a.pending(), 0, "and the expired one");
    for _ in 0..4 { a.call(6, "drone", "M", b"", 10).unwrap(); drain(&mut a, 6); }
    assert_eq!((a.pending(), a.call(6, "drone", "M", b"", 10)), (4, Err(Error::Full)), "at PENDING, Full");
    for _ in 0..4 { a.publish(6, "t", b"x", 1).unwrap(); }
    assert_eq!((a.queued(), a.publish(6, "t", b"x", 1)), (4, Err(Error::Full)), "at OUT, Full");
    assert_eq!((a.queued(), a.outgoing(10).is_none(), a.queued()), (4, true, 0), "past their ttl, counted until the exit drops them");
}

// ─────────────────── identity (SPEC.md layer 1, the nonce) ──────────────────

fn nonce_of(o: &Out) -> (u32, u32, u32) {
    let n = valid(o);
    let u = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap());
    (u(&n[0..4]), u(&n[4..8]), u(&n[8..12]))
}

fn block_of(p: &N) -> Vec<u8> { unsafe { std::slice::from_raw_parts(std::ptr::from_ref(p).cast::<u8>(), core::mem::size_of::<N>()).to_vec() } }

#[test]
fn the_nonce_is_sender_epoch_counter_and_never_repeats_across_sends_and_resumes() {
    let mut a = N::new("gcs", Key::new(KEY), 7, 3).unwrap();
    drain(&mut a, 0);
    a.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce_of(&send(&mut a, 0)), (7, 3, 0));
    a.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce_of(&send(&mut a, 0)), (7, 3, 1));
    let mut b = N::new("drone", Key::new(KEY), 8, 3).unwrap();
    drain(&mut b, 0);
    b.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce_of(&send(&mut b, 0)), (8, 3, 0), "another sender, another nonce, same counter");
    let mut twin = N::new("gcs", Key::new(KEY), 7, 3).unwrap();
    drain(&mut twin, 0);
    twin.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce_of(&send(&mut twin, 0)), (7, 3, 0), "same roster line, same nonces: the roster's duty, documented");
    let flash = block_of(&a);
    let mut c = from_block(&flash);
    assert_eq!(c.resume(3), Err(Error::Epoch), "the same epoch would replay (7, 3, 2)");
    assert_eq!(c.resume(2), Err(Error::Epoch));
    c.resume(4).unwrap();
    c.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce_of(&send(&mut c, 0)), (7, 4, 0), "a new epoch, the counter restarts, nothing repeats");
    let mut d = from_block(&flash);
    d.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce_of(&send(&mut d, 0)), (7, 3, 2), "restored without resume: the nonce the live peer also emits next, which is why resume exists");
    a.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce_of(&send(&mut a, 0)), (7, 3, 2));
}

#[test]
fn a_corrupt_block_and_a_wrong_magic_are_refused_by_resume() {
    let a = peer("gcs", 1);
    let mut flash = block_of(&a);
    let mut garbage = vec![0xFFu8; flash.len()];
    garbage[..flash.len() / 2].copy_from_slice(&flash[..flash.len() / 2]);
    assert_eq!(from_block(&garbage).resume(2), Err(Error::Corrupt));
    flash[0] ^= 1;
    assert_eq!(from_block(&flash).resume(2), Err(Error::Corrupt), "another layout's block fails its magic");
}

#[test]
fn sender_zero_is_unprovisioned_and_renew_is_the_only_way_out_of_exhaustion() {
    let mut blank = N::new("image", Key::new(KEY), 0, 1).unwrap();
    assert!(matches!(blank.outgoing(0), Some(Outgoing::Listen { .. })), "it exists and listens");
    blank.sent();
    assert_eq!(blank.publish(0, "t", b"x", 10), Err(Error::Unprovisioned));
    assert_eq!(blank.call(0, "t", "M", b"", 10), Err(Error::Unprovisioned));
    assert_eq!(blank.tick(100), None, "a refused call left nothing pending");
    let mut gcs = peer("gcs", 1);
    gcs.publish(0, "image", b"hi", 10).unwrap();
    let o = send(&mut gcs, 0);
    assert!(matches!(blank.deliver(1, &o.topic, &o.wire, &mut [0u8; 256]), Incoming::Event { .. }), "and delivers");
    let mut a = peer("gcs", 1);
    assert_eq!(a.renew(1), Err(Error::Epoch), "renew must also climb");
    assert!(a.renew(2).is_ok());
    assert_eq!(a.renew(2), Err(Error::Epoch));
    drain(&mut a, 0);
    a.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce_of(&send(&mut a, 0)), (1, 2, 0));
}

#[test]
fn two_peers_from_identical_inputs_are_byte_identical_and_one_epoch_bit_apart_share_nothing() {
    let (mut a, mut b, mut c) = (N::new("gcs", Key::new(KEY), 5, 9).unwrap(), N::new("gcs", Key::new(KEY), 5, 9).unwrap(), N::new("gcs", Key::new(KEY), 5, 8).unwrap());
    for p in [&mut a, &mut b, &mut c] { drain(p, 0); p.call(0, "nav", "Ping", b"1", 30).unwrap(); }
    let (wa, wb, wc) = (send(&mut a, 0).wire, send(&mut b, 0).wire, send(&mut c, 0).wire);
    assert_eq!(wa, wb, "no RNG anywhere to differ");
    assert_ne!(wa[..13], wc[..13], "a different epoch, a different nonce");
}

#[test]
fn dedupe_keys_on_the_nonce_and_a_unary_call_with_a_wrong_id_field_is_dropped() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    a.publish(0, "nav", b"e", 30).unwrap();
    let o = send(&mut a, 0);
    let mut buf = [0u8; 256];
    assert!(matches!(b.deliver(1, &o.topic, &o.wire, &mut buf), Incoming::Event { .. }));
    assert_eq!(b.deliver(1, &o.topic, &o.wire, &mut buf), Incoming::Dropped(Dropped::Duplicate));
    let mut plain = [0u8; 128];
    let n = Envelope { id: Some(Id([3; 12])), ttl_secs: 30, seq: 0, end: true, body: Body::Call { method: "M", reply_to: "gcs", payload: b"" } }.encode(&mut plain).unwrap();
    let mut wire = vec![VERSION];
    let mut sealed = [0u8; 256];
    let m = Key::new(KEY).seal(&[9, 0, 0, 0, 1, 0, 0, 0, 5, 0, 0, 0], b"\x01nav", &mut plain, n, &mut sealed).unwrap();
    wire.extend_from_slice(&sealed[..m]);
    assert_eq!(b.deliver(2, "nav", &wire, &mut buf), Incoming::Dropped(Dropped::Id));
    a.call(0, "nav", "M", b"", 30).unwrap();
    let unary = send(&mut a, 0);
    // version 1 + header 12 + tag 16 + envelope: ttl 2 + end 2 + call (1+1 + method 3 + reply_to 5) = 44; an earlier design carried a 16-byte id (18 more) and a 12-byte nonce in place of the header
    assert_eq!(unary.wire.len(), 1 + 12 + 16 + 2 + 2 + 2 + 3 + 5, "a unary call carries no id field");
}

#[test]
fn the_per_sender_floor_bounds_replay_to_max_ttl_and_names_a_rollback() {
    let (mut a, mut b) = (N::new("gcs", Key::new(KEY), 1, 2).unwrap(), peer("drone", 2));
    drain(&mut a, 0);
    let mut wires = Vec::new();
    for i in 0..3u8 { a.publish(0, "nav", &[i], 30).unwrap(); wires.push(send(&mut a, 0)); }
    let mut buf = [0u8; 256];
    assert!(matches!(b.deliver(10, &wires[2].topic, &wires[2].wire, &mut buf), Incoming::Event { .. }), "counter 2 seen at uptime 10");
    assert!(matches!(b.deliver(11, &wires[0].topic, &wires[0].wire, &mut buf), Incoming::Event { .. }), "an older message within MAX_TTL of that sighting is legitimate");
    let late = 10 + MAX_TTL;
    assert_eq!(b.deliver(late, &wires[1].topic, &wires[1].wire, &mut buf), Incoming::Dropped(Dropped::Replay), "below the floor, after MAX_TTL: a replay, whatever the seen table remembers");
    a.publish(0, "nav", b"new", 30).unwrap();
    let fresh = send(&mut a, 0);
    assert!(matches!(b.deliver(late + 1, &fresh.topic, &fresh.wire, &mut buf), Incoming::Event { .. }), "above the floor: accepted");
    let mut rolled = N::new("gcs", Key::new(KEY), 1, 1).unwrap(); // the same roster line from an old image: epoch 1 again
    drain(&mut rolled, 0);
    rolled.publish(0, "nav", b"ghost", 30).unwrap();
    let ghost = send(&mut rolled, 0);
    assert_eq!(b.deliver(late + 2, &ghost.topic, &ghost.wire, &mut buf), Incoming::Rollback { sender: 1 }, "an epoch below the floor's epoch: a cloned or rolled-back device");
    let mut again = from_block(&block_of(&b));
    again.resume(2).unwrap();
    assert_eq!(again.deliver(5, &wires[1].topic, &wires[1].wire, &mut buf), Incoming::Dropped(Dropped::Replay), "the floor survived the restore");
    let (mut r, mut s, mut forger) = (peer("gcs", 1), peer("drone", 2), N::new("x", Key::new(KEY), 2, 5).unwrap()); // a key holder writes sender 2 at epoch 5
    let mut nav = |n: &mut N, now: u32| {
        n.publish(now, "nav", b"", 600).unwrap();
        let w = send(n, now);
        match r.deliver(now, &w.topic, &w.wire, &mut [0u8; 256]) { Incoming::Event { sender: 2, .. } => "event", Incoming::Rollback { sender: 2 } => "rollback", Incoming::Dropped(Dropped::Duplicate) => "duplicate", other => panic!("{other:?}") }
    };
    assert_eq!([nav(&mut s, 0), nav(&mut forger, 10)], ["event"; 2], "the forged wire runs as an event: epoch 5 first heard at 10");
    let mut got = vec![nav(&mut s, 610), nav(&mut s, 611)];
    for e in 2..=6 { s.renew(e).unwrap(); got.push(nav(&mut s, 700)); }
    assert_eq!(got, ["event", "rollback", "rollback", "rollback", "rollback", "duplicate", "event"], "silent from its ttl past epoch 5's first hearing until it writes an epoch above it, epoch 5 itself the forger's nonces");
    assert_eq!(r.counts().rollbacks, 4, "the operator's signal, each naming the honest number");
}

#[test]
fn rate_is_counted_after_the_tag_per_sender_and_globally() {
    let (mut a, mut b, mut c) = (peer("gcs", 1), peer("d1", 2), peer("d2", 3));
    let mut buf = [0u8; 256];
    a.publish(0, "nav", b"real", 30).unwrap();
    let real = send(&mut a, 0);
    assert!(matches!(b.deliver(0, &real.topic, &real.wire, &mut buf), Incoming::Event { .. }));
    for _ in 0..200 {
        let mut junk = real.wire.clone();
        junk[20] ^= 1;
        assert_eq!(b.deliver(0, &real.topic, &junk, &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Tag)));
        assert_eq!(b.deliver(0, &real.topic, &real.wire, &mut buf), Incoming::Dropped(Dropped::Duplicate));
    }
    a.publish(0, "nav", b"still fine", 30).unwrap();
    let fine = send(&mut a, 0);
    assert!(matches!(b.deliver(0, &fine.topic, &fine.wire, &mut buf), Incoming::Event { .. }), "A was not rate limited by someone else's noise");
    let mut got = (0, 0, 0);
    for i in 0..MAX_PER_SEC + 20 {
        let t = i / SEND_PER_SEC + 1; // A's own send bound is per second of its clock; move it along
        a.publish(t, "nav", b"f", 30).unwrap();
        let w = send(&mut a, t);
        match b.deliver(5, &w.topic, &w.wire, &mut buf) { Incoming::Event { .. } => got.0 += 1, Incoming::Dropped(Dropped::Rate) => got.1 += 1, Incoming::Flood { sender: 1 } => got.2 += 1, other => panic!("{other:?}") }
    }
    assert_eq!(got, (MAX_PER_SEC as usize, 19, 1), "accepted, dropped, one report");
    c.publish(0, "nav", b"c", 30).unwrap();
    let cw = send(&mut c, 0);
    assert!(matches!(b.deliver(5, &cw.topic, &cw.wire, &mut buf), Incoming::Event { .. }), "another sender in the same second is fine");
    let (mut accepted, mut named) = (0, Vec::new());
    for s in 100..100 + RX_PER_SEC + 30 {
        let mut r = N::new("r", Key::new(KEY), s, 1).unwrap();
        drain(&mut r, 0);
        r.publish(0, "nav", b"r", 30).unwrap();
        let w = send(&mut r, 0);
        match b.deliver(9, &w.topic, &w.wire, &mut buf) { Incoming::Event { .. } => accepted += 1, Incoming::Flood { sender } => named.push(sender), Incoming::Dropped(Dropped::Rate) => {}, other => panic!("{other:?}") }
    }
    let first = SENDERS as u32 - 2 + MAX_PER_SEC; // the table's fourteen free windows, then one sender's cap for every number with none
    assert_eq!((accepted, named), (first, vec![100 + first]), "rotating numbers fill the table, then share one cap, the 65th named by the number it wrote");
    assert!(b.pressure() > 0, "the sender table was full and said so");
}

/// Five senders at 51 a second and an honest sixth spend a receiver's `RX_PER_SEC`; past it each of the five is named on
/// its next wire as over its share, the cap over the six heard, then refused; the honest one is refused and never named.
#[test]
fn whoever_spends_the_global_second_is_named_as_over_its_share_and_the_next_second_starts_clean() {
    let clock = std::cell::Cell::new(0); // the senders' clocks lie forward, so their own caps never bite
    let wire = |x: &mut N| { clock.set(clock.get() + 1); x.publish(clock.get(), "nav", b"f", 60).unwrap(); send(x, clock.get()) };
    let (mut b, mut buf, mut six) = (peer("drone", 99), [0u8; 256], (1..=6).map(|s| peer("x", s)).collect::<Vec<N>>());
    for _ in 0..51 { for x in &mut six[..5] { assert_eq!(dropped(&mut b, 0, &wire(x)), None); } }
    assert_eq!(dropped(&mut b, 0, &wire(&mut six[5])), None, "the honest report, the 256th");
    for (s, x) in (1..).zip(&mut six[..5]) { let w = wire(x); assert_eq!(b.deliver(0, &w.topic, &w.wire, &mut buf), Incoming::Flood { sender: s }, "52 of a second six spent: over 256 / 6"); }
    for x in &mut six { for _ in 0..3 { assert_eq!(dropped(&mut b, 0, &wire(x)), Some(Dropped::Rate), "then refused, the honest one too, and never named"); } }
    assert_eq!((b.counts().floods, b.counts().dropped), (5, 18));
    for x in &mut six { assert_eq!(dropped(&mut b, 1, &wire(x)), None, "the next second starts clean"); }
    let (mut c, mut ten, mut got, mut named) = (peer("leaf", 99), (11..=20).map(|s| peer("x", s)).collect::<Vec<N>>(), (0, 0), Vec::new());
    for _ in 0..MAX_PER_SEC {
        for x in &mut ten {
            let w = wire(x);
            match c.deliver(5, &w.topic, &w.wire, &mut buf) { Incoming::Event { .. } => got.0 += 1, Incoming::Flood { sender } => named.push(sender), Incoming::Dropped(Dropped::Rate) => got.1 += 1, other => panic!("{other:?}") }
        }
    }
    named.sort_unstable();
    assert_eq!(named, (11..=20).collect::<Vec<_>>(), "ten at their caps: each named once");
    assert_eq!(got, (RX_PER_SEC, 10 * MAX_PER_SEC - RX_PER_SEC - 10), "the cap accepted, the rest refused");
}

/// A fleet's events spend the global cap; the answers to the receiver's own calls are outside it, bounded by `PENDING` ×
/// `MAX_SEQ`, and count against their sender's window where it has one and in `pressure` where it has none.
#[test]
fn the_answers_to_our_own_calls_land_in_a_second_full_of_events_and_still_count_against_their_sender_where_it_has_a_window() {
    let (mut a, mut b) = (peer("gcs", 1), peer("d1", 2));
    let calls: Vec<Out> = ["d1", "d1", "d2", "d3"].iter().map(|s| { a.call(0, s, "Get", b"", 60).unwrap(); send(&mut a, 0) }).collect();
    let (mut buf, mut answers) = ([0u8; 256], Vec::new());
    b.publish(0, "nav", b"up", 60).unwrap();
    let up = send(&mut b, 0);
    assert!(matches!(a.deliver(0, &up.topic, &up.wire, &mut buf), Incoming::Event { sender: 2, .. }), "gcs keeps d1 a window, so its rate is counted");
    let stream = call(&mut b, 1, &calls[0], &mut buf);
    for seq in 0..MAX_SEQ { let t = 1 + seq / SEND_PER_SEC; b.reply(t, &stream, seq, 0, "", b"s", seq == MAX_SEQ - 1).unwrap(); answers.push(send(&mut b, t)); }
    let unary = call(&mut b, 3, &calls[1], &mut buf);
    b.reply(3, &unary, 0, 0, "", b"u", true).unwrap();
    let sixty_fifth = send(&mut b, 3);
    for (s, i) in [(3, 2), (4, 3), (5, 3)] {
        let mut server = peer("d", s);
        let k = call(&mut server, 0, &calls[i], &mut buf);
        server.reply(0, &k, 0, 0, "", b"r", true).unwrap();
        answers.push(send(&mut server, 0));
    }
    let late = answers.pop().unwrap();
    let mut fleet: Vec<N> = (100..113).map(|s| peer("x", s)).collect(); // with d1 and the two servers that answer first, gcs's sixteen windows
    let mut spend = |t: u32| -> Vec<Out> { (0..RX_PER_SEC as usize).map(|i| { let x = &mut fleet[i % 13]; x.publish(t, "nav", &[1], 60).unwrap(); send(x, t) }).collect() };
    let stranger = |s: u32, t: u32| { let mut x = peer("x", s); x.publish(t, "nav", &[1], 60).unwrap(); send(&mut x, t) };
    for w in &spend(9) { assert!(matches!(a.deliver(9, &w.topic, &w.wire, &mut buf), Incoming::Event { .. })); }
    for w in &answers { assert!(matches!(a.deliver(9, &w.topic, &w.wire, &mut buf), Incoming::Reply { .. }), "every matched answer lands in the second the fleet spent"); }
    assert_eq!(dropped(&mut a, 9, &late), Some(Dropped::NoPending), "an answer to a resolved call is no answer of ours");
    assert_eq!(dropped(&mut a, 9, &stranger(200, 9)), Some(Dropped::Rate), "the global cap still holds for events");
    assert_eq!(a.deliver(9, &sixty_fifth.topic, &sixty_fifth.wire, &mut buf), Incoming::Flood { sender: 2 }, "the 65th answer from one sender in a second");
    assert!(matches!(a.deliver(10, &sixty_fifth.topic, &sixty_fifth.wire, &mut buf), Incoming::Reply { sender: 2, .. }), "unmarked, it lands a second later");
    assert_eq!((a.counts().floods, a.counts().dropped), (1, 2));
    let calls: Vec<Out> = (0..4).map(|_| { a.call(20, "srv", "Get", b"", 60).unwrap(); send(&mut a, 20) }).collect();
    let mut items = Vec::new(); // gcs's sixteen windows are senders heard this week: none for a key holder's fresh number on each item
    for (t, c) in (0..).zip(&calls) {
        let k = call(&mut peer("srv", 6), t, c, &mut buf);
        for seq in 0..MAX_SEQ { let mut x = peer("x", 2000 + t * MAX_SEQ + seq); x.reply(t, &k, seq, 0, "", b"s", seq == MAX_SEQ - 1).unwrap(); items.push(send(&mut x, t)); }
    }
    let mut fleet = spend(21);
    let last = fleet.pop().unwrap();
    for w in &fleet { assert!(matches!(a.deliver(21, &w.topic, &w.wire, &mut buf), Incoming::Event { .. })); }
    let lands = |a: &mut N, w: &Out| {
        let p = a.pressure();
        assert!(matches!(a.deliver(21, &w.topic, &w.wire, &mut [0u8; 256]), Incoming::Reply { .. }), "no window and outside the strangers' cap: the pending calls × MAX_SEQ bound them");
        assert_eq!(a.pressure() - p, 1, "the trace of a slotless answer");
    };
    let (before, after) = items.split_at(items.len() / 2);
    for w in before { lands(&mut a, w); }
    assert!(matches!(a.deliver(21, &last.topic, &last.wire, &mut buf), Incoming::Event { .. }), "none spent the global count, which the fleet's last event fills");
    for w in after { lands(&mut a, w); }
    assert_eq!(dropped(&mut a, 21, &stranger(201, 21)), Some(Dropped::Rate), "the second the second half landed in was full");
}

/// A number with no window and none to take shares one sender's cap with every other such number: at a full leaf one
/// number lands `MAX_PER_SEC` calls and events a second, is `Flood` under the number it wrote on the next, then `Rate`,
/// and the senders with windows keep `RX_PER_SEC` − `MAX_PER_SEC` of the second; an honest newcomer waiting for a slot
/// shares the strangers' cap, the stated cost, and lands the next second. The cap counts what landed, so in a second the
/// windows spent, 65 newcomers are `Rate` and none is named.
#[test]
fn a_number_with_no_window_shares_one_cap_with_every_other_and_is_named_by_the_number_it_wrote() {
    let wire = |x: &mut N, t: u32| { x.publish(t, "nav", b"f", 60).unwrap(); send(x, t) };
    let (mut r, mut buf, mut got) = (peer("gcs", 1), [0u8; 256], (0, 0, 0));
    let mut honest: Vec<N> = (2..2 + SENDERS as u32).map(|s| peer("h", s)).collect();
    for x in &mut honest { assert_eq!(dropped(&mut r, 100, &wire(x, 0)), None, "sixteen windows, every one heard"); }
    let mut k = peer("k", 999);
    for i in 0..400 {
        let w = wire(&mut k, i / SEND_PER_SEC); // its own send cap is per second of its clock
        match r.deliver(101, &w.topic, &w.wire, &mut buf) { Incoming::Event { sender: 999, .. } => got.0 += 1, Incoming::Flood { sender: 999 } => got.1 += 1, Incoming::Dropped(Dropped::Rate) => got.2 += 1, other => panic!("{other:?}") }
    }
    assert_eq!(got, (MAX_PER_SEC, 1, 400 - MAX_PER_SEC - 1), "one sender's cap, named once by the number it wrote");
    for x in &mut honest[..3] { for i in 0..MAX_PER_SEC { assert_eq!(dropped(&mut r, 101, &wire(x, 1 + i / SEND_PER_SEC)), None, "the windows keep the rest of the second"); } }
    assert_eq!(dropped(&mut r, 101, &wire(&mut honest[3], 1)), Some(Dropped::Rate), "and the second is spent");
    let mut newcomer = peer("n", 500);
    let w = wire(&mut newcomer, 0);
    assert_eq!(dropped(&mut r, 101, &w), Some(Dropped::Rate), "an honest newcomer with no slot shares the strangers' cap");
    assert_eq!(dropped(&mut r, 102, &w), None, "and lands in the next second");
    for _ in 0..RX_PER_SEC / SENDERS as u32 { for x in &mut honest { assert_eq!(dropped(&mut r, 103, &wire(x, 3)), None, "the windows spend a second"); } }
    for s in 600..=600 + MAX_PER_SEC { assert_eq!(dropped(&mut r, 103, &wire(&mut peer("n", s), 0)), Some(Dropped::Rate), "the strangers' cap counts what landed: none did, so nobody is named"); }
    assert_eq!(r.counts().floods, 1);
}

/// Kademlia's rule against the longest-silent one it replaced: sixteen honest senders fill the table, sender 2 quiet since
/// its first wire, then a key holder cycles fresh numbers inside the global cap.
#[test]
fn established_senders_are_not_evicted_for_newcomers_and_a_week_of_silence_frees_a_slot() {
    let mut b = Peer::<Key, 256, 4, 2048, SENDERS, 4>::new("gcs", Key::new(KEY), 1, 1).unwrap();
    let one = |s: u32, ttl: u32| { let mut x = peer("x", s); x.publish(0, "t", b"", ttl).unwrap(); send(&mut x, 0) };
    let honest: Vec<Out> = (2..2 + SENDERS as u32).map(|s| one(s, 1)).collect();
    assert_eq!(dropped(&mut b, 0, &honest[0]), None, "the quiet one, heard once");
    for w in &honest[1..] { assert_eq!(dropped(&mut b, 1000, w), None); }
    let mut old: Vec<(u32, u32)> = (2..2 + SENDERS as u32).map(|s| (s, if s == 2 { 0 } else { 1000 })).collect(); // the control: (sender, last heard)
    let strangers: Vec<Out> = (0..1000).map(|i| one(10_000 + i, 60)).collect();
    for (i, w) in (0..).zip(&strangers) {
        let now = 1001 + i / MAX_PER_SEC; // every number with no window shares one sender's cap
        assert_eq!(dropped(&mut b, now, w), None, "a stranger's wire is accepted once");
        if let Some(o) = old.iter_mut().min_by_key(|s| s.1) { *o = (10_000 + i, now); }
        assert!(i > 0 || !old.iter().any(|s| s.0 == 2), "the longest-silent rule evicts the quiet honest sender at the first stranger");
    }
    assert!(old.iter().all(|s| s.0 >= 10_000), "and every honest sender by the sixteenth");
    assert_eq!(b.pressure(), 1000, "one refused slot per stranger, nothing evicted");
    assert_eq!(dropped(&mut b, 1020, &strangers[0]), Some(Dropped::Duplicate), "a stranger's replay inside its ttl: the seen table");
    for w in &honest { assert_eq!(dropped(&mut b, 1020, w), Some(Dropped::Duplicate), "every honest window kept; the seen table forgot these at their ttl"); }
    let late = one(20_000, 1);
    assert_eq!(dropped(&mut b, MAX_TTL, &late), None);
    assert_eq!(b.pressure(), 1001, "the table was full: a slot taken, counted");
    assert_eq!(dropped(&mut b, MAX_TTL + 2, &late), Some(Dropped::Duplicate), "the newcomer has a window");
    assert_eq!(dropped(&mut b, MAX_TTL + 2, &honest[1]), Some(Dropped::Duplicate), "the senders heard at 1000 kept theirs");
    assert_eq!(dropped(&mut b, MAX_TTL + 2, &honest[0]), None, "the quiet one's, a week silent, was the slot taken: its wire held past its ttl is a stranger's");
    let (boot, pressure) = (2 * MAX_TTL, b.pressure()); // a clock that survives the reboot, every sender a week silent on it
    b.resume(2).unwrap();
    let newcomer = one(30_000, 1);
    assert_eq!(dropped(&mut b, boot, &newcomer), None);
    assert_eq!(b.pressure(), pressure + 1, "the table full: counted");
    let mut x = N::new("x", Key::new(KEY), 3, 2).unwrap();
    x.publish(0, "t", b"", 1).unwrap();
    assert_eq!(dropped(&mut b, boot + 1, &send(&mut x, 0)), None, "an established sender's new wire lands");
    for w in honest[1..].iter().chain([&late]) { assert_eq!(dropped(&mut b, boot + 2, w), Some(Dropped::Duplicate), "no window evicted: silence counts from the first call after `resume`"); }
    let early = one(30_001, 1);
    assert_eq!(dropped(&mut b, boot + MAX_TTL - 1, &early), None, "a second short of a week after that call: no slot, so it runs on the seen table");
    assert_eq!(dropped(&mut b, boot + MAX_TTL, &early), None, "a week after it, no window and its seen entry gone: it runs again and takes the slot unheard since the reboot");
    assert_eq!(dropped(&mut b, boot + MAX_TTL + 1, &early), Some(Dropped::Duplicate), "now in that window");
    assert_eq!(dropped(&mut b, boot + MAX_TTL + 1, &late), None, "and it was the one the newcomer took");
}

/// Like `send`, but validates under a different key (the peer has rotated).
fn send_under(n: &mut N, now: u32, key: [u8; 32]) -> Out {
    let mut v: Vec<Out> = drain(n, now).into_iter().filter(|o| !o.listen).collect();
    assert_eq!(v.len(), 1);
    let o = v.pop().unwrap();
    let mut ad = vec![VERSION];
    ad.extend_from_slice(o.topic.as_bytes());
    assert!(Key::new(key).open(&ad, &o.wire[1..], &mut [0u8; 256]).is_ok());
    o
}

#[test]
fn rotation_reports_which_key_opened_and_leaves_the_epoch_alone() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    a.publish(0, "nav", b"old", 30).unwrap();
    let old = send(&mut a, 0);
    a.sealer_mut().stage([9; 32]);
    a.sealer_mut().rotate();
    a.publish(0, "nav", b"new", 30).unwrap();
    let new = send_under(&mut a, 0, [9; 32]);
    let mut buf = [0u8; 256];
    assert_eq!(b.deliver(1, &new.topic, &new.wire, &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Tag)), "B has not staged the new key");
    b.sealer_mut().stage([9; 32]);
    assert!(matches!(b.deliver(1, &new.topic, &new.wire, &mut buf), Incoming::Event { .. }));
    assert_eq!(b.opened(), Slot::Next);
    assert!(matches!(b.deliver(1, &old.topic, &old.wire, &mut buf), Incoming::Event { .. }));
    assert_eq!(b.opened(), Slot::Current);
    b.sealer_mut().rotate();
    a.publish(0, "nav", b"after", 30).unwrap();
    assert_eq!(nonce_of_under(&send_under(&mut a, 0, [9; 32]), [9; 32]), (1, 1, 2), "rotate left the epoch and the counter alone");
    let mut late = N::new("ghost", Key::new(KEY), 7, 1).unwrap(); // a late wire under the old key
    drain(&mut late, 0);
    late.publish(0, "nav", b"late", 30).unwrap();
    let lw = send(&mut late, 0);
    assert!(matches!(b.deliver(2, &lw.topic, &lw.wire, &mut buf), Incoming::Event { .. }));
    assert_eq!(b.opened(), Slot::Previous);
    b.sealer_mut().retire();
    a.sealer_mut().retire();
    a.publish(0, "nav", b"retired", 30).unwrap();
    assert_eq!(nonce_of_under(&send_under(&mut a, 0, [9; 32]), [9; 32]), (1, 1, 3), "and so did retire");
    late.publish(0, "nav", b"too late", 30).unwrap();
    let lw2 = send(&mut late, 0);
    assert_eq!(b.deliver(3, &lw2.topic, &lw2.wire, &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Tag)));
}

fn nonce_of_under(o: &Out, key: [u8; 32]) -> (u32, u32, u32) {
    let mut ad = vec![VERSION];
    ad.extend_from_slice(o.topic.as_bytes());
    let n = Key::new(key).open(&ad, &o.wire[1..], &mut [0u8; 256]).unwrap().nonce;
    let u = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap());
    (u(&n[0..4]), u(&n[4..8]), u(&n[8..12]))
}

#[test]
fn the_window_holds_64_counters_per_sender_in_any_order_and_a_wire_further_behind_is_a_replay() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    drain(&mut a, 0);
    let wires: Vec<Out> = (0..71u32).map(|i| { a.publish(i, "nav", &i.to_le_bytes(), 3600).unwrap(); send(&mut a, i) }).collect();
    for (i, w) in wires.iter().enumerate().take(68) { if i != 5 && i != 60 { assert_eq!(dropped(&mut b, 100 + i as u32, w), None, "wire {i}"); } }
    assert_eq!(dropped(&mut b, 100, &wires[60]), None, "7 behind, never seen: late, accepted");
    assert_eq!(dropped(&mut b, 100, &wires[60]), Some(Dropped::Duplicate));
    assert_eq!(dropped(&mut b, 100, &wires[4]), Some(Dropped::Duplicate), "63 behind: the seen table (8) forgot it, the window has not");
    assert_eq!(dropped(&mut b, 100, &wires[3]), Some(Dropped::Replay), "64 behind, inside the ttl: a replay, which the seen table alone accepted");
    for w in &wires[68..] { assert_eq!(dropped(&mut b, 100, w), None); }
    assert_eq!(dropped(&mut b, 100, &wires[3]), Some(Dropped::Replay), "after 70 newer wires");
    assert_eq!(dropped(&mut b, 100, &wires[5]), Some(Dropped::Replay), "the stated cost: never seen, but overtaken by {WINDOW}, so a replay");
    assert_eq!(dropped(&mut b, 100, &wires[7]), Some(Dropped::Duplicate), "63 behind the newest: still remembered");
    let mut again = from_block(&block_of(&b));
    again.resume(2).unwrap();
    assert_eq!(dropped(&mut again, 0, &wires[7]), Some(Dropped::Duplicate), "the window survived the restore");
    assert_eq!(dropped(&mut again, 0, &wires[3]), Some(Dropped::Replay));
}

#[test]
fn out_of_order_delivery_executes_each_wire_once() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    drain(&mut a, 0);
    let wires: Vec<Out> = (0..7u32).map(|i| { a.publish(0, "nav", &[i as u8], 60).unwrap(); send(&mut a, 0) }).collect();
    for i in [5, 3, 4, 0, 1, 6, 2] { assert_eq!(dropped(&mut b, 1, &wires[i]), None, "wire {i}"); }
    for w in &wires { assert_eq!(dropped(&mut b, 1, w), Some(Dropped::Duplicate)); }
}

#[test]
fn the_window_follows_a_sender_across_one_epoch_and_two_back_is_a_rollback() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    drain(&mut a, 0);
    let wire = |a: &mut N, now: u32, body: &[u8], ttl: u32| { a.publish(now, "nav", body, ttl).unwrap(); send(a, now) };
    let (old, older) = (wire(&mut a, 0, b"old", 3600), wire(&mut a, 0, b"older", 3600));
    a.renew(2).unwrap();
    let new = wire(&mut a, 1, b"new", 3600);
    let late: Vec<Out> = (0..4).map(|_| wire(&mut a, 1, b"late", 20)).collect();
    assert_eq!(dropped(&mut b, 1, &new), None);
    assert_eq!(dropped(&mut b, 2, &old), None, "an old-epoch wire the mesh held lands once");
    assert_eq!(dropped(&mut b, 2, &old), Some(Dropped::Duplicate));
    let image = block_of(&a);
    a.renew(3).unwrap();
    let newer = wire(&mut a, 3, b"newer", 3600);
    assert_eq!(dropped(&mut b, 3, &newer), None, "epoch 3 first heard at 3");
    assert_eq!(b.deliver(3, &older.topic, &older.wire, &mut [0u8; 256]), Incoming::Rollback { sender: 1 }, "two epochs back, no warp: no honest device speaks from there, since resume kills its queue");
    assert_eq!(dropped(&mut b, 3, &new), Some(Dropped::Duplicate), "the previous epoch is still remembered");
    assert_eq!(dropped(&mut b, 3 + 20, &late[0]), None, "sent before epoch 3's first wire, so due by 3 + its ttl: a straggler lands");
    let (before, rollbacks) = (block_of(&b), b.counts().rollbacks);
    assert_eq!(b.deliver(3 + 21, &late[1].topic, &late[1].wire, &mut [0u8; 256]), Incoming::Rollback { sender: 1 }, "a second past it no honest wire of epoch 2 is left: a clone or a rolled-back device");
    assert_eq!(b.counts().rollbacks, rollbacks + 1);
    assert_eq!(before.iter().zip(block_of(&b)).filter(|(x, y)| *x != y).count(), 1, "the counter's byte moved and nothing else: no window bit, seen entry or rate count");
    b.call(4, "nav", "Arm", b"", 60).unwrap();
    let mut clone = from_block(&image); // a reflash from the image taken before epoch 3
    let (arm, mut buf) = (send(&mut b, 4), [0u8; 256]);
    let c = call(&mut clone, 4, &arm, &mut buf);
    clone.reply(4, &c, 0, 0, "", b"", true).unwrap();
    let answer = send(&mut clone, 4);
    assert_eq!(b.deliver(5, &answer.topic, &answer.wire, &mut [0u8; 256]), Incoming::Rollback { sender: 1 }, "inside its ttl, but a reply is sent after its call, and this call was made after epoch 3 was first heard");
    let mut r = from_block(&before);
    r.resume(2).unwrap();
    assert_eq!(r.tick(1000), None, "a reboot: when epoch 3 was first heard is unknown, so it is this first clocked call");
    assert_eq!(dropped(&mut r, 1020, &late[2]), None, "measured from it");
    assert_eq!(r.deliver(1021, &late[3].topic, &late[3].wire, &mut [0u8; 256]), Incoming::Rollback { sender: 1 });
    let (mut g, mut d) = (peer("gcs", 1), peer("drone", 2));
    assert_eq!(dropped(&mut g, 100, &wire(&mut d, 100, b"up", 600)), None);
    g.call(1000, "drone", "Get", b"", 60).unwrap();
    let get = send(&mut g, 1000);
    let k = call(&mut d, 1000, &get, &mut buf);
    d.reply(1000, &k, 0, 0, "", b"", true).unwrap();
    let answer = send(&mut d, 1000);
    d.renew(2).unwrap();
    assert_eq!(dropped(&mut g, 999, &wire(&mut d, 1001, b"new", 600)), None, "the caller's clock stepped back a second: epoch 2 first heard at 999");
    assert_eq!(g.deliver(999, &answer.topic, &answer.wire, &mut [0u8; 256]), Incoming::Rollback { sender: 2 }, "the pinned cost of a clock that steps back: an honest answer to a call made at 1000 expires at 1060, past 999 + 60");
}

#[test]
fn a_call_saved_at_uptime_1000_and_restored_at_0_expires_on_the_first_tick() {
    let mut a = peer("gcs", 1);
    drain(&mut a, 0);
    let id = a.call(1000, "nav", "Arm", b"", 60).unwrap();
    send(&mut a, 1000);
    a.publish(1000, "nav", b"queued", 60).unwrap();
    let mut r = from_block(&block_of(&a));
    r.resume(2).unwrap();
    assert!(drain(&mut r, 0).iter().all(|o| o.listen), "a queued wire of unknown age dies at the exit; its own listen is queued again");
    assert_eq!(r.tick(0), Some(Incoming::Expired { id }), "60 to live at uptime 1000, restored at 0: of unknown age, expired");
    assert_eq!(r.tick(0), None);
    assert_eq!(a.tick(1059), None);
    assert_eq!(a.tick(1060), Some(Incoming::Expired { id }), "the peer that did not reboot keeps its schedule");
}

/// The seam under test: a sealer with a four-byte trailer inside the plaintext, its own wire version, and a `verify` that
/// counts its calls and can be told to refuse.
struct Checked { key: Key, verified: u32, refuse: bool }
impl Sealer for Checked {
    const VERSION: u8 = 2;
    const TRAILER: usize = 4;
    const ROSTER: usize = 0;
    fn seal(&mut self, nonce: &[u8; 12], ad: &[u8], plain: &mut [u8], n: usize, out: &mut [u8]) -> Result<usize, seal::Error> {
        plain.get_mut(n..n + 4).ok_or(seal::Error::Full)?.copy_from_slice(b"sig!");
        self.key.seal(nonce, ad, plain, n + 4, out)
    }
    fn open(&mut self, ad: &[u8], wire: &[u8], out: &mut [u8]) -> Result<Opened, seal::Error> { self.key.open(ad, wire, out) }
    fn verify(&mut self, _: u32, _: &[u8], _: &[u8; 12], plain: &mut [u8]) -> Result<(), seal::Error> {
        self.verified += 1;
        if self.refuse || !plain.ends_with(b"sig!") { plain.fill(0); return Err(seal::Error::Sig); }
        Ok(())
    }
    fn intact(&self) -> bool { self.key.intact() }
    fn resume(&mut self) {}
}
type C = Peer<Checked, 256, 4, 8, 16, 4>;
fn checked(name: &str, sender: u32) -> C { C::new(name, Checked { key: Key::new(KEY), verified: 0, refuse: false }, sender, 1).unwrap() }
fn sends_of(n: &mut C, now: u32) -> Vec<(String, Vec<u8>)> {
    let mut v = Vec::new();
    while let Some(o) = n.outgoing(now) {
        if let Outgoing::Send { topic, wire, .. } = o { v.push((topic.into(), wire.into())); }
        n.sent();
    }
    v
}

#[test]
fn verify_runs_after_every_public_verdict_and_nothing_is_marked_before_it_passes() {
    let (mut a, mut b, mut c) = (checked("gcs", 1), checked("drone", 2), checked("other", 3));
    let mut buf = [0u8; 256];
    let id = a.call(0, "nav", "Arm", b"", 30).unwrap();
    let (t, w) = sends_of(&mut a, 0).remove(0);
    assert_eq!(w[0], 2, "a sealer names the wire version it writes");
    let Incoming::Call(call) = b.deliver(1, &t, &w, &mut buf) else { panic!() };
    assert_eq!((call.sender, b.sealer_mut().verified), (1, 1), "the call names its sender; one verification");
    assert_eq!(b.deliver(1, &t, &w, &mut [0u8; 256]), Incoming::Dropped(Dropped::Duplicate));
    assert_eq!(b.sealer_mut().verified, 1, "a replay is a verdict from the seen table: no verification");
    b.reply(1, &call, 0, 0, "", b"ok", true).unwrap();
    let (rt, rw) = sends_of(&mut b, 1).remove(0);
    assert_eq!(c.deliver(2, &rt, &rw, &mut buf), Incoming::Dropped(Dropped::NoPending));
    assert_eq!(c.sealer_mut().verified, 0, "a reply to no call: no verification");
    a.sealer_mut().refuse = true;
    assert_eq!(a.deliver(2, &rt, &rw, &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Sig)));
    a.sealer_mut().refuse = false;
    assert!(matches!(a.deliver(2, &rt, &rw, &mut buf), Incoming::Reply { id: got, sender: 2, .. } if got == id), "the refused reply set no pending bit");
    let mut unsigned = peer("plain", 4);
    assert_eq!(unsigned.deliver(2, &t, &w, &mut buf), Incoming::Dropped(Dropped::Version(2)), "an unsigned receiver counts the signed wire, never opens it");
    unsigned.publish(2, "nav", b"", 30).unwrap();
    let o = send(&mut unsigned, 2);
    assert_eq!(b.deliver(2, &o.topic, &o.wire, &mut buf), Incoming::Dropped(Dropped::Version(1)), "and the signed receiver the unsigned wire: no downgrade");
    a.publish(3, "nav", b"evt", 30).unwrap();
    let (et, ew) = sends_of(&mut a, 3).remove(0);
    b.sealer_mut().refuse = true;
    let mut scratch = [0xAAu8; 256];
    assert_eq!(b.deliver(3, &et, &ew, &mut scratch), Incoming::Dropped(Dropped::Open(seal::Error::Sig)));
    assert!(scratch[..ew.len() - 1 - 12 - 16].iter().all(|&x| x == 0), "a refused plaintext is zeroed in the caller's buffer");
    for _ in 0..MAX_PER_SEC { assert_eq!(b.deliver(3, &et, &ew, &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Sig))); }
    b.sealer_mut().refuse = false;
    assert!(matches!(b.deliver(3, &et, &ew, &mut buf), Incoming::Event { sender: 1, .. }), "the refusals marked nothing: not the window, not the seen table, not the rate count");
    assert_eq!(b.counts().floods, 0);
    a.publish(3, "nav", b"newer", 30).unwrap();
    let (nt, nw) = sends_of(&mut a, 3).remove(0);
    assert!(matches!(b.deliver(3, &nt, &nw, &mut buf), Incoming::Event { .. }));
    let v = b.sealer_mut().verified;
    assert_eq!(b.deliver(3 + MAX_TTL, &et, &ew, &mut buf), Incoming::Dropped(Dropped::Replay), "a week on, below the floor");
    assert_eq!(b.sealer_mut().verified, v, "a wire below the floor: no verification");
    let mut d = checked("fresh", 5);
    d.sealer_mut().refuse = true;
    assert_eq!(d.deliver(3, &t, &w, &mut buf), Incoming::Dropped(Dropped::Open(seal::Error::Sig)));
    assert_eq!(d.pressure(), 0);
    d.sealer_mut().refuse = false;
    assert!(matches!(d.deliver(3, &t, &w, &mut buf), Incoming::Call(_)), "an unknown sender's refused wire inserted no record");
}

#[test]
fn the_trailer_comes_out_of_the_plaintext_budget() {
    let fits = |n: usize| peer("x", 1).publish(0, "t", &vec![0u8; n], 1).is_ok();
    let fits_checked = |n: usize| checked("x", 1).publish(0, "t", &vec![0u8; n], 1).is_ok();
    let max = (0..256).rev().find(|&n| fits(n)).unwrap();
    assert!(!fits(max + 1) && fits_checked(max - 4) && !fits_checked(max - 3), "four bytes of trailer, four bytes less payload");
}

#[test]
fn a_clock_that_goes_backwards_does_not_open_a_receive_budget_either() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    drain(&mut a, 0);
    let wires: Vec<Out> = (0..MAX_PER_SEC + 2).map(|i| { a.publish(i, "nav", &[1], 60).unwrap(); send(&mut a, i) }).collect();
    for w in &wires[..MAX_PER_SEC as usize] { assert_eq!(dropped(&mut b, 10, w), None); }
    assert!(matches!(b.deliver(10, &wires[64].topic, &wires[64].wire, &mut [0u8; 256]), Incoming::Flood { sender: 1 }));
    assert_eq!(dropped(&mut b, 9, &wires[65]), Some(Dropped::Rate), "an earlier second opens no budget on the way in");
    a.publish(70, "nav", &[1], 60).unwrap();
    let later = send(&mut a, 70);
    assert_eq!(dropped(&mut b, 11, &later), None, "the next second does");
}

#[test]
fn no_nonce_twice_across_every_lifecycle_to_depth_five_and_the_host_duty_shown() {
    // ops on every device of the lineage: 0 send, 1 save and restore as a new device at the host's next epoch, 2 renew, 3 stage and rotate, 4 retire
    let mut runs = 0;
    for depth in 1..=5u32 {
        for mut code in 0..5u32.pow(depth) {
            let (mut host_epoch, mut seen) = (1u32, std::collections::HashSet::new());
            let mut devices = vec![N::new("gcs", Key::new(KEY), 7, host_epoch).unwrap()];
            drain(&mut devices[0], 0);
            for _ in 0..depth {
                let op = code % 5;
                code /= 5;
                for i in 0..devices.len() {
                    match op {
                        0 => { devices[i].publish(0, "t", b"x", 10).unwrap(); let n = nonce_of(&send(&mut devices[i], 0)); assert!(seen.insert(n), "{n:?} twice"); }
                        1 => { host_epoch += 1; let mut c = from_block(&block_of(&devices[i])); c.resume(host_epoch).unwrap(); assert!(drain(&mut c, 0).iter().all(|o| o.listen), "a restored device has no wire queued"); devices.push(c); }
                        2 => { host_epoch += 1; devices[i].renew(host_epoch).unwrap(); }
                        3 => { devices[i].sealer_mut().stage(KEY); devices[i].sealer_mut().rotate(); }
                        _ => devices[i].sealer_mut().retire(),
                    }
                }
            }
            runs += 1;
        }
    }
    assert_eq!(runs, 5 + 25 + 125 + 625 + 3125);
    let mut a = N::new("gcs", Key::new(KEY), 7, 1).unwrap();
    drain(&mut a, 0);
    let (mut b, mut c) = (from_block(&block_of(&a)), from_block(&block_of(&a)));
    b.resume(2).unwrap();
    c.resume(2).unwrap();
    b.publish(0, "t", b"x", 10).unwrap();
    c.publish(0, "t", b"x", 10).unwrap();
    assert_eq!(nonce_of(&send(&mut b, 0)), nonce_of(&send(&mut c, 0)), "an epoch handed out twice is a nonce used twice: the host's duty, which core cannot check");
}

#[test]
fn a_rate_dropped_wire_marks_no_window_and_lands_when_redelivered_in_a_calmer_second() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    drain(&mut a, 0);
    let wires: Vec<Out> = (0..=MAX_PER_SEC).map(|i| { a.publish(i, "nav", &[1], 60).unwrap(); send(&mut a, i) }).collect();
    for w in &wires[..MAX_PER_SEC as usize] { assert_eq!(dropped(&mut b, 10, w), None); }
    assert!(matches!(b.deliver(10, &wires[64].topic, &wires[64].wire, &mut [0u8; 256]), Incoming::Flood { sender: 1 }), "the 65th in one second");
    assert_eq!(dropped(&mut b, 11, &wires[64]), None, "the same wire a second later: it was never marked, so it lands");
    let (mut c, mut fleet) = (peer("drone2", 3), (100..100 + SENDERS as u32).map(|s| peer("x", s)).collect::<Vec<N>>());
    for i in 0..RX_PER_SEC as usize { let x = &mut fleet[i % SENDERS]; x.publish(20, "nav", &[1], 60).unwrap(); assert_eq!(dropped(&mut c, 20, &send(x, 20)), None, "sixteen windows, sixteen wires each"); }
    let (before, mut x) = (c.pressure(), peer("x", 999));
    x.publish(20, "nav", &[1], 60).unwrap();
    assert_eq!(dropped(&mut c, 20, &send(&mut x, 20)), Some(Dropped::Rate), "the global cap");
    assert_eq!(c.pressure(), before, "and a wire over it takes no slot and evicts no one");
}

/// Client streaming is cut, so a call's `seq` is 0 or the wire is `Seq`; its id field, if present, is its own nonce or `Id`.
#[test]
fn a_call_with_any_seq_but_zero_is_dropped_and_its_id_is_its_nonce() {
    let mut s = peer("drone", 2);
    let craft = |seq: u32, id: Option<Id>| {
        let e = Envelope { id, ttl_secs: 30, seq, end: true, body: Body::Call { method: "M", reply_to: "gcs", payload: b"" } };
        let mut plain = [0u8; 256];
        let n = e.encode(&mut plain).unwrap();
        let mut nonce = [0u8; 12];
        nonce[0..4].copy_from_slice(&3u32.to_le_bytes());
        nonce[4..8].copy_from_slice(&1u32.to_le_bytes());
        nonce[8..12].copy_from_slice(&(seq + 10).to_le_bytes());
        let (mut ad, mut wire) = (vec![VERSION, b'n', b'a', b'v'], vec![VERSION; 256]);
        let w = Key::new(KEY).seal(&nonce, &ad.split_off(0), &mut plain, n, &mut wire[1..]).unwrap();
        wire.truncate(1 + w);
        (wire, Id(nonce))
    };
    let verdict = |s: &mut N, wire: &[u8]| match s.deliver(1, "nav", wire, &mut [0u8; 256]) { Incoming::Dropped(d) => Some(d), _ => None };
    let planted = Some(Id([0xAB; 12]));
    assert_eq!(verdict(&mut s, &craft(1, planted).0), Some(Dropped::Seq));
    assert_eq!(verdict(&mut s, &craft(7, None).0), Some(Dropped::Seq), "no id field either: the seq alone refuses it");
    assert_eq!(verdict(&mut s, &craft(0, planted).0), Some(Dropped::Id));
    let (wire, own) = craft(0, None);
    let (wire2, _) = craft(0, Some(own));
    assert!(matches!(peer("x", 4).deliver(1, "nav", &wire2, &mut [0u8; 256]), Incoming::Call(_)), "seq 0 and its own nonce as id: a call");
    assert_eq!(verdict(&mut s, &wire), None, "a unary call");
    assert_eq!(verdict(&mut s, &wire2), Some(Dropped::Duplicate), "its own nonce as id is the same call, already seen");
}

/// Completion is a mask over 0..=end: an item a misbehaving server sends above `end` cannot hold the call open.
#[test]
fn a_stray_item_above_end_does_not_keep_a_completed_call_pending() {
    let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
    a.call(0, "cam", "Frames", b"", 300).unwrap();
    let o = drain(&mut a, 0).pop().unwrap();
    let mut buf = [0u8; 256];
    let Incoming::Call(c) = b.deliver(1, &o.topic, &o.wire, &mut buf) else { panic!() };
    b.reply(1, &c, 5, 0, "", b"stray", false).unwrap();
    for o in drain(&mut b, 1).into_iter().filter(|o| !o.listen) { assert!(matches!(a.deliver(2, &o.topic, &o.wire, &mut [0u8; 256]), Incoming::Reply { .. })); }
    for seq in 0..4u32 { b.reply(1, &c, seq, 0, "", &[seq as u8], seq == 3).unwrap(); }
    for o in drain(&mut b, 1) { assert!(matches!(a.deliver(2, &o.topic, &o.wire, &mut [0u8; 256]), Incoming::Reply { .. })); }
    assert_eq!(a.tick(300), None, "items 0..=3 arrived with end at 3: complete, nothing expires");
    assert!(a.call(300, "cam", "Frames", b"", 1).is_ok(), "and the pending slot is free again");
}

/// The lowest `end` closes the stream, in either order, and an item above it is `Seq`: a second `end` above the first
/// cannot hold a completed call pending, nor an item above a lowered `end` reach product code.
#[test]
fn a_second_end_above_the_first_does_not_keep_a_completed_call_pending() {
    for (ends, above) in [([3, 5], &[5, 4][..]), ([5, 3], &[4][..])] {
        let (mut a, mut b) = (peer("gcs", 1), peer("drone", 2));
        a.call(0, "cam", "Frames", b"", 300).unwrap();
        let o = drain(&mut a, 0).pop().unwrap();
        let mut buf = [0u8; 256];
        let Incoming::Call(c) = b.deliver(1, &o.topic, &o.wire, &mut buf) else { panic!() };
        for seq in ends.into_iter().chain(4..5).chain(0..3) {
            b.reply(1, &c, seq, 0, "", &[seq as u8], ends.contains(&seq)).unwrap();
            for o in drain(&mut b, 1).into_iter().filter(|o| !o.listen) {
                let v = format!("{:?}", a.deliver(2, &o.topic, &o.wire, &mut [0u8; 256]));
                assert!(v.starts_with(if above.contains(&seq) { "Dropped(Seq)" } else { "Reply" }), "{ends:?}: seq {seq} is {v}");
            }
        }
        assert_eq!(a.tick(300), None, "{ends:?}: items 0..=3 with an end at 3 arrived: complete, nothing expires");
    }
}

/// A saved copy is disarmed: it still listens and delivers, but cannot send until `resume` arms it.
#[test]
fn a_disarmed_block_cannot_send_until_it_is_resumed() {
    let mut a = peer("gcs", 1);
    a.disarm();
    assert_eq!(a.call(0, "nav", "M", b"", 10), Err(Error::Corrupt));
    assert_eq!(a.publish(0, "t", b"", 10), Err(Error::Corrupt));
    assert!(a.listen("t").is_ok(), "a listen sends nothing");
    assert!(matches!(a.deliver(0, "nav", &[VERSION], &mut [0u8; 256]), Incoming::Dropped(_)), "delivery still answers");
    let mut b = peer("b", 2);
    b.publish(0, "t", b"x", 10).unwrap();
    let o = send(&mut b, 0);
    assert!(matches!(a.deliver(0, &o.topic, &o.wire, &mut [0u8; 256]), Incoming::Event { .. }), "and an honest wire reaches product code");
    assert_eq!(a.resume(1), Err(Error::Epoch));
    assert_eq!(a.publish(0, "t", b"", 10), Err(Error::Corrupt), "a resume refused arms nothing: its epoch and counter are the live peer's");
    a.resume(2).unwrap();
    assert!(a.call(0, "nav", "M", b"", 10).is_ok());
}

/// A `resume` refused for its epoch touches nothing on a live peer: the send cap, the receive cap, a live call's ttl and a
/// queued wire all stay as they were.
#[test]
fn a_resume_refused_for_its_epoch_resets_nothing() {
    let (mut a, mut b) = (peer("gcs", 1), peer("b", 2));
    drain(&mut a, 0);
    for _ in 0..SEND_PER_SEC { a.publish(9, "t", b"x", 10).unwrap(); drain(&mut a, 9); }
    for i in 0..MAX_PER_SEC {
        b.publish(1_000 + i, "t", b"x", 60).unwrap(); // a lying clock on the sender so its own cap never bites
        let o = drain(&mut b, 1_000 + i).pop().unwrap();
        assert!(matches!(a.deliver(9, &o.topic, &o.wire, &mut [0u8; 256]), Incoming::Event { .. }));
    }
    assert_eq!(a.resume(1), Err(Error::Epoch));
    assert_eq!(a.publish(9, "t", b"x", 10), Err(Error::Rate), "the 33rd send this second is still refused");
    b.publish(2_000, "t", b"x", 60).unwrap();
    let o = drain(&mut b, 2_000).pop().unwrap();
    assert_eq!(a.deliver(9, &o.topic, &o.wire, &mut [0u8; 256]), Incoming::Flood { sender: 2 }, "and the 65th receipt from b");
    let id = a.call(100, "nav", "M", b"", 600).unwrap();
    send(&mut a, 100);
    a.publish(100, "t", b"queued", 600).unwrap();
    assert_eq!(a.resume(1), Err(Error::Epoch));
    assert_eq!(a.tick(101), None, "the live call keeps its ttl");
    assert_eq!(drain(&mut a, 101).len(), 1, "the queued wire survives");
    assert_eq!(a.tick(700), Some(Incoming::Expired { id }));
}

/// The operator's counters: management, rollbacks, floods and drops but duplicates, saturating, kept across a resume.
#[test]
fn counts_tally_management_rollbacks_floods_and_drops_and_survive_resume() {
    let (mut a, mut b) = (peer("gcs", 1), peer("b", 2));
    assert_eq!(a.counts(), Counts::default());
    a.sealer_mut().stage([9; 32]);
    a.sealer_mut().rotate();
    assert_eq!(a.counts().managed, 2, "the sealer taken twice for a change");
    let _ = a.sealer_ref();
    assert_eq!(a.counts().managed, 2, "reading it is not a change");
    assert!(matches!(a.deliver(0, "nav", &[VERSION], &mut [0u8; 256]), Incoming::Dropped(_)));
    assert_eq!(a.counts().dropped, 1);
    b.publish(0, "t", b"first", 60).unwrap();
    b.publish(0, "t", b"held", 60).unwrap();
    let mut w = drain(&mut b, 0).into_iter().filter(|o| !o.listen);
    let (first, held) = (w.next().unwrap(), w.next().unwrap());
    assert!(matches!(a.deliver(1, &first.topic, &first.wire, &mut [0u8; 256]), Incoming::Event { .. }));
    assert_eq!(dropped(&mut a, 1, &first), Some(Dropped::Duplicate), "a mesh that delivers twice: no drop counted");
    for epoch in [2, 3] {
        b.renew(epoch).unwrap();
        b.publish(1, "t", b"x", 60).unwrap();
        let o = drain(&mut b, 1).pop().unwrap();
        assert!(matches!(a.deliver(1, &o.topic, &o.wire, &mut [0u8; 256]), Incoming::Event { .. }));
    }
    assert!(matches!(a.deliver(1, &held.topic, &held.wire, &mut [0u8; 256]), Incoming::Rollback { sender: 2 }), "older than both epochs heard");
    assert_eq!((a.counts().rollbacks, a.counts().dropped), (1, 1));
    for i in 0..66u32 {
        b.publish(1_000 + i, "t", b"spam", 5).unwrap(); // a lying clock on the sender so its own cap never bites
        let o = drain(&mut b, 1_000 + i).pop().unwrap();
        let _ = a.deliver(5, &o.topic, &o.wire, &mut [0u8; 256]);
    }
    assert_eq!((a.counts().floods, a.counts().dropped), (1, 2), "the 65th named the flood, the 66th was a drop");
    let before = a.counts();
    a.resume(2).unwrap();
    assert_eq!(a.counts(), before, "the block keeps them");
}

/// At a receiver with more windows than `RX_PER_SEC`, a Rust hub for a fleet past 256, a spent second names a sender with
/// a window once, counted among the heard from its first wire, and a sender with no window never by share.
#[test]
fn past_the_cap_in_windows_a_sender_with_one_is_named_once_and_a_stranger_never_by_share() {
    let wire = |x: &mut N, t: u32| { x.publish(t, "nav", b"f", 60).unwrap(); send(x, t) };
    let (mut hub, mut buf, mut fleet) = (Peer::<Key, 256, 4, 8, 300, 4>::new("hub", Key::new(KEY), 999, 1).unwrap(), [0u8; 256], (1..=300).map(|s| peer("x", s)).collect::<Vec<N>>());
    for (t, first) in [(1, 0), (2, 200)] { for i in 0..300 { let w = wire(&mut fleet[(i + first) % 300], t); let _ = hub.deliver(t, &w.topic, &w.wire, &mut buf); } } // every sender heard twice, so each holds a window
    let named = fleet.iter_mut().filter_map(|x| { let w = wire(x, 3); matches!(hub.deliver(3, &w.topic, &w.wire, &mut buf), Incoming::Flood { .. }).then_some(()) }).count();
    assert_eq!(named, 44, "the 257th to the 300th wire of the second, each its sender's first: 1 × 257 and more heard, itself among them, is over the cap");
    let next: Vec<_> = fleet.iter_mut().map(|x| { let w = wire(x, 3); match hub.deliver(3, &w.topic, &w.wire, &mut buf) { Incoming::Flood { sender } => Some(sender), Incoming::Dropped(Dropped::Rate) => None, v => panic!("{v:?}") } }).collect();
    assert_eq!(next, (1..=300).map(|s| (s <= 256).then_some(s)).collect::<Vec<_>>(), "each of the 256 that landed before the cap was spent named on its next wire, the 44 named already refused: all 300 hold a window");
    let w = wire(&mut fleet[0], 3);
    assert_eq!(hub.deliver(3, &w.topic, &w.wire, &mut buf), Incoming::Dropped(Dropped::Rate), "named once, then refused");
    let mut stranger = peer("x", 301);
    for _ in 0..3 { let w = wire(&mut stranger, 3); assert_eq!(hub.deliver(3, &w.topic, &w.wire, &mut buf), Incoming::Dropped(Dropped::Rate), "a sender with no window has no share: its count is the strangers'"); }
}
