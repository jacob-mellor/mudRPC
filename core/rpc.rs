//! The peer: a sans-IO state machine over the wire.
//!
//! Pending calls, seen messages, known senders, an outbox. Plain
//! `repr(C)` integers and arrays with explicit flags, so the struct is its own save file and `resume` can check every
//! byte that decides a verdict. No randomness: the nonce is `sender ‖ epoch ‖ counter`, and the epoch is the host's persisted number.
//! `deliver` is three named stages: `judge` reaches every verdict public facts allow on copies, the sealer's `verify`
//! runs once, and `mark` applies the rate caps and every write in one order, so a mark can never precede a verdict.
use crate::codec::le;
use crate::envelope::{self, Body, Envelope, Id};
use crate::seal::{self, Opened, Sealer, Slot, NONCE, TAG};

/// Longest topic, in bytes; a peer's name is a topic.
pub const MAX_TOPIC: usize = 64;
/// Wire format version of the unsigned wire: the one plaintext byte, authenticated as associated data with the topic. A
/// sealer names the version it writes (`Sealer::VERSION`); this is `Key`'s.
pub const VERSION: u8 = 1;
/// Longest lifetime a message may claim; longer is clamped, so no seen entry is permanent.
pub const MAX_TTL: u32 = 7 * 86_400;
/// Messages per second a receiver takes from one sender, and from every sender with no window together: what one sender
/// number can push, whichever number a key holder writes.
pub const MAX_PER_SEC: u32 = 64;
/// Messages per second a peer sends: half `MAX_PER_SEC`, since a receiver's second, not lined up with the sender's, can
/// take two of the sender's.
pub const SEND_PER_SEC: u32 = MAX_PER_SEC / 2;
/// Accepted calls and events per second across all senders: the hard bound against a key holder rotating sender numbers.
///
/// A reply to one of our calls is outside it, already bounded by `PENDING` × `MAX_SEQ`.
pub const RX_PER_SEC: u32 = 4 * MAX_PER_SEC;
/// Items a stream may carry (seq 0..64); a bitmap records which arrived.
pub const MAX_SEQ: u32 = 64;
/// Counters a receiver remembers per sender below the highest seen; a wire further behind is a replay.
pub const WINDOW: u32 = 64;
/// Layout version of the saved block; changes whenever a field moves.
const MAGIC: u32 = 0x6d75_643c;
const NONE: u32 = u32::MAX;

/// Why the peer refused.
///
/// `Full`: no pending slot or no outbox slot; `TooLarge`: a topic over `MAX_TOPIC`, a message over `MSG` or a seq at
/// `MAX_SEQ`; `Corrupt`: a block `resume` cannot accept, or one never resumed; `Epoch`: not above the current one;
/// `Unprovisioned`: sender 0; `Exhausted`: the counter under this epoch is spent; `Rate`: this second's `SEND_PER_SEC` sends;
/// `Empty`: a name or topic of no bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub enum Error { Full, TooLarge, Corrupt, Epoch, Unprovisioned, Exhausted, Rate, Empty }

/// What the transport takes next: a sealed wire for a topic, or a subscription to one.
#[derive(Debug, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub enum Outgoing<'a> { Send { topic: &'a str, wire: &'a [u8], ttl_secs: u32 }, Listen { topic: &'a str } }

/// A call as delivered: who sent it, where, and what `reply` needs back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // the fields are the envelope's, plus the sender and the topic
pub struct Call<'a> { pub id: Id, pub sender: u32, pub topic: &'a str, pub method: &'a str, pub reply_to: &'a str, pub ttl_secs: u32, pub seq: u32, pub end: bool, pub bytes: &'a [u8] }

/// Why a delivered wire never reached product code. Log every one in development; count them in the field.
///
/// `Topic`: the wire's, or a call's `reply_to`, empty or over `MAX_TOPIC`; `Seq`: beyond `MAX_SEQ` or a stream's lowest `end`, or not 0 on a call; `Id`: the field
/// disagrees with the header; `NoPending`: a reply to no call of ours; `Replay`: behind the sender's window or floor;
/// `Rate`: over `MAX_PER_SEC` or `RX_PER_SEC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub enum Dropped { Version(u8), Open(seal::Error), Decode(envelope::Error), Topic, Seq, Id, Duplicate, NoPending, Replay, Rate }

/// What `deliver` and `tick` yield to product code.
#[derive(Debug, PartialEq, Eq)]
#[allow(missing_docs)] // the fields are the envelope's, plus the sender and the topic
pub enum Incoming<'a> {
    /// For this peer to answer with `reply`.
    Call(Call<'a>),
    /// To a call of ours; `end` closes the stream.
    Reply { id: Id, sender: u32, seq: u32, end: bool, status: u32, detail: &'a str, bytes: &'a [u8] },
    /// On a topic this peer listens to.
    Event { topic: &'a str, sender: u32, bytes: &'a [u8] },
    /// A call of ours whose ttl passed with the stream open.
    Expired { id: Id },
    /// A sender above `MAX_PER_SEC` this second, or above its share of a second the global cap spent, or the one whose
    /// wire came after the senders with no window landed their shared `MAX_PER_SEC`: the fleet past its windows, not a
    /// flooder. Reported once per second, the rest are `Dropped::Rate`.
    Flood { sender: u32 },
    /// A sender whose epoch is below the floor we hold for it, below both epochs we have heard, or below the newest past
    /// the wire's ttl since we first heard that: a cloned or rolled-back device. Reached before the signature under a
    /// signed fleet, so the number is the one written. False alarms: a wire held across two reboots or past its ttl across
    /// one, and a reply to a call made before our clock stepped back, the newest epoch first heard after the step.
    Rollback { sender: u32 },
    /// A wire refused, and why.
    Dropped(Dropped),
}

/// What an operator watches beside `pressure`: four saturating counters kept in the block across `resume`.
///
/// A `managed` that rises when nobody ordered a change is the dwell signal for an implant rekeying the unit.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// The sealer taken for a change: a key staged, rotated or retired, a roster line added or removed.
    pub managed: u32,
    /// `Rollback` verdicts.
    pub rollbacks: u32,
    /// `Flood` verdicts.
    pub floods: u32,
    /// `Dropped` verdicts but `Duplicate`, which a mesh that delivers twice makes when nothing is wrong.
    pub dropped: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Name { bytes: [u8; MAX_TOPIC], len: u8 }

impl Name {
    const EMPTY: Self = Self { bytes: [0; MAX_TOPIC], len: 0 };
    fn new(s: &str) -> Result<Self, Error> {
        if s.is_empty() { return Err(Error::Empty); }
        let mut n = Self::EMPTY;
        n.bytes.get_mut(..s.len()).ok_or(Error::TooLarge)?.copy_from_slice(s.as_bytes());
        n.len = s.len() as u8;
        Ok(n)
    }
    fn str(&self) -> Option<&str> { core::str::from_utf8(self.bytes.get(..self.len as usize)?).ok() }
}

// Every struct in the block names its padding, zeroes it and asserts its size is the sum of its fields: a byte the
// compiler pads is a byte nobody wrote, and the block is a save file.
#[repr(C)]
#[derive(Clone, Copy)]
struct Pend { got: u64, expires: u32, end: u32, id: Id, live: u8, _pad: [u8; 3] }
const _: () = assert!(size_of::<Pend>() == 8 + 4 + 4 + 12 + 1 + 3);

impl Pend {
    const EMPTY: Self = Self { got: 0, expires: 0, end: NONE, id: Id([0; 12]), live: 0, _pad: [0; 3] };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Seen { expires: u32, nonce: [u8; 12], live: u8, _pad: [u8; 3] }
const _: () = assert!(size_of::<Seen>() == 4 + 12 + 1 + 3);

/// What a receiver knows of one sender: the highest (epoch, counter) with a map of the WINDOW counters below it, for the
/// two newest epochs heard (older is a rollback: `resume` kills queued wires); when the highest was seen; the floor; this
/// second's count; when the newest epoch was first heard.
#[repr(C)]
#[derive(Clone, Copy)]
struct Sender { bits: [u64; 2], id: u32, high: [[u32; 2]; 2], seen_at: u32, floor: [u32; 2], sec: u32, count: u32, epoch_at: u32, live: u8, flooded: u8, _pad: [u8; 2] }
const _: () = assert!(size_of::<Sender>() == 16 + 4 + 16 + 4 + 8 + 4 + 4 + 4 + 1 + 1 + 2);

impl Sender {
    const fn fresh(id: u32, now: u32) -> Self { Self { bits: [0; 2], id, high: [[0; 2]; 2], seen_at: NONE, floor: [0; 2], sec: now, count: 0, epoch_at: now, live: 1, flooded: 0, _pad: [0; 2] } }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Queued<const MSG: usize> { len: usize, ttl: u32, expires: u32, topic: Name, live: u8, listen: u8, _pad: [u8; 5], wire: [u8; MSG] }

impl<const MSG: usize> Queued<MSG> {
    const EMPTY: Self = Self { len: 0, ttl: 0, expires: 0, topic: Name::EMPTY, live: 0, listen: 0, _pad: [0; 5], wire: [0; MSG] };
}

/// gRPC's status names by number, as C strings for the ABI; `status_name` gives the text.
pub const STATUS_NAMES: [&core::ffi::CStr; 17] = [c"OK", c"CANCELLED", c"UNKNOWN", c"INVALID_ARGUMENT", c"DEADLINE_EXCEEDED", c"NOT_FOUND", c"ALREADY_EXISTS", c"PERMISSION_DENIED", c"RESOURCE_EXHAUSTED", c"FAILED_PRECONDITION", c"ABORTED", c"OUT_OF_RANGE", c"UNIMPLEMENTED", c"INTERNAL", c"UNAVAILABLE", c"DATA_LOSS", c"UNAUTHENTICATED"];

/// gRPC's name for a status number, `"FAILED_PRECONDITION"` for 9; empty past 16.
#[must_use]
pub fn status_name(status: u32) -> &'static str { STATUS_NAMES.get(status as usize).and_then(|n| n.to_str().ok()).unwrap_or("") }

/// The peer; `repr(C)`, so its bytes are its save file.
///
/// `MSG`: the sealed message size; `PENDING`: calls in flight; `SEEN`: messages remembered against replay; `SENDERS`:
/// senders kept a window, a floor and a rate, at least the sealer's `ROSTER`, none evicted while heard within `MAX_TTL`;
/// `OUT`: wires queued for the transport.
#[repr(C)]
pub struct Peer<S: Sealer, const MSG: usize, const PENDING: usize, const SEEN: usize, const SENDERS: usize, const OUT: usize> {
    magic: u32,
    epoch: u32,
    next: u64,
    pending: [Pend; PENDING],
    senders: [Sender; SENDERS],
    out: [Queued<MSG>; OUT],
    sender: u32,
    pressure: u32,
    counts: Counts,
    sec: u32,
    sent_this_sec: u32,
    rx_sec: u32,
    rx_count: u32,
    seen: [Seen; SEEN],
    sealer: S,
    name: Name,
    opened: u8,
    /// Whether `outgoing` has shown the head since the last `sent`: `sent` takes only what was shown.
    shown: u8,
    /// Set by `new` and `resume`, cleared in the copy a save makes: a block restored without `resume` cannot send.
    armed: u8,
    /// The ABI's detector of a second caller on one block, set while an entry runs (`hold`); 0 in a block saved before it.
    busy: u8,
    _pad: [u8; 1],
    /// This second's calls and events landed from senders with no window and none to take, who share one sender's cap, counted on past it; was padding, so 0 in a block saved before it.
    strangers: u16,
    out_head: usize,
    out_len: usize,
}

/// What `judge` leaves for `mark`: the sender's record as it would be, where it lives if it does, the dedupe entry.
#[derive(Clone, Copy)]
struct Judged { sender: u32, k: Option<usize>, record: Sender, pend: Option<(usize, u64)>, nonce: [u8; 12], ttl: u32, seq: u32, end: bool }

/// Associated data: version ‖ topic, a topic of 1 to `MAX_TOPIC` bytes.
fn ad(version: u8, topic: &str) -> Result<([u8; MAX_TOPIC + 1], usize), Error> {
    if topic.is_empty() { return Err(Error::Empty); }
    let mut ad = [version; MAX_TOPIC + 1];
    ad.get_mut(1..1 + topic.len()).ok_or(Error::TooLarge)?.copy_from_slice(topic.as_bytes());
    Ok((ad, 1 + topic.len()))
}

/// One more in this second, where a second begins only when the clock moves forward: a clock that steps back opens no
/// budget. The one rule for sends, receipts and every sealer's count.
#[must_use]
pub const fn tally(now: u32, sec: u32, count: u32) -> (u32, u32) { if now > sec { (now, 1) } else { (sec, count.saturating_add(1)) } }

impl<S: Sealer, const MSG: usize, const PENDING: usize, const SEEN: usize, const SENDERS: usize, const OUT: usize> Peer<S, MSG, PENDING, SEEN, SENDERS, OUT> {
    /// MSG is the sealed message size on the wire, 64..=65535 and above the sealer's overhead; capacities are at least 1.
    /// The block has no padding, so MSG is a multiple of 8, SEEN and OUT are even and the sealer is a multiple of 8 bytes.
    /// `sender` is the roster number (0: unprovisioned, cannot send). `epoch` is the host's persisted counter, written
    /// to storage before this returns.
    pub fn new(name: &str, sealer: S, sender: u32, epoch: u32) -> Result<Self, Error> {
        const { assert!(MSG >= 64 && MSG <= 65535 && MSG > 1 + NONCE + TAG + S::TRAILER && PENDING > 0 && SEEN > 0 && SENDERS > 0 && OUT > 0) }
        const { assert!(SENDERS >= S::ROSTER, "a window for every roster line, so no stranger takes a believed sender's") }
        const { assert!(size_of::<Queued<MSG>>() == size_of::<usize>() + 8 + MAX_TOPIC + 1 + 2 + 5 + MSG) }
        const { assert!(size_of::<Self>() == 16 + PENDING * size_of::<Pend>() + SENDERS * size_of::<Sender>() + OUT * size_of::<Queued<MSG>>() + 8 + 16 + 16 + SEEN * size_of::<Seen>() + size_of::<S>() + MAX_TOPIC + 1 + 4 + 1 + 2 + 2 * size_of::<usize>()) }
        let (known, seen) = (Sender { live: 0, ..Sender::fresh(0, 0) }, Seen { expires: 0, nonce: [0; 12], live: 0, _pad: [0; 3] });
        let mut p = Self { magic: MAGIC, epoch, next: 0, pending: [Pend::EMPTY; PENDING], senders: [known; SENDERS], out: [Queued::EMPTY; OUT], sender, pressure: 0, counts: Counts::default(), sec: 0, sent_this_sec: 0, rx_sec: 0, rx_count: 0, seen: [seen; SEEN], sealer, name: Name::new(name)?, opened: 0, shown: 0, armed: 1, busy: 0, _pad: [0; 1], strangers: 0, out_head: 0, out_len: 0 };
        p.listen(name)?;
        Ok(p)
    }

    /// After restoring a peer from its saved bytes: check every flag, bound and invariant, then take the host's new epoch,
    /// which must be above the stored one. Windows and floors survive; the uptimes they were seen and their newest epochs
    /// first heard at, and this second's counts, do not. Uptime restarted, so what was in flight is of unknown age: pending calls expire on the first tick,
    /// queued wires at the exit. The peer's own listen is queued again, unless the outbox is full; other listens are the host's.
    pub fn resume(&mut self, epoch: u32) -> Result<(), Error> {
        let flag = |f: u8| f <= 1;
        let ok = self.magic == MAGIC && self.name.str().is_some_and(|n| !n.is_empty()) && self.sealer.intact() && self.out_head < OUT && self.out_len <= OUT && self.opened < 3 && flag(self.shown) && flag(self.armed)
            && self.pending.iter().all(|p| flag(p.live) && (p.end == NONE || p.end < MAX_SEQ))
            && self.seen.iter().all(|s| flag(s.live))
            && self.senders.iter().all(|s| flag(s.live) && flag(s.flooded) && s.floor <= s.high[0] && (s.high[1][0] < s.high[0][0] || s.high[1] == [0; 2]))
            && self.out.iter().all(|s| flag(s.live) && flag(s.listen) && s.len <= MSG && s.topic.str().is_some_and(|t| s.live == 0 || !t.is_empty()));
        if !ok { return Err(Error::Corrupt); }
        self.renew(epoch)?;
        for s in &mut self.senders { (s.seen_at, s.sec, s.count, s.flooded) = (NONE, NONE, 0, 0); }
        self.sealer.resume();
        (self.sec, self.sent_this_sec, self.rx_sec, self.rx_count, self.strangers, self.shown, self.busy) = (0, 0, 0, 0, 0, 0, 0); // busy, any value: set by an entry a crash ended
        for p in &mut self.pending { p.expires = 0; }
        for s in &mut self.out { s.expires = 0; }
        self.armed = 1;
        let _ = self.push(Queued { live: 1, listen: 1, topic: self.name, ..Queued::EMPTY });
        Ok(())
    }

    /// A new epoch from the host, above the current one; clears `Exhausted`. The host has written it to storage first.
    pub const fn renew(&mut self, epoch: u32) -> Result<(), Error> {
        if epoch <= self.epoch { return Err(Error::Epoch); }
        (self.epoch, self.next) = (epoch, 0);
        Ok(())
    }

    /// Live seen or sender entries evicted, or senders refused a window, for want of room. Rising under load means replay
    /// protection is degraded.
    pub const fn pressure(&self) -> u32 { self.pressure }
    /// Which key slot opened the last wire that reached product code.
    pub fn opened(&self) -> Slot { seal::SLOTS.get(self.opened as usize).copied().unwrap_or(Slot::Current) }
    /// Whether these bytes carry this layout's magic: what a host that copies a block in without `resume` is checked against.
    pub const fn live(&self) -> bool { self.magic == MAGIC }
    /// For the copy a host writes to storage: it cannot send until `resume` arms it, so a block restored without
    /// resuming refuses rather than reusing its nonces.
    pub const fn disarm(&mut self) { self.armed = 0; }
    /// The sealer, to change it: a key staged, rotated or retired, a roster line added or removed. Counted in `counts().managed`.
    pub const fn sealer_mut(&mut self) -> &mut S { self.counts.managed = self.counts.managed.saturating_add(1); &mut self.sealer }
    /// The sealer, to read it.
    pub const fn sealer_ref(&self) -> &S { &self.sealer }
    /// The operator's counters.
    pub const fn counts(&self) -> Counts { self.counts }
    /// Calls in flight, `Expired` ones not yet ticked included: at `PENDING`, `call` is `Full`.
    pub fn pending(&self) -> u32 { self.pending.iter().filter(|p| p.live == 1).count() as u32 } // at most PENDING
    /// Items in the outbox, listens and wires past their ttl not yet reached by `outgoing` included: at `OUT`, every producer is `Full`.
    pub const fn queued(&self) -> u32 { self.out_len as u32 } // at most OUT
    /// Sets or clears the busy byte and says whether it was set, any value but 0, which a set leaves as it was: the ABI's
    /// detector of two threads on one block, held while each entry that takes it runs and cleared by `resume`. Not a lock,
    /// and Rust needs none: `&mut self` rules a second caller out.
    pub fn hold(&mut self, on: bool) -> bool { let was = self.busy != 0; if !(on && was) { self.busy = u8::from(on); } was }

    /// A unary call to `service`; the id names its replies, or its `Expired`. `ttl_secs` is clamped to 1..=`MAX_TTL`.
    pub fn call(&mut self, now: u32, service: &str, method: &str, req: &[u8], ttl_secs: u32) -> Result<Id, Error> {
        let ttl_secs = ttl_secs.clamp(1, MAX_TTL);
        let free = self.pending.iter().position(|p| p.live == 0).ok_or(Error::Full)?;
        let reply_to = self.name;
        let id = self.send(now, service, &Envelope { id: None, ttl_secs, seq: 0, end: true, body: Body::Call { method, reply_to: reply_to.str().unwrap_or(""), payload: req } })?;
        if let Some(p) = self.pending.get_mut(free) { *p = Pend { live: 1, id, expires: now.saturating_add(ttl_secs), ..Pend::EMPTY }; }
        Ok(id)
    }

    /// An event on `topic`; nothing comes back.
    pub fn publish(&mut self, now: u32, topic: &str, bytes: &[u8], ttl_secs: u32) -> Result<(), Error> {
        self.send(now, topic, &Envelope { id: None, ttl_secs: ttl_secs.clamp(1, MAX_TTL), seq: 0, end: false, body: Body::Event { payload: bytes } }).map(|_| ())
    }

    /// Subscribes to `topic`: the transport is told on the next `outgoing`.
    pub fn listen(&mut self, topic: &str) -> Result<(), Error> {
        self.push(Queued { live: 1, listen: 1, topic: Name::new(topic)?, ..Queued::EMPTY })
    }

    /// Answers a call with gRPC's status number, its detail (empty when there is none) and the payload. Stream: seq
    /// 0..`MAX_SEQ`, `end` on the last; the reply lives as long as the call did.
    #[allow(clippy::too_many_arguments)] // the call, its place in a stream, and gRPC's status, detail and payload
    pub fn reply(&mut self, now: u32, call: &Call, seq: u32, status: u32, detail: &str, bytes: &[u8], end: bool) -> Result<(), Error> {
        if seq >= MAX_SEQ { return Err(Error::TooLarge); }
        self.send(now, call.reply_to, &Envelope { id: Some(call.id), ttl_secs: call.ttl_secs.clamp(1, MAX_TTL), seq, end, body: Body::Reply { status, detail, payload: bytes } }).map(|_| ())
    }

    /// Inbound from the transport. Every wire gets a verdict: the tag first, with nothing before it touching a table;
    /// then `judge`, every verdict public facts allow, on copies; then the sealer's `verify`, once; then `mark`. A wire
    /// refused after the tag leaves `buf` zeroed where its plaintext was.
    pub fn deliver<'a>(&mut self, now: u32, topic: &'a str, wire: &[u8], buf: &'a mut [u8]) -> Incoming<'a> {
        let v = self.verdict(now, topic, wire, buf);
        let c = &mut self.counts;
        match v { Incoming::Rollback { .. } => c.rollbacks = c.rollbacks.saturating_add(1), Incoming::Flood { .. } => c.floods = c.floods.saturating_add(1), Incoming::Dropped(d) if d != Dropped::Duplicate => c.dropped = c.dropped.saturating_add(1), _ => {} }
        v
    }

    fn verdict<'a>(&mut self, now: u32, topic: &'a str, wire: &[u8], buf: &'a mut [u8]) -> Incoming<'a> {
        let Some((&v, sealed)) = wire.split_first() else { return Incoming::Dropped(Dropped::Open(seal::Error::Short)) };
        if v != S::VERSION { return Incoming::Dropped(Dropped::Version(v)); }
        let Ok((ad_all, al)) = ad(v, topic) else { return Incoming::Dropped(Dropped::Topic) };
        let Some(ad) = ad_all.get(..al) else { return Incoming::Dropped(Dropped::Topic) };
        let Opened { len, nonce, slot } = match self.sealer.open(ad, sealed, buf) { Ok(o) => o, Err(e) => return Incoming::Dropped(Dropped::Open(e)) };
        let admitted: Result<(usize, u32, Id, u32, bool), Incoming<'static>> = 'admit: { // every verdict after the tag, for one funnel
            for s in &mut self.senders { if s.sec == NONE { (s.sec, s.epoch_at) = (now, now); } } // unknown since `resume`: from a second this peer saw; in the clock's last the stamp is `NONE` again, which only widens that second
            let Some(n) = len.checked_sub(S::TRAILER) else { break 'admit Err(Incoming::Dropped(Dropped::Open(seal::Error::Short))) };
            let Some(first) = buf.get(..n) else { break 'admit Err(Incoming::Dropped(Dropped::Open(seal::Error::Full))) };
            let e = match Envelope::decode(first) { Ok(e) => e, Err(e) => break 'admit Err(Incoming::Dropped(Dropped::Decode(e))) };
            let (ttl, id, seq, end) = (e.ttl_secs.clamp(1, MAX_TTL), e.id.unwrap_or(Id(nonce)), e.seq, e.end);
            let judged = match self.judge(now, &e, id, nonce, ttl) { Ok(j) => j, Err(verdict) => break 'admit Err(verdict) };
            let Some(whole) = buf.get_mut(..len) else { break 'admit Err(Incoming::Dropped(Dropped::Open(seal::Error::Full))) };
            if let Err(e) = self.sealer.verify(now, ad, &nonce, whole) { break 'admit Err(Incoming::Dropped(Dropped::Open(e))); }
            if let Err(verdict) = self.mark(now, judged, slot) { break 'admit Err(verdict); }
            Ok((n, ttl, id, seq, end))
        };
        let (n, ttl, id, seq, end) = match admitted { Ok(j) => j, Err(verdict) => { if let Some(b) = buf.get_mut(..len) { b.fill(0); } return verdict; } }; // a wire refused after it opened leaves no plaintext behind
        // the product's view, decoded again because the verdicts' borrow had to end: the funnel read and decoded these bytes
        // and `verify` left them as they were, so the else, which would leave the plaintext, is never taken
        let Some(Ok(e)) = buf.get(..n).map(Envelope::decode) else { return Incoming::Dropped(Dropped::Open(seal::Error::Full)) };
        let sender = le(&nonce[0..4]) as u32;
        match e.body {
            Body::Reply { status, detail, payload } => Incoming::Reply { id, sender, seq, end, status, detail, bytes: payload },
            Body::Call { method, reply_to, payload } => Incoming::Call(Call { id, sender, topic, method, reply_to, ttl_secs: ttl, seq, end, bytes: payload }),
            Body::Event { payload } => Incoming::Event { topic, sender, bytes: payload },
        }
    }

    /// After the tag: dedupe (a reply against its call's bitmap, everything else against the seen table; a call is seq 0
    /// and its id is its nonce, since the server will sign a reply under that id), then the sender's floor and window on
    /// a copy of its record. Reads only; a wire refused here or by `verify` leaves no mark.
    fn judge(&self, now: u32, e: &Envelope, id: Id, nonce: [u8; 12], ttl: u32) -> Result<Judged, Incoming<'static>> {
        let pend = match e.body {
            Body::Reply { .. } => {
                let Some((i, p)) = self.pending.iter().enumerate().find(|(_, p)| p.live == 1 && p.id == id) else { return Err(Incoming::Dropped(Dropped::NoPending)) };
                if e.seq >= MAX_SEQ || e.seq > p.end { return Err(Incoming::Dropped(Dropped::Seq)); }
                if p.got & (1u64 << e.seq) != 0 { return Err(Incoming::Dropped(Dropped::Duplicate)); }
                Some((i, 1u64 << e.seq))
            }
            Body::Call { reply_to, .. } if Name::new(reply_to).is_err() => return Err(Incoming::Dropped(Dropped::Topic)), // a call nobody could answer
            Body::Call { .. } if e.seq != 0 => return Err(Incoming::Dropped(Dropped::Seq)),
            Body::Call { .. } if e.id.is_some_and(|i| i.0 != nonce) => return Err(Incoming::Dropped(Dropped::Id)),
            _ => { if self.seen.iter().any(|s| s.live == 1 && s.expires > now && s.nonce == nonce) { return Err(Incoming::Dropped(Dropped::Duplicate)); } None }
        };
        let (sender, at) = (le(&nonce[0..4]) as u32, [le(&nonce[4..8]) as u32, le(&nonce[8..12]) as u32]);
        let k = self.senders.iter().position(|s| s.live == 1 && s.id == sender);
        let mut s = k.and_then(|k| self.senders.get(k)).copied().unwrap_or_else(|| Sender::fresh(sender, now));
        if s.seen_at.checked_add(MAX_TTL).is_some_and(|t| now >= t) { s.floor = s.high[0]; } // a deadline past the clock's end, `NONE`'s included, never comes
        if at < s.floor { return Err(if at[0] < s.floor[0] { Incoming::Rollback { sender } } else { Incoming::Dropped(Dropped::Replay) }); }
        if at > s.high[0] || s.seen_at == NONE { s.seen_at = now; }
        if at[0] > s.high[0][0] { (s.high, s.bits, s.epoch_at) = ([[at[0], 0], s.high[0]], [0, s.bits[0]], now); }
        else if s.high[1][0] < at[0] && at[0] < s.high[0][0] { (s.high[1], s.bits[1]) = ([at[0], 0], 0); }
        let Some(w) = s.high.iter().position(|h| h[0] == at[0]) else { return Err(Incoming::Rollback { sender }) };
        let (high, bits) = (&mut s.high[w][1], &mut s.bits[w]);
        if at[1] > *high { (*bits, *high) = (bits.checked_shl(at[1] - *high).unwrap_or(0) | 1, at[1]); }
        else if *high - at[1] >= WINDOW { return Err(Incoming::Dropped(Dropped::Replay)); }
        else if *bits >> (*high - at[1]) & 1 == 1 { return Err(Incoming::Dropped(Dropped::Duplicate)); }
        else { *bits |= 1 << (*high - at[1]); }
        let by = pend.and_then(|(i, _)| self.pending.get(i)).map_or(now, |p| p.expires.max(now)); // a reply is sent after its call, which expires a ttl after it was made
        if w == 1 && by > s.epoch_at.saturating_add(ttl) { return Err(Incoming::Rollback { sender }); } // sent before the newest epoch's first wire, so due within its ttl of it
        if s.flooded == 1 && tally(now, s.sec, s.count).1 > MAX_PER_SEC { return Err(Incoming::Dropped(Dropped::Rate)); } // already reported this second: refused before the sealer spends anything
        Ok(Judged { sender, k, record: s, pend, nonce, ttl, seq: e.seq, end: e.end })
    }

    /// After `verify`: the global, per-sender and strangers' rate verdicts, still on copies, then every write in one order:
    /// the sender's record and slot, the receive counts, the key slot that opened, the dedupe entry. A wire over a cap keeps
    /// only its sender's count, or the strangers' once past theirs, takes no slot and sets no window bit, so it lands in a calmer second.
    fn mark(&mut self, now: u32, j: Judged, slot: Slot) -> Result<(), Incoming<'static>> {
        let Judged { sender, k, mut record, pend, nonce, ttl, seq, end } = j;
        let (rx_sec, rx) = if pend.is_some() { (self.rx_sec, self.rx_count) } else { tally(now, self.rx_sec, self.rx_count) };
        let (sec, count) = tally(now, record.sec, record.count);
        if sec != record.sec { record.flooded = 0; }
        (record.sec, record.count) = (sec, count);
        let window = k.or_else(|| self.sender_slot(now));
        let stranger = window.is_none() && pend.is_none(); // no window and none to take: every such call and event counts as one sender's
        let strangers = if rx_sec == self.rx_sec { self.strangers } else { 0 }.saturating_add(u16::from(stranger));
        if rx > RX_PER_SEC || count > MAX_PER_SEC || stranger && u32::from(strangers) > MAX_PER_SEC {
            // a sender with a window, over its own cap or an equal share of a spent second: the cap over the windows heard in it,
            // its own among them from its first wire, after which the table holds it; no division: a divide on a core without one is a panic path
            let heard = || self.senders.iter().filter(|s| s.live == 1 && s.sec == rx_sec && s.count > 0).count() as u64 + u64::from(sec != rx_sec || count == 1);
            let named = k.is_some() && (count > MAX_PER_SEC || u64::from(count) * heard() > u64::from(RX_PER_SEC)) || stranger && u32::from(strangers) == MAX_PER_SEC + 1;
            let verdict = if named && record.flooded == 0 { record.flooded = 1; Incoming::Flood { sender } } else { Incoming::Dropped(Dropped::Rate) };
            if let Some(known) = k.and_then(|k| self.senders.get_mut(k)) { (known.sec, known.count, known.flooded) = (record.sec, record.count, record.flooded); }
            if stranger && u32::from(strangers) > MAX_PER_SEC { self.strangers = strangers; } // the strangers count what landed; past their 64 the count runs on, so the 65th is named once
            return Err(verdict);
        }
        if k.is_none() && !self.senders.iter().any(|s| s.live == 0) { self.pressure = self.pressure.saturating_add(1); } // a window evicted or refused
        if let Some(known) = window.and_then(|k| self.senders.get_mut(k)) { *known = record; }
        (self.rx_sec, self.rx_count, self.strangers, self.opened) = (rx_sec, rx, strangers, slot as u8);
        match pend {
            Some((i, bit)) => {
                let Some(p) = self.pending.get_mut(i) else { return Err(Incoming::Dropped(Dropped::NoPending)) };
                p.got |= bit;
                if end { p.end = p.end.min(seq); }
                if p.end != NONE && { let all = u64::MAX >> (MAX_SEQ - 1 - p.end); p.got & all == all } { p.live = 0; }
            }
            None => self.remember(nonce, now, ttl),
        }
        Ok(())
    }

    /// Time passes. Call until None.
    pub fn tick(&mut self, now: u32) -> Option<Incoming<'static>> {
        for s in &mut self.senders { if s.sec == NONE { (s.sec, s.epoch_at) = (now, now); } }
        let p = self.pending.iter_mut().find(|p| p.live == 1 && p.expires <= now)?;
        p.live = 0;
        Some(Incoming::Expired { id: p.id })
    }

    /// Next item for the transport, lent from the outbox and left in place until `sent()`.
    pub fn outgoing(&mut self, now: u32) -> Option<Outgoing<'_>> {
        while self.out_len > 0 && self.out.get(self.out_head).is_some_and(|s| s.listen == 0 && s.expires <= now) { self.pop(); }
        self.shown = u8::from(self.out_len > 0);
        let s = self.out.get(self.out_head).filter(|_| self.out_len > 0)?;
        let topic = s.topic.str().unwrap_or("");
        Some(if s.listen == 1 { Outgoing::Listen { topic } } else { Outgoing::Send { topic, wire: s.wire.get(..s.len)?, ttl_secs: s.ttl } })
    }

    /// The transport has taken the item `outgoing` last showed; with nothing shown, nothing happens.
    pub fn sent(&mut self) {
        if self.shown == 1 && self.out_len > 0 { self.pop(); }
        self.shown = 0;
    }

    /// Seals and queues one envelope under the next nonce; returns the nonce as the message's id. The plaintext scratch
    /// is zeroed before this returns: the sealed wire is what the outbox holds, and nothing else of the message stays.
    #[allow(clippy::debug_assert_with_mut_call)] // the debug check opens our own wire; no sealer counts in `open`
    fn send(&mut self, now: u32, topic: &str, e: &Envelope) -> Result<Id, Error> {
        if self.armed == 0 { return Err(Error::Corrupt); }
        if self.sender == 0 { return Err(Error::Unprovisioned); }
        if self.next > u64::from(u32::MAX) { return Err(Error::Exhausted); }
        if self.out_len == OUT { return Err(Error::Full); }
        let (sec, sent) = tally(now, self.sec, self.sent_this_sec);
        if sent > SEND_PER_SEC { return Err(Error::Rate); }
        let mut nonce = [0u8; 12];
        nonce[0..4].copy_from_slice(&self.sender.to_le_bytes());
        nonce[4..8].copy_from_slice(&self.epoch.to_le_bytes());
        nonce[8..12].copy_from_slice(&(self.next as u32).to_le_bytes());
        let mut slot = Queued { live: 1, topic: Name::new(topic)?, ttl: e.ttl_secs, expires: now.saturating_add(e.ttl_secs), ..Queued::EMPTY };
        let mut plain = [0; MSG];
        let sealed = (|| {
            let n = e.encode(&mut plain[..MSG - 1 - NONCE - TAG - S::TRAILER]).map_err(|_| Error::TooLarge)?;
            let (ad_all, al) = ad(S::VERSION, topic)?;
            let ad = ad_all.get(..al).ok_or(Error::TooLarge)?;
            slot.wire[0] = S::VERSION;
            slot.len = 1 + self.sealer.seal(&nonce, ad, &mut plain, n, &mut slot.wire[1..]).map_err(|_| Error::TooLarge)?;
            debug_assert!({ let mut check = [0; MSG]; let ok = self.sealer.open(ad, &slot.wire[1..slot.len], &mut check).map(|o| o.len) == Ok(n + S::TRAILER) && check[..n + S::TRAILER] == plain[..n + S::TRAILER]; check.fill(0); core::hint::black_box(&check); ok }, "our own output must open");
            Ok(())
        })();
        plain = [0; MSG];
        core::hint::black_box(&plain);
        sealed?;
        self.push(slot)?;
        (self.next, self.sec, self.sent_this_sec) = (self.next + 1, sec, sent);
        Ok(Id(nonce))
    }

    const fn push(&mut self, slot: Queued<MSG>) -> Result<(), Error> {
        if self.out_len == OUT { return Err(Error::Full); }
        self.out[(self.out_head + self.out_len) % OUT] = slot;
        self.out_len += 1;
        Ok(())
    }

    fn pop(&mut self) {
        if let Some(s) = self.out.get_mut(self.out_head) { s.live = 0; }
        self.out_head = (self.out_head + 1) % OUT;
        self.out_len -= 1;
    }

    /// A verified, new wire enters the seen table; the soonest-expiring entry goes if the table is full.
    fn remember(&mut self, nonce: [u8; 12], now: u32, ttl: u32) {
        let live = |s: &Seen| s.live == 1 && s.expires > now;
        let i = self.seen.iter().position(|s| !live(s)).unwrap_or_else(|| {
            self.pressure = self.pressure.saturating_add(1);
            self.seen.iter().enumerate().min_by_key(|(_, s)| s.expires).map_or(0, |(i, _)| i)
        });
        if let Some(s) = self.seen.get_mut(i) { *s = Seen { expires: now.saturating_add(ttl), nonce, live: 1, _pad: [0; 3] }; }
    }

    /// A slot for a new sender: a free one, else one silent `MAX_TTL`, else none. Kademlia's rule (Maymounkov and
    /// Mazières 2002): a sender still heard is never evicted for a newcomer.
    fn sender_slot(&self, now: u32) -> Option<usize> {
        self.senders.iter().position(|s| s.live == 0).or_else(|| self.senders.iter().position(|s| s.sec.checked_add(MAX_TTL).is_some_and(|t| now >= t)))
    }
}
