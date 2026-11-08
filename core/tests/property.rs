//! The property test (SPEC.md layer 2): seeded random interleavings of every operation over two to seven peers on a mesh
//! that loses nothing but delays, reorders and repeats, each verdict predicted by a model of the promises, not of the
//! code. `MUDRPC_SEED` and `MUDRPC_FUZZ` as in harden.rs; a release runs it long.
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
    clippy::many_single_char_names, // test scripts: `n` a node, `w` a wire, `s` a sender's record, `e` an expectation
)]
use mudrpc_core::envelope::Id;
use mudrpc_core::rpc::{Call, Counts, Dropped, Error, Incoming, Outgoing, Peer, MAX_PER_SEC, MAX_TTL, RX_PER_SEC, SEND_PER_SEC, VERSION, WINDOW};
use mudrpc_core::seal::{self, Key, Sealer, Slot};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
mod support;
use support::{env, Rng};

const PENDING: usize = 4;
const SEEN: usize = 8;
const OUT: usize = 4;
type P<const SENDERS: usize> = Peer<Key, 256, PENDING, SEEN, SENDERS, OUT>;
type Nonce = (u32, u32, u32); // sender, epoch, counter

/// The schedule's operations and their weights, out of 200.
const OPS: [(&str, usize); 12] = [("publish", 24), ("call", 20), ("reply", 20), ("flush", 24), ("deliver", 63), ("tick", 16), ("time", 24), ("reboot", 2), ("renew", 2), ("rekey", 2), ("burst", 1), ("reflash", 2)];

/// A second begins only when the clock moves forward (SPEC.md layer 2), stated again so the model shares no code.
const fn tally(now: u32, sec: u32, count: u32) -> (u32, u32) { if now > sec { (now, 1) } else { (sec, count.saturating_add(1)) } }

fn key(generation: u32) -> [u8; 32] { let mut k = [7; 32]; k[..4].copy_from_slice(&generation.to_le_bytes()); k }
fn id((s, e, c): Nonce) -> Id { Id([s.to_le_bytes(), e.to_le_bytes(), c.to_le_bytes()].concat().try_into().unwrap()) }

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Class { Exec, Duplicate, Replay, Rollback, NoPending, Rate, Flood, Tag, Seq }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind { Call, Reply { call: Nonce, seq: u32, end: bool }, Event }

/// `at`: each node's clock and backward steps when the wire left, and `from` the node it left, for the oracle.
#[derive(Clone, Debug)]
struct Wire { topic: String, bytes: Vec<u8>, nonce: Nonce, kind: Kind, ttl: u32, generation: u32, at: Vec<(u32, u32)>, from: usize }

/// The nonce under the header, as a receiver holding the wire's key reads it.
fn nonce_of(w: &Wire) -> Nonce {
    let ad = [&[VERSION], w.topic.as_bytes()].concat();
    let n = Key::new(key(w.generation)).open(&ad, &w.bytes[1..], &mut [0; 256]).expect("opens under the key it was sealed with").nonce;
    let u = |i: usize| u32::from_le_bytes(n[i..i + 4].try_into().unwrap());
    (u(0), u(4), u(8))
}

/// What a receiver knows of one sender: per epoch, newest first, the highest counter and those below it that arrived;
/// when the highest was seen, the floor, this second's count, when the newest epoch was first heard.
#[derive(Clone)]
struct Win { sender: u32, epochs: Vec<(u32, u32, BTreeSet<u32>)>, seen_at: Option<u32>, floor: (u32, u32), sec: u32, count: u32, flooded: bool, epoch_at: u32 }

impl Win {
    const fn fresh(sender: u32, now: u32) -> Self { Self { sender, epochs: Vec::new(), seen_at: None, floor: (0, 0), sec: now, count: 0, flooded: false, epoch_at: now } }
}

/// The floor, then the window, on a copy: the record as an accepted wire leaves it, or the verdict that refuses it. A
/// wire below the newest epoch was sent before that epoch's first, so an honest one arrives within its ttl of it, and
/// a reply, sent after its call, answers one made before it: `by` is now, or for a reply the later of now and the
/// call's expiry, a ttl after it was made.
fn window(known: &Win, at: (u32, u32), now: u32, ttl: u32, by: u32) -> Result<Win, Class> {
    let mut s = known.clone();
    let high = s.epochs.first().map_or((0, 0), |e| (e.0, e.1));
    if s.seen_at.is_some_and(|t| t.checked_add(MAX_TTL).is_some_and(|t| now >= t)) { s.floor = high; }
    if at < s.floor { return Err(if at.0 < s.floor.0 { Class::Rollback } else { Class::Replay }); }
    if at > high || s.seen_at.is_none() { s.seen_at = Some(now); }
    let i = match s.epochs.iter().position(|e| e.0 == at.0) {
        Some(i) => i,
        None if at.0 > high.0 => { s.epochs.insert(0, (at.0, 0, BTreeSet::new())); s.epochs.truncate(2); s.epoch_at = now; 0 }
        None if s.epochs.len() == 1 || s.epochs[1].0 < at.0 => { s.epochs.truncate(1); s.epochs.push((at.0, 0, BTreeSet::new())); 1 }
        None => return Err(Class::Rollback),
    };
    let (_, high, arrived) = &mut s.epochs[i];
    if at.1 > *high { *high = at.1; arrived.retain(|&c| c + WINDOW > at.1); } else if *high - at.1 >= WINDOW { return Err(Class::Replay); }
    if !arrived.insert(at.1) { return Err(Class::Duplicate); }
    if i == 1 && by > s.epoch_at.saturating_add(ttl) { return Err(Class::Rollback); }
    if s.flooded && tally(now, s.sec, s.count).1 > MAX_PER_SEC { return Err(Class::Rate); }
    Ok(s)
}

#[derive(Clone)]
struct Queued { topic: String, wire: Option<Wire>, expires: u32 }
#[derive(Clone)]
struct Pend { expires: u32, got: u64, end: Option<u32>, steps: u32 }
#[derive(Clone)]
struct Served { call: Nonce, reply_to: String, ttl: u32, next: u32, last: u32 }

/// A verdict the model predicts: `maybe_dup` when the seen table may have let the wire go, what a verdict after the
/// window writes to the sender's slot, and the strangers' count it leaves.
struct Expect { maybe_dup: bool, class: Class, write: Option<(Option<usize>, Win)>, strangers: Option<u32> }

/// One device and the model of it: its own counters, the keys it holds, its outbox, pending calls, every seen entry as
/// (nonce, expiry), `false` where a full table or a reboot may have let it go, the sender table, this second's calls and
/// events received and those from senders with no window to hold, whether a reboot left the sender table's silence unknown, a save kept for a reflash and its epoch,
/// the backward steps its clock has taken and every (sender, epoch, from a reflash) it accepted.
struct Node<const SENDERS: usize> {
    p: P<SENDERS>, name: String, sender: u32, now: u32, epoch: u32, next: u32, sent: (u32, u32), keys: [Option<u32>; 3], steps: u32, heard: BTreeSet<(u32, u32, bool)>,
    out: VecDeque<Queued>, pending: BTreeMap<Nonce, Pend>, seen: BTreeMap<(Nonce, u32), bool>, slots: Vec<Option<Win>>, rx: (u32, u32), strangers: u32, asleep: bool,
    counts: Counts, pressure: u32, served: Vec<Served>, last: Option<Wire>, executed: BTreeSet<Nonce>, image: Option<(Box<Self>, u32)>,
}

impl<const SENDERS: usize> Node<SENDERS> {
    fn new(i: usize, now: u32) -> Self {
        let (name, sender) = (format!("p{i}"), i as u32 + 1);
        let mut p = P::<SENDERS>::new(&name, Key::new(key(0)), sender, 1).unwrap();
        p.listen("t").unwrap();
        let out = [name.clone(), "t".into()].map(|topic| Queued { topic, wire: None, expires: 0 }).into();
        Self { p, name, sender, now, epoch: 1, next: 0, sent: (0, 0), keys: [Some(0), None, None], steps: 0, heard: BTreeSet::new(), out, pending: BTreeMap::new(), seen: BTreeMap::new(), slots: vec![None; SENDERS], rx: (0, 0), strangers: 0, asleep: false, counts: Counts::default(), pressure: 0, served: Vec::new(), last: None, executed: BTreeSet::new(), image: None }
    }

    /// The device's bytes as a host saves them, and the model of it.
    fn copy(&self) -> Self {
        let p = unsafe { std::ptr::read_unaligned(std::ptr::from_ref(&self.p)) };
        Self { p, name: self.name.clone(), heard: self.heard.clone(), out: self.out.clone(), pending: self.pending.clone(), seen: self.seen.clone(), slots: self.slots.clone(), served: self.served.clone(), last: self.last.clone(), executed: self.executed.clone(), image: None, ..*self }
    }

    /// `resume` at `epoch`: what was in flight is of unknown age and the sender table's silence unknown.
    fn restart(&mut self, epoch: u32, reset: bool) {
        self.p.resume(epoch).unwrap();
        if reset { self.clock(0); }
        (self.epoch, self.next, self.sent, self.rx, self.strangers, self.asleep) = (epoch, 0, (0, 0), (0, 0), 0, true);
        for s in self.slots.iter_mut().flatten() { (s.seen_at, s.count, s.flooded) = (None, 0, false); }
        for p in self.pending.values_mut() { p.expires = 0; }
        for q in &mut self.out { q.expires = 0; }
        if self.out.len() < OUT { self.out.push_back(Queued { topic: self.name.clone(), wire: None, expires: 0 }); }
    }

    /// The clock set back by a reboot or a step: a seen entry dead before may be live again, or its slot reused.
    fn clock(&mut self, now: u32) {
        if now < self.now { self.steps += 1; for (k, sure) in &mut self.seen { *sure &= k.1 > self.now; } }
        self.now = now;
    }

    /// Silence and the newest epoch's first hearing are counted from a second the receiver saw: after a reboot, from the
    /// first `tick` or the first wire that opens.
    fn wake(&mut self) {
        if self.asleep { for s in self.slots.iter_mut().flatten() { (s.sec, s.epoch_at) = (self.now, self.now); } }
        self.asleep = false;
    }

    fn can_send(&self) -> Result<(), Error> {
        if self.out.len() == OUT { Err(Error::Full) } else if tally(self.now, self.sent.0, self.sent.1).1 > SEND_PER_SEC { Err(Error::Rate) } else { Ok(()) }
    }

    fn queue(&mut self, topic: &str, kind: Kind, ttl: u32) -> Nonce {
        let nonce = (self.sender, self.epoch, self.next);
        (self.next, self.sent) = (self.next + 1, tally(self.now, self.sent.0, self.sent.1));
        let wire = Wire { topic: topic.into(), bytes: Vec::new(), nonce, kind, ttl, generation: self.keys[0].unwrap(), at: Vec::new(), from: 0 };
        self.out.push_back(Queued { topic: topic.into(), wire: Some(wire), expires: self.now.saturating_add(ttl) });
        nonce
    }

    /// The verdict, in the flowchart's order: the tag, dedupe, the floor and window, the caps.
    fn expect(&self, w: &Wire) -> Expect {
        let (now, refuse) = (self.now, |class| Expect { maybe_dup: false, class, write: None, strangers: None });
        if !self.keys.contains(&Some(w.generation)) { return refuse(Class::Tag); }
        let (mut maybe_dup, mut by) = (false, now);
        if let Kind::Reply { call, seq, .. } = w.kind {
            let Some(p) = self.pending.get(&call) else { return refuse(Class::NoPending) };
            if p.end.is_some_and(|e| seq > e) { return refuse(Class::Seq); }
            if p.got >> seq & 1 == 1 { return refuse(Class::Duplicate); }
            by = p.expires.max(now);
        } else {
            let mut live = self.seen.range((w.nonce, now + 1)..=(w.nonce, u32::MAX)).map(|(_, &sure)| sure);
            if live.clone().any(|sure| sure) { return refuse(Class::Duplicate); }
            maybe_dup = live.next().is_some();
        }
        let k = self.slots.iter().position(|s| s.as_ref().is_some_and(|s| s.sender == w.nonce.0));
        let known = k.and_then(|k| self.slots[k].clone()).unwrap_or_else(|| Win::fresh(w.nonce.0, now));
        let mut rec = match window(&known, (w.nonce.1, w.nonce.2), now, w.ttl, by) { Ok(r) => r, Err(class) => return Expect { maybe_dup, class, write: None, strangers: None } };
        let reply = matches!(w.kind, Kind::Reply { .. }); // a reply to our call: PENDING × MAX_SEQ bound it
        let (rx_sec, spent) = if reply { (self.rx.0, false) } else { let (sec, n) = tally(now, self.rx.0, self.rx.1); (sec, n > RX_PER_SEC) };
        let room = self.slots.iter().any(|s| s.as_ref().is_none_or(|s| s.sec.checked_add(MAX_TTL).is_some_and(|t| now >= t)));
        let stranger = !reply && k.is_none() && !room; // no window and none to hold: one sender's cap for every such number
        let strangers = if rx_sec == self.rx.0 { self.strangers } else { 0 } + u32::from(stranger);
        let (sec, count) = tally(now, rec.sec, rec.count);
        (rec.sec, rec.count, rec.flooded) = (sec, count, rec.flooded && sec == rec.sec);
        let others = self.slots.iter().enumerate().filter(|&(i, s)| Some(i) != k && s.as_ref().is_some_and(|s| s.sec == self.rx.0 && s.count > 0)).count();
        let named = k.is_some() && (count > MAX_PER_SEC || spent && u64::from(count) * (others as u64 + 1) > u64::from(RX_PER_SEC)) || stranger && strangers == MAX_PER_SEC + 1; // a sender with a window, over its own cap or its share of a spent second among the windows heard in it, its own too; the strangers, past theirs
        if spent || count > MAX_PER_SEC || stranger && strangers > MAX_PER_SEC {
            let class = if named && !rec.flooded { Class::Flood } else { Class::Rate };
            return Expect { maybe_dup, class, write: k.map(|k| (Some(k), Win { sec, count, flooded: rec.flooded || named, ..known })), strangers: (stranger && strangers > MAX_PER_SEC).then_some(strangers) }; // counted where it landed, and past the cap, where the count running on names the 65th once
        }
        Expect { maybe_dup, class: Class::Exec, write: Some((k, rec)), strangers: Some(strangers) }
    }

    /// The marks an accepted wire leaves; `delta` is the peer's pressure since the wire arrived. Returns whether the seen
    /// table evicted.
    fn commit(&mut self, w: &Wire, k: Option<usize>, rec: Win, delta: u32) -> bool {
        let now = self.now;
        let refused = u32::from(k.is_none() && self.slots.iter().all(Option::is_some));
        let slot = k.or_else(|| self.slots.iter().position(Option::is_none)).or_else(|| self.slots.iter().position(|s| s.as_ref().is_some_and(|s| s.sec.checked_add(MAX_TTL).is_some_and(|t| now >= t))));
        if let Some(i) = slot { self.slots[i] = Some(rec); }
        let held = self.keys.iter().position(|g| *g == Some(w.generation)).unwrap();
        assert_eq!(self.p.opened(), [Slot::Current, Slot::Previous, Slot::Next][held], "the slot that opened it");
        let Kind::Reply { call, seq, end } = w.kind else {
            self.rx = tally(now, self.rx.0, self.rx.1);
            let sure = self.seen.iter().filter(|&(&(_, x), &sure)| x > now && sure).map(|(&(_, x), _)| x);
            let (low, sure) = (sure.clone().min().unwrap_or(u32::MAX), sure.count());
            let all = self.seen.keys().filter(|&&(_, x)| x > now).count();
            let evicted = delta.checked_sub(refused).expect("a refused window is counted");
            assert!(evicted <= 1 && (sure < SEEN || evicted == 1) && (all >= SEEN || evicted == 0), "the seen table evicts when full and only then: {evicted}, {sure} sure of {all}");
            if evicted == 1 {
                let gone: Vec<(Nonce, u32)> = self.seen.keys().filter(|&&(_, x)| x > now && x <= low).copied().collect();
                for g in &gone { if gone.len() == 1 { self.seen.remove(g); } else { self.seen.insert(*g, false); } }
            }
            self.seen.insert((w.nonce, now.saturating_add(w.ttl)), true);
            return evicted == 1;
        };
        assert_eq!(delta, refused, "a reply enters no seen table");
        let p = self.pending.get_mut(&call).unwrap();
        p.got |= 1 << seq;
        if end { p.end = Some(p.end.map_or(seq, |e| e.min(seq))); }
        if p.end.is_some_and(|e| (0..=e).all(|s| p.got >> s & 1 == 1)) { self.pending.remove(&call); }
        false
    }
}

#[derive(Default)]
struct Stats { classes: BTreeMap<Class, u64>, forgot: u64, late: u64, evicted: u64, expired: usize }

struct World<const SENDERS: usize> { nodes: Vec<Node<SENDERS>>, epochs: Vec<u32>, bag: Vec<Wire>, aired: BTreeSet<Nonce>, rng: Rng, round: u64, life: u64, lasting: bool, op: String, stats: Stats }

impl<const SENDERS: usize> World<SENDERS> {
    const fn ttl(&mut self) -> u32 {
        match self.rng.below(20) { 0..=7 => self.rng.below(5) as u32, 8..=15 => 5 + self.rng.below(300) as u32, 16..=18 => 300 + self.rng.below(3600) as u32, _ => [MAX_TTL, u32::MAX][self.rng.below(2)] }
    }

    fn round(&mut self) {
        if self.round.is_multiple_of(self.life) { // a fresh fleet of two to four, to seven over six windows, its clocks anywhere, or past a week and surviving reboots
            self.lasting = self.rng.below(2) == 0;
            let base = if self.lasting { MAX_TTL + self.rng.below(9 * MAX_TTL as usize) as u32 } else { 0 };
            self.nodes = (0..2 + self.rng.below(SENDERS.max(3))).map(|i| Node::new(i, base + self.rng.below(1000) as u32)).collect();
            self.epochs = vec![1; self.nodes.len()];
            (self.bag, self.aired, self.life) = (Vec::new(), BTreeSet::new(), 1000 + self.rng.below(4000) as u64);
        }
        self.round += 1;
        let (i, mut k) = (self.rng.below(self.nodes.len()), self.rng.below(200));
        let op = OPS.iter().position(|&(_, w)| if k < w { true } else { k -= w; false }).unwrap();
        self.op = format!("{} on p{i}", OPS[op].0);
        match op {
            0 => self.publish(i),
            1 => self.call(i),
            2 => self.reply(i),
            3 => { let most = 1 + self.rng.below(OUT); self.flush(i, most); }
            4 => self.deliver_any(i),
            5 => self.tick(i),
            6 => self.time(i),
            7 => self.reboot(i),
            8 => self.renew(i),
            9 => self.rekey(i),
            10 => self.burst(i),
            _ => self.reflash(i),
        }
        for n in &self.nodes { assert!(n.p.sealer_ref().intact() && n.p.counts() == n.counts, "{}: intact, and {:?} counted", n.name, n.counts); }
    }

    /// Mostly a second; sometimes to the next deadline or another (a seen entry's or a call's expiry, a floor's rise, a
    /// window's week of silence, the last second a wire below its sender's newest epoch can be a straggler and the next),
    /// a few seconds, back a few, an hour, or past `MAX_TTL`, the warp a week of silence needs.
    fn time(&mut self, i: usize) {
        let n = &self.nodes[i];
        let week = |t: u32| t.saturating_add(MAX_TTL);
        let floors = n.slots.iter().flatten().flat_map(|s| [s.seen_at.map(week), Some(week(s.sec))]).flatten();
        let late = self.bag.iter().filter_map(|w| n.slots.iter().flatten().find(|s| s.sender == w.nonce.0 && s.epochs.first().is_some_and(|e| w.nonce.1 < e.0)).map(|s| s.epoch_at.saturating_add(w.ttl)));
        let deadlines: Vec<u32> = n.seen.keys().map(|k| k.1).chain(n.pending.values().map(|p| p.expires)).chain(floors).chain(late.flat_map(|t| [t, t.saturating_add(1)])).filter(|&t| t > n.now).collect();
        let next = if self.rng.below(2) == 0 { deadlines.iter().min() } else { deadlines.get(self.rng.below(deadlines.len())) }.map_or(1, |t| t - n.now);
        let now = n.now;
        let to = match self.rng.below(100) { 0..=59 => now + 1, 60..=79 => now + next, 80..=93 => now + 2 + self.rng.below(30) as u32, 94 => now.saturating_sub(1 + self.rng.below(30) as u32), 95..=96 => now + 32 + self.rng.below(3600) as u32, _ => now + MAX_TTL + self.rng.below(86_400) as u32 };
        self.nodes[i].clock(to);
    }

    fn publish(&mut self, i: usize) {
        let ttl = self.ttl();
        let n = &mut self.nodes[i];
        let want = n.can_send();
        assert_eq!(n.p.publish(n.now, "t", b"e", ttl), want);
        if want.is_ok() { n.queue("t", Kind::Event, ttl.clamp(1, MAX_TTL)); }
    }

    fn call(&mut self, i: usize) {
        let ttl = self.ttl();
        let n = &mut self.nodes[i];
        let want = if n.pending.len() == PENDING { Err(Error::Full) } else { n.can_send() };
        let got = n.p.call(n.now, "svc", "m", b"c", ttl);
        assert_eq!(got.map(|_| ()), want);
        if let Ok(got) = got {
            let ttl = ttl.clamp(1, MAX_TTL);
            let nonce = n.queue("svc", Kind::Call, ttl);
            assert_eq!(got, id(nonce), "the id is the nonce");
            n.pending.insert(nonce, Pend { expires: n.now.saturating_add(ttl), got: 0, end: None, steps: n.steps });
        }
    }

    /// One to three items of a stream this node is serving; `end` on its last.
    fn reply(&mut self, i: usize) {
        let n = &mut self.nodes[i];
        if n.served.is_empty() { return; }
        let j = self.rng.below(n.served.len());
        for _ in 0..=self.rng.below(3) {
            let s = &n.served[j];
            let (seq, end, kind, ttl, topic) = (s.next, s.next == s.last, Kind::Reply { call: s.call, seq: s.next, end: s.next == s.last }, s.ttl, s.reply_to.clone());
            let call = Call { id: id(s.call), sender: s.call.0, topic: "svc", method: "m", reply_to: &s.reply_to, ttl_secs: s.ttl, seq: 0, end: true, bytes: b"" };
            let want = n.can_send();
            assert_eq!(n.p.reply(n.now, &call, seq, 0, "", b"r", end), want);
            if want.is_err() { return; }
            n.queue(&topic, kind, ttl);
            if end { n.served.swap_remove(j); return; }
            n.served[j].next += 1;
        }
    }

    /// The transport takes up to `most` items; each opens under its key to the nonce the model says, once on the air.
    fn flush(&mut self, i: usize, most: usize) {
        let at: Vec<(u32, u32)> = self.nodes.iter().map(|n| (n.now, n.steps)).collect();
        let n = &mut self.nodes[i];
        for _ in 0..most {
            while n.out.front().is_some_and(|q| q.wire.is_some() && q.expires <= n.now) { n.out.pop_front(); }
            if self.rng.below(8) == 0 { n.p.sent(); } // nothing shown: a no-op
            if self.rng.below(8) == 0 { let _ = n.p.outgoing(n.now); } // a peek the transport refused
            let Some(o) = n.p.outgoing(n.now) else { assert!(n.out.is_empty(), "the peer's outbox is the model's"); return };
            let q = n.out.pop_front().expect("the model's outbox is the peer's");
            match (o, q.wire) {
                (Outgoing::Listen { topic }, None) => assert_eq!(topic, q.topic),
                (Outgoing::Send { topic, wire, ttl_secs }, Some(mut w)) => {
                    assert!(topic == w.topic && ttl_secs == w.ttl && q.expires > n.now, "no wire past its ttl");
                    (w.bytes, w.at, w.from) = (wire.to_vec(), at.clone(), i);
                    assert_eq!(nonce_of(&w), w.nonce, "the nonce is sender, epoch, counter");
                    assert!(self.aired.insert(w.nonce), "{:?} on the air twice", w.nonce);
                    if self.bag.len() == 256 { self.bag.remove(self.rng.below(256)); }
                    self.bag.push(w);
                }
                (o, w) => panic!("{o:?} where the model has {w:?}"),
            }
            n.p.sent();
        }
    }

    /// To one node: a radio's replay of its last wire; a wire at the top or the edge of the window it holds, or anywhere
    /// in two windows; a wire its seen table lets go within a second of now. Or from the bag a recent wire or any, to its
    /// topic's owner or anyone.
    fn deliver_any(&mut self, r: usize) {
        let (n, len, mode) = (&self.nodes[r], self.bag.len(), self.rng.below(5));
        let target = match mode {
            0 => return if let Some(w) = n.last.clone() { self.deliver(r, &w) },
            1 => n.slots[self.rng.below(SENDERS)].as_ref().and_then(|s| {
                let (e, high, _) = s.epochs.first()?;
                let behind = if self.rng.below(2) == 0 { [0, WINDOW - 1, WINDOW, WINDOW + 1][self.rng.below(4)] } else { self.rng.below(2 * WINDOW as usize + 1) as u32 };
                Some((s.sender, *e, high.saturating_sub(behind)))
            }),
            2 => { let edge: Vec<Nonce> = n.seen.keys().filter(|k| k.1.abs_diff(n.now) <= 1).map(|k| k.0).collect(); edge.get(self.rng.below(edge.len())).copied() }
            _ if len == 0 => return,
            3 => Some(self.bag[len - 1 - self.rng.below(len.min(8))].nonce),
            _ => Some(self.bag[self.rng.below(len)].nonce),
        };
        let Some(w) = target.and_then(|t| self.bag.iter().find(|w| w.nonce == t)).cloned() else { return };
        let owner = self.nodes.iter().position(|n| n.name == w.topic).filter(|_| mode > 2 && self.rng.below(4) != 0);
        self.deliver(owner.unwrap_or(r), &w);
    }

    fn deliver(&mut self, r: usize, w: &Wire) {
        let clone = w.from >= self.epochs.len(); // a reflash is a node past the fleet's own
        let n = &mut self.nodes[r];
        self.op = format!("deliver {:?} {:?} under key {} to {} at {}", w.nonce, w.kind, w.generation, n.name, n.now);
        if n.keys.contains(&Some(w.generation)) { n.wake(); }
        let e = n.expect(w);
        let (sender, caller, mut buf) = (w.nonce.0, format!("p{}", w.nonce.0 - 1), [0u8; 256]);
        let class = match n.p.deliver(n.now, &w.topic, &w.bytes, &mut buf) {
            Incoming::Call(c) => {
                assert!(w.kind == Kind::Call && c.id == id(w.nonce) && c.sender == sender && c.reply_to == caller && c.ttl_secs == w.ttl && (c.topic, c.method, c.bytes) == ("svc", "m", b"c"), "{c:?}");
                if n.served.len() == 16 { n.served.remove(0); }
                n.served.push(Served { call: w.nonce, reply_to: caller, ttl: w.ttl, next: 0, last: [0, 1 + self.rng.below(5) as u32][self.rng.below(2)] });
                Class::Exec
            }
            Incoming::Reply { id: got, sender: s, seq, end, status, detail, bytes } => { assert!(s == sender && (status, detail, bytes) == (0, "", b"r") && matches!(w.kind, Kind::Reply { call, seq: q, end: d } if id(call) == got && q == seq && d == end)); Class::Exec }
            Incoming::Event { topic, sender: s, bytes } => { assert!(w.kind == Kind::Event && s == sender && (topic, bytes) == ("t", b"e")); Class::Exec }
            Incoming::Rollback { sender: s } if s == sender => Class::Rollback,
            Incoming::Flood { sender: s } if s == sender => Class::Flood,
            Incoming::Dropped(Dropped::Duplicate) => Class::Duplicate,
            Incoming::Dropped(Dropped::Replay) => Class::Replay,
            Incoming::Dropped(Dropped::NoPending) => Class::NoPending,
            Incoming::Dropped(Dropped::Rate) => Class::Rate,
            Incoming::Dropped(Dropped::Open(seal::Error::Tag)) => Class::Tag,
            Incoming::Dropped(Dropped::Seq) => Class::Seq,
            other => panic!("{other:?}"),
        };
        *self.stats.classes.entry(class).or_default() += 1;
        // the oracle, sharing nothing with the window: a wire from no reflash, at a receiver that took none of its number's,
        // within its ttl, one epoch back at most, under a clock that never stepped back since it left or since its call
        if class == Class::Rollback {
            let heard: Vec<_> = n.heard.iter().filter(|h| h.0 == sender).collect();
            let fresh = w.at.get(r).is_some_and(|&(t, steps)| steps == n.steps && n.now < t.saturating_add(w.ttl));
            let since_call = if let Kind::Reply { call, .. } = w.kind { n.pending.get(&call).is_some_and(|p| p.steps == n.steps) } else { true };
            assert!(clone || heard.iter().any(|h| h.2) || heard.iter().filter(|h| h.1 > w.nonce.1).count() > 1 || !fresh || !since_call, "an honest wire named Rollback");
        }
        if class == Class::Exec { n.heard.insert((sender, w.nonce.1, clone)); }
        let delta = n.p.pressure() - n.pressure;
        n.pressure += delta;
        if e.maybe_dup && class == Class::Duplicate { assert_eq!(delta, 0); } else {
            assert_eq!(class, e.class, "the model's verdict");
            if let Some(v) = e.strangers { n.strangers = v; }
            if e.maybe_dup { n.seen.retain(|k, _| k.0 != w.nonce || k.1 <= n.now); } // the table had let it go
            match (class, e.write) {
                (Class::Exec, Some((k, rec))) => {
                    let again = !n.executed.insert(w.nonce);
                    assert!(!again || w.kind == Kind::Call || w.kind == Kind::Event, "a reply lands once");
                    if again && e.maybe_dup { self.stats.forgot += 1; } else if again { self.stats.late += 1; }
                    self.stats.evicted += u64::from(n.commit(w, k, rec, delta));
                }
                (_, Some((Some(k), rec))) => { n.slots[k] = Some(rec); assert_eq!(delta, 0); }
                _ => assert_eq!(delta, 0, "pressure counts an eviction or a refused window"),
            }
        }
        match class { Class::Rollback => n.counts.rollbacks += 1, Class::Flood => n.counts.floods += 1, Class::Exec | Class::Duplicate => {} _ => n.counts.dropped += 1 }
        n.last = Some(w.clone());
    }

    fn tick(&mut self, i: usize) {
        let n = &mut self.nodes[i];
        n.wake();
        let mut due: Vec<Id> = n.pending.iter().filter(|(_, p)| p.expires <= n.now).map(|(c, _)| id(*c)).collect();
        n.pending.retain(|_, p| p.expires > n.now);
        let mut got = Vec::new();
        while let Some(e) = n.p.tick(n.now) { let Incoming::Expired { id } = e else { panic!("{e:?}") }; got.push(id); }
        due.sort_by_key(|i| i.0);
        got.sort_by_key(|i| i.0);
        assert_eq!(got, due, "each pending call expires once, at its ttl");
        self.stats.expired += got.len();
    }

    /// A save, the copy resumed at the host's next epoch, the original gone; uptime restarts at 0, or a clock survives, as
    /// it always does in a lasting fleet. Half the saves are kept for `reflash`, the epoch after theirs left unused.
    fn reboot(&mut self, i: usize) {
        let (reset, keep) = (!self.lasting && self.rng.below(2) == 0, self.rng.below(2) == 0);
        let n = &mut self.nodes[i];
        let mut saved = n.copy();
        saved.p.disarm();
        assert_eq!(saved.p.publish(n.now, "t", b"e", 1), Err(Error::Corrupt), "a saved copy cannot send until resumed");
        let epoch = &mut self.epochs[n.sender as usize - 1];
        if keep { *epoch += 1; n.image = Some((Box::new(saved.copy()), *epoch)); }
        *epoch += 1;
        saved.image = n.image.take();
        saved.restart(*epoch, reset);
        *n = saved;
    }

    /// The host's next epoch for this roster number, which a reflash beside the device shares.
    fn renew(&mut self, i: usize) {
        let n = &mut self.nodes[i];
        assert_eq!(n.p.renew(n.epoch), Err(Error::Epoch));
        let epoch = &mut self.epochs[n.sender as usize - 1];
        *epoch += 1;
        (n.epoch, n.next) = (*epoch, 0);
        n.p.renew(n.epoch).unwrap();
    }

    /// A save a reboot kept, this node's or the next that has one, resumed beside the device at the epoch left for it: a
    /// reflash one epoch behind, its wires at receivers that may hold the device's newer epoch. Its clock survives with
    /// the device's, or restarts; it hears the calls the mesh holds and its answers reach their callers.
    fn reflash(&mut self, i: usize) {
        let reset = !self.lasting && self.rng.below(2) == 0;
        let Some(j) = (i..self.nodes.len()).chain(0..i).find(|&j| self.nodes[j].image.is_some()).filter(|_| self.nodes.len() < 6) else { return };
        let now = self.nodes[j].now;
        let (mut c, epoch) = self.nodes[j].image.take().unwrap();
        c.clock(now);
        c.restart(epoch, reset);
        self.nodes.push(*c);
        let c = self.nodes.len() - 1;
        let calls: Vec<Wire> = self.bag.iter().filter(|w| w.kind == Kind::Call).cloned().collect();
        for w in &calls { self.deliver(c, w); }
        for _ in 0..OUT { self.reply(c); self.flush(c, OUT); }
        let answers: Vec<Wire> = self.bag.iter().filter(|w| (w.nonce.0, w.nonce.1) == (self.nodes[c].sender, epoch) && matches!(w.kind, Kind::Reply { .. })).cloned().collect();
        for w in &answers { if let Some(r) = self.nodes.iter().position(|n| n.name == w.topic) { self.deliver(r, w); } }
    }

    /// Stage, rotate or retire on one node; nobody rotates more than one key ahead of the slowest.
    fn rekey(&mut self, i: usize) {
        let low = self.nodes.iter().filter_map(|n| n.keys[0]).min().unwrap();
        let n = &mut self.nodes[i];
        let current = n.keys[0].unwrap();
        match self.rng.below(3) {
            0 => { n.p.sealer_mut().stage(key(current + 1)); n.keys[2] = Some(current + 1); }
            1 if current == low => { n.p.sealer_mut().rotate(); if n.keys[2].is_some() { n.keys = [n.keys[2], n.keys[0], None]; } }
            _ => { n.p.sealer_mut().retire(); n.keys[1] = None; }
        }
        n.counts.managed += 1;
    }

    /// Many publishes from anyone delivered to one node whose clock stands still, so the per-second caps are reached.
    fn burst(&mut self, r: usize) {
        for _ in 0..MAX_PER_SEC as usize + self.rng.below(2 * RX_PER_SEC as usize) {
            let s = self.rng.below(self.nodes.len());
            if s != r && self.nodes[s].can_send() == Err(Error::Rate) { self.nodes[s].now += 1; }
            self.publish(s);
            self.flush(s, OUT);
            if let Some(w) = self.bag.last().filter(|w| w.nonce.0 == self.nodes[s].sender).cloned() { self.deliver(r, &w); }
        }
    }
}

fn run<const SENDERS: usize>(seed: u64) {
    let rounds = env("MUDRPC_FUZZ", 20_000);
    let mut w = World::<SENDERS> { nodes: Vec::new(), epochs: Vec::new(), bag: Vec::new(), aired: BTreeSet::new(), rng: Rng(seed), round: 0, life: 1, lasting: false, op: String::new(), stats: Stats::default() };
    let run = catch_unwind(AssertUnwindSafe(|| for _ in 0..rounds { w.round(); }));
    assert!(run.is_ok(), "MUDRPC_SEED={seed}: round {}, {}", w.round, w.op);
    let s = &w.stats;
    println!("seed {seed}, {rounds} rounds: {:?}, {} expired, {} seen evictions, {} run again after the seen table forgot them, {} after their ttl from a sender with no window", s.classes, s.expired, s.evicted, s.forgot, s.late);
    assert!(rounds < 20_000 || (s.classes.range(..Class::Seq).count() == 8 && s.forgot > 0 && s.expired > 0), "every verdict but the rare `Seq` reached, and the forgetting path");
}

/// One window for up to four senders: a sender refused one, its seen entries its only memory.
#[test]
fn the_peer_keeps_its_promises_under_random_interleavings_against_a_model() { run::<1>(env("MUDRPC_SEED", 1)); }

/// Two windows, so the Kademlia rule chooses among slots.
#[test]
fn the_peer_keeps_its_promises_from_a_second_seed() { run::<2>(env("MUDRPC_SEED", 1) ^ 0x5eed); }

/// Six windows for up to seven peers, so a spent second has senders to name past their share.
#[test]
fn the_peer_keeps_its_promises_where_a_spent_second_has_senders_to_name() { run::<6>(env("MUDRPC_SEED", 1) ^ 0x5a1e); }
