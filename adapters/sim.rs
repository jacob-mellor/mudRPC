//! A simulated mesh. Many `Sim` transports share one `World`; a scenario mutates radios and links at set times.
//!
//! Two profiles, each what we think a real mesh of that kind does. The announcing mesh (`reticulum` in a scenario file):
//! a destination is known to a radio only after its announce has crossed a live path to that radio (`ANNOUNCE_MS` after it
//! was sent, and only while a path exists); a message waits at its sender until a path exists; the mesh itself does not
//! store and forward. The custody mesh (`dtn`): a bundle leaves at once and every radio that receives it keeps a copy
//! until its lifetime ends; it survives the sender's death and reaches a listener through any path that opens in time.
//! Both: a radio with a bandwidth limit transmits serially at `len / bps`; relays are not throttled.
use crate::transport::{Rx, Transport};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

/// How long an announce takes to cross a live path on the announcing mesh.
pub const ANNOUNCE_MS: u64 = 1000;
/// Most radios a scenario may have.
pub const MAX_PEERS: usize = 1024;
/// Latest `at_ms` a scenario may name: a warp of seven days plus a day of scenario, nine digits, the log's time column.
pub const MAX_MS: u64 = 7 * 86_400_000 + 86_400_000;
/// A method name no terminal should see raw: an escape sequence, a bell, both quotes, a backslash, a tab.
pub const HOSTILE: &str = "\x1b]0;pwned\x07 \"q\" back\\slash\ttab";
/// Most of the world's own lines a log holds; past it each is counted by its kind, and the harness's lines are all written.
const MAX_LOG: usize = 100_000;
/// The kinds of the world's own lines, each line's first word: what the cap and `peer_lines` count.
const WORLD: [&str; 7] = ["send", "deliver", "expire", "listen", "radio", "event", "full"];
/// Most wires in flight at once, about 500 MB at 1 024 radios; past it a send is not queued and the log says `full`.
pub const MAX_INFLIGHT: usize = 1 << 20;

/// Which mesh the `World` models: the announcing mesh (`reticulum` in a file) or the custody mesh (`dtn`), as above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub enum Profile { Reticulum, Dtn }

/// What a scenario line does, named as the file names it (`heal a` alone is `Unjam`). The first row is the `World`'s:
/// links, radios and injected traffic, a radio being an index below `peers`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // named above and in the rows' own lines
pub enum Kind {
    Partition(usize, usize), Heal(usize, usize), Jam(usize), Unjam(usize), Malformed(usize, String), Replay(usize), Flood(usize, String, u32), Bandwidth(usize, u32), PeerDown(usize), PeerUp(usize),
    /// Host events: the World ignores them; the harness performs them. Clone(a, b): radio b becomes a copy of peer a's
    /// roster line at epoch 1. Spam(a, n): peer a sends n a second with a lying clock. Stage/Rotate/Retire: keys.
    /// Warp(ms): the clock jumps, as a replay floor needs seven days to advance. Hostile(a): peer a calls `nav` with
    /// `HOSTILE` as the method, so every harness proves it escapes what it logs.
    Clone(usize, usize), Spam(usize, u32), Stage, Rotate(usize), Retire, Warp(u64), Hostile(usize),
    /// A radio fault: radio a's receiver flips, on average, n bits per thousand bytes in what its peer takes in; what the
    /// radio holds or replays is what was on the air. 0 clears it.
    Noise(usize, u32),
    /// A relay fault: from now radio a keeps each wire it receives, once, and sends it again, byte for byte as it was on the
    /// air, its own noise on its peer's copy alone, ms after it arrived, as a store-and-forward box that held it past its
    /// ttl; held wires outlive the radio going down. 0 stops the keeping.
    Hold(usize, u64),
    /// Storage faults the harness performs. StaleReboot(a): peer a reboots from its boot image and resumes at boot
    /// epoch + 1, every time, as a host whose epoch write never reached storage would; the second such reboot reuses
    /// nonces. TornSave(a): peer a reboots from a block whose first half is its latest save and second half its boot
    /// image, as a flash write that failed midway leaves it; refused or run on, never a trap.
    StaleReboot(usize), TornSave(usize),
    /// Revoke(a, b): peer a drops peer b's roster line, so a signed wire from b is `Open(Roster)` at a; an unsigned fleet has no roster.
    Revoke(usize, usize),
    /// Telemetry: every drone publishes one position to gcs's name, so a fleet's reports reach one receiver's tables.
    Telemetry,
}

/// One scenario line: what happens, at which scenario millisecond, and the line of the file it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub struct Event { pub at_ms: u64, pub kind: Kind, pub line: usize }

/// A parsed scenario file: the `World`'s random seed, the radio count, the profile, and the events in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub struct Scenario { pub seed: u64, pub peers: usize, pub profile: Profile, pub events: Vec<Event> }

impl Scenario {
    /// `seed N` · `peers N` · `profile reticulum|dtn` · then `at_ms kind args`. Errors name the line (0 = whole file).
    ///
    /// A clone's radio is empty until its first `clone` row, in the order the harnesses perform host events (by time, then
    /// file order), and has no boot image of its own: a host event its peer performs before then, a storage fault on it at
    /// any time, is a scenario no harness can run, refused at its line.
    pub fn parse(text: &str) -> Result<Self, (usize, String)> {
        let (mut seed, mut peers, mut profile, mut events) = (None, None, None, Vec::new());
        for (i, line) in text.lines().enumerate() {
            let bad = |m: &str| (i + 1, m.to_string());
            let num = |s: &str| s.parse::<u64>().map_err(|_| bad("expected a number"));
            let w: Vec<&str> = line.split_whitespace().collect();
            let once = |set: bool| if set { Err(bad("repeated header line")) } else { Ok(()) };
            match w.as_slice() {
                [] | ["#", ..] => {}
                ["seed", s] => { once(seed.is_some())?; seed = Some(num(s)?) }
                ["peers", s] => { once(peers.is_some())?; peers = Some(usize::try_from(num(s)?).ok().filter(|n| (1..=MAX_PEERS).contains(n)).ok_or_else(|| bad("peers must be 1..=1024"))?) }
                ["profile", "reticulum"] => { once(profile.is_some())?; profile = Some(Profile::Reticulum) }
                ["profile", "dtn"] => { once(profile.is_some())?; profile = Some(Profile::Dtn) }
                [at, kind, args @ ..] => {
                    let at_ms = num(at).ok().filter(|&t| t <= MAX_MS).ok_or_else(|| bad("at_ms must be a number within eight days"))?;
                    let idx = |k: usize| usize::try_from(num(args[k])?).map_err(|_| bad("expected a number"));
                    let small = |k: usize, max: u32, m: &str| num(args[k]).ok().and_then(|n| u32::try_from(n).ok()).filter(|&n| n <= max).ok_or_else(|| bad(m));
                    let kind = match (*kind, args.len()) {
                        ("partition", 2) => Kind::Partition(idx(0)?, idx(1)?),
                        ("heal", 2) => Kind::Heal(idx(0)?, idx(1)?),
                        ("heal", 1) => Kind::Unjam(idx(0)?),
                        ("jam", 1) => Kind::Jam(idx(0)?),
                        ("malformed", 2) => Kind::Malformed(idx(0)?, args[1].to_string()),
                        ("replay", 1) => Kind::Replay(idx(0)?),
                        ("flood", 3) => Kind::Flood(idx(0)?, args[1].to_string(), small(2, 20_000, "flood rate must be 0..=20000")?),
                        ("bandwidth", 2) => Kind::Bandwidth(idx(0)?, small(1, u32::MAX, "bandwidth is bytes per second, below 2^32")?),
                        ("peer_down", 1) => Kind::PeerDown(idx(0)?),
                        ("peer_up", 1) => Kind::PeerUp(idx(0)?),
                        ("clone", 2) => Kind::Clone(idx(0)?, idx(1)?),
                        ("spam", 2) => Kind::Spam(idx(0)?, small(1, 1000, "spam rate must be 0..=1000")?),
                        ("stage", 0) => Kind::Stage,
                        ("rotate", 1) => Kind::Rotate(idx(0)?),
                        ("retire", 0) => Kind::Retire,
                        ("warp", 1) => Kind::Warp(num(args[0]).ok().filter(|&t| t <= MAX_MS).ok_or_else(|| bad("warp must be within eight days"))?),
                        ("hostile", 1) => Kind::Hostile(idx(0)?),
                        ("stale_reboot", 1) => Kind::StaleReboot(idx(0)?),
                        ("torn_save", 1) => Kind::TornSave(idx(0)?),
                        ("revoke", 2) => Kind::Revoke(idx(0)?, idx(1)?),
                        ("telemetry", 0) => Kind::Telemetry,
                        ("noise", 2) => Kind::Noise(idx(0)?, small(1, 1000, "noise is bits per thousand bytes, 0..=1000")?),
                        ("hold", 2) => Kind::Hold(idx(0)?, num(args[1]).ok().filter(|&t| t <= MAX_MS).ok_or_else(|| bad("hold must be within eight days"))?),
                        _ => return Err(bad("unknown event")),
                    };
                    let n = peers.ok_or_else(|| bad("peers must come first"))?;
                    if kind.radios().iter().any(|&r| r >= n) { return Err(bad("peer index out of range")); }
                    events.push(Event { at_ms, kind, line: i + 1 });
                }
                _ => return Err(bad("unknown line")),
            }
        }
        let mut filled = BTreeMap::new();
        for (k, e) in events.iter().enumerate() { if let Kind::Clone(_, b) = e.kind { let f = filled.entry(b).or_insert((e.at_ms, k)); *f = (*f).min((e.at_ms, k)); } }
        for (k, e) in events.iter().enumerate() {
            let Some(at) = e.kind.actor().and_then(|r| filled.get(&r)) else { continue };
            if matches!(e.kind, Kind::StaleReboot(_) | Kind::TornSave(_)) { return Err((e.line, "a storage fault on a clone's radio, which has no boot image".into())); }
            if *at >= (e.at_ms, k) { return Err((e.line, format!("the radio is empty until its clone at {} ms", at.0))); }
        }
        let miss = |m: &str| (0, m.to_string());
        Ok(Self { seed: seed.ok_or_else(|| miss("seed missing"))?, peers: peers.ok_or_else(|| miss("peers missing"))?, profile: profile.ok_or_else(|| miss("profile missing"))?, events })
    }
}

impl Kind {
    /// The radio whose peer performs a host event: none for the world's kinds, the fleet-wide ones and `clone`, which boots
    /// from its source's roster line, not from its peer.
    const fn actor(&self) -> Option<usize> {
        match *self { Self::Spam(a, _) | Self::Rotate(a) | Self::Hostile(a) | Self::StaleReboot(a) | Self::TornSave(a) | Self::Revoke(a, _) => Some(a), _ => None }
    }
    fn radios(&self) -> Vec<usize> {
        match *self {
            Self::Partition(a, b) | Self::Heal(a, b) | Self::Clone(a, b) | Self::Revoke(a, b) => vec![a, b],
            Self::Jam(a) | Self::Unjam(a) | Self::Malformed(a, _) | Self::Replay(a) | Self::Flood(a, ..) | Self::Bandwidth(a, _) | Self::PeerDown(a) | Self::PeerUp(a) | Self::Spam(a, _) | Self::Rotate(a) | Self::Hostile(a) | Self::Noise(a, _) | Self::Hold(a, _) | Self::StaleReboot(a) | Self::TornSave(a) => vec![a],
            Self::Stage | Self::Retire | Self::Warp(_) | Self::Telemetry => vec![],
        }
    }
}

/// The fault model shared by every rung: which radios are up and which pairs can hear each other right now.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::len_without_is_empty)] // `len` is the radio count
pub struct Links { cuts: Vec<(usize, usize)>, jammed: Vec<bool>, down: Vec<bool>, group: Vec<usize> }

impl Links {
    /// `n` radios, every one up and unjammed, no pair partitioned.
    #[must_use] pub fn new(n: usize) -> Self { let mut l = Self { cuts: vec![], jammed: vec![false; n], down: vec![false; n], group: vec![] }; l.regroup(); l }
    /// The radio count.
    #[must_use] pub const fn len(&self) -> usize { self.down.len() }
    /// Whether radio `a` is down: it neither sends nor hears.
    #[must_use] pub fn is_down(&self, a: usize) -> bool { self.down[a] }

    /// Applies the link-shaped events; returns false for the others (traffic injection, bandwidth).
    pub fn apply(&mut self, k: &Kind) -> bool {
        match *k {
            Kind::Partition(a, b) => self.cuts.push((a.min(b), a.max(b))),
            Kind::Heal(a, b) => self.cuts.retain(|&c| c != (a.min(b), a.max(b))),
            Kind::Jam(a) => self.jammed[a] = true,
            Kind::Unjam(a) => self.jammed[a] = false,
            Kind::PeerDown(a) => self.down[a] = true,
            Kind::PeerUp(a) => self.down[a] = false,
            _ => return false,
        }
        true
    }

    /// Labels each radio with the lowest index it can reach over live links, so `path` is a compare: O(n²) a call, so the
    /// `World` calls it after each link event and `lan`, which asks only for `link`, never.
    fn regroup(&mut self) {
        let n = self.len();
        self.group = (0..n).collect();
        for s in 0..n {
            if self.group[s] != s { continue; }
            let mut queue = vec![s];
            while let Some(a) = queue.pop() {
                for b in s + 1..n { if self.group[b] == b && self.link(a, b) { self.group[b] = s; queue.push(b); } }
            }
        }
    }

    /// Whether `a` and `b` hear each other directly: two radios, both up and unjammed, the pair not partitioned.
    #[must_use] pub fn link(&self, a: usize, b: usize) -> bool {
        a != b && !self.down[a] && !self.down[b] && !self.jammed[a] && !self.jammed[b] && !self.cuts.contains(&(a.min(b), a.max(b)))
    }

    /// Whether `to` is reachable from `from` over live links, any number of hops, as of the last `regroup`.
    fn path(&self, from: usize, to: usize) -> bool { self.group[from] == self.group[to] }

    /// Whether radio `r` hears at least one other radio.
    #[must_use] pub fn on_mesh(&self, r: usize) -> bool { (0..self.len()).any(|o| self.link(r, o)) }
}

enum Owned { Message(String, Vec<u8>), OnMesh, OffMesh }

/// A listen: the topic, and per radio the tick a live path to it first existed (None: never). A radio knows the
/// destination `ANNOUNCE_MS` after that tick, which is how long an announce takes to cross.
struct Listen { topic: String, reached: Vec<Option<u64>> }

#[derive(Default)]
struct Radio { silent: bool, on_mesh: bool, bps: u32, busy_until: u64, flood: Option<(String, u32)>, noise: u32, hold: u64, held: Vec<(u64, String, Vec<u8>)>, kept: BTreeSet<Vec<u8>>, listens: Vec<Listen>, inbox: VecDeque<Owned>, last: Option<(String, Vec<u8>)> }

struct Msg { from: usize, topic: String, wire: Vec<u8>, ready_at: u64, expires: u64, holders: Bits, delivered: Bits }

/// One bit a radio.
#[derive(Clone)]
struct Bits(Vec<u64>);
impl Bits {
    fn new(n: usize) -> Self { Self(vec![0; n.div_ceil(64)]) }
    fn get(&self, i: usize) -> bool { self.0[i / 64] >> (i % 64) & 1 == 1 }
    fn set(&mut self, i: usize, on: bool) { if on { self.0[i / 64] |= 1 << (i % 64) } else { self.0[i / 64] &= !(1 << (i % 64)) } }
}

/// The one mesh every `Sim` shares: the clock, the radios, the links, what is in flight, and the log.
pub struct World { now: u64, rng: u64, profile: Profile, radios: Vec<Radio>, links: Links, inflight: Vec<Msg>, events: Vec<Event>, log: Vec<String>, lines: usize, unwritten: Vec<(String, u64)> }

/// A send or listen refused because the radio is down.
#[derive(Debug, PartialEq, Eq)]
pub struct Down;

/// One radio's `Transport` into the shared `World`.
pub struct Sim { world: Rc<RefCell<World>>, id: usize, cur: Option<Owned> }

/// One world, one Sim per peer.
#[must_use] pub fn world(s: &Scenario) -> (Rc<RefCell<World>>, Vec<Sim>) {
    let mut events = s.events.clone();
    events.sort_by_key(|e| e.at_ms);
    events.reverse(); // pop from the end: earliest first, file order within a tick
    let radios = (0..s.peers).map(|_| Radio::default()).collect();
    let w = Rc::new(RefCell::new(World { now: 0, rng: s.seed | 1, profile: s.profile, radios, links: Links::new(s.peers), inflight: vec![], events, log: vec![], lines: 0, unwritten: vec![] }));
    let sims = (0..s.peers).map(|id| Sim { world: w.clone(), id, cur: None }).collect();
    (w, sims)
}

impl World {
    /// Scenario time, in milliseconds.
    #[must_use] pub const fn now_ms(&self) -> u64 { self.now }
    /// Every line so far, each led by its time; past `MAX_LOG` world lines, a last one counts by kind those not written.
    #[must_use] pub fn log(&self) -> Vec<String> {
        let tail = (!self.unwritten.is_empty()).then(|| format!("{:>9} world lines past {MAX_LOG} counted, not written: {}", self.now, listed(&self.unwritten)));
        self.log.iter().cloned().chain(tail).collect()
    }
    /// A host line at the current time, so the harness's peer events interleave with the world's own.
    pub fn note(&mut self, s: &str) { self.log.push(format!("{:>9} {s}", self.now)); }
    /// A world line: written while under `MAX_LOG`, then counted by its first word.
    fn line(&mut self, s: &str) {
        let kind = s.split(' ').next().unwrap_or_default();
        debug_assert!(WORLD.contains(&kind), "a world line's kind is in WORLD: {s}");
        if self.lines < MAX_LOG { self.lines += 1; return self.note(s); }
        tally(&mut self.unwritten, kind);
    }

    /// Apply due events, move messages, expire, report link changes, advance the clock.
    pub fn step(&mut self, dt_ms: u64) {
        while self.events.last().is_some_and(|e| e.at_ms <= self.now) {
            let e = self.events.pop().unwrap();
            self.apply(e.kind);
        }
        for l in 0..self.radios.len() {
            for k in 0..self.radios[l].listens.len() {
                for r in 0..self.radios.len() {
                    if self.radios[l].listens[k].reached[r].is_none() && (r == l || self.links.path(l, r)) { self.radios[l].listens[k].reached[r] = Some(self.now); }
                }
            }
        }
        for r in 0..self.radios.len() {
            if !self.links.is_down(r) { // a held wire goes out as `replay` sends one: from the radio, through the mesh
                let (due, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.radios[r].held).into_iter().partition(|h| h.0 <= self.now);
                self.radios[r].held = kept;
                for (_, topic, wire) in due { let _ = self.send(r, &topic, &wire, 60); }
            }
            if let Some((topic, per_sec)) = self.radios[r].flood.clone().filter(|_| !self.links.is_down(r) && dt_ms < 10_000) { // junk lives 10 s: a longer step, a warp, reads none
                let n = u64::from(per_sec) * dt_ms / 1000;
                if per_sec <= 1000 { for _ in 0..n { let j = self.junk(); let _ = self.send(r, &topic, &j, 10); } } else { self.blast(r, &topic, n); }
            }
        }
        let awake: Vec<bool> = (0..self.radios.len()).map(|h| !self.links.is_down(h) && !self.radios[h].silent).collect(); // who can pass a wire on this step
        let mut hearers: Vec<(String, Vec<usize>)> = vec![]; // per topic in flight, the radios with a listen on it
        for m in 0..self.inflight.len() {
            if self.inflight[m].expires <= self.now + dt_ms { continue; } // what this step moves is read after it, so it must be alive then, a warp's week included
            let topic = &self.inflight[m].topic;
            let i = hearers.iter().position(|h| &h.0 == topic).unwrap_or_else(|| { hearers.push((topic.clone(), (0..self.radios.len()).filter(|&r| self.radios[r].listens.iter().any(|l| &l.topic == topic)).collect())); hearers.len() - 1 });
            let ears = std::mem::take(&mut hearers[i].1);
            self.spread(m, &awake, &ears);
            hearers[i].1 = ears;
        }
        let now = self.now;
        let mut log = Vec::new();
        self.inflight.retain(|m| m.expires > now || { log.push(format!("expire {} {:?}", m.from, m.topic)); false });
        for l in log { self.line(&l); }
        for r in 0..self.radios.len() {
            let on = self.links.on_mesh(r);
            if on != self.radios[r].on_mesh {
                self.radios[r].on_mesh = on;
                self.radios[r].inbox.push_back(if on { Owned::OnMesh } else { Owned::OffMesh });
                self.line(&format!("radio {r} {}", if on { "on-mesh" } else { "off-mesh" }));
            }
        }
        self.now += dt_ms;
    }

    fn apply(&mut self, k: Kind) {
        self.line(&format!("event {k:?}"));
        if let Kind::PeerDown(a) = k {
            self.radios[a].inbox.clear();
            for m in &mut self.inflight { m.holders.set(a, false); }
        }
        if self.links.apply(&k) { self.links.regroup(); return; }
        match k {
            Kind::Bandwidth(a, bps) => {
                self.radios[a].bps = bps;
                let mut busy = self.now;
                for m in self.inflight.iter_mut().filter(|m| m.from == a && m.ready_at > self.now) {
                    m.ready_at = if bps == 0 { self.now } else { busy + m.wire.len() as u64 * 1000 / u64::from(bps) };
                    busy = m.ready_at;
                }
                self.radios[a].busy_until = busy;
            }
            Kind::Replay(a) => if let Some((t, w)) = self.radios[a].last.clone() { let _ = self.send(a, &t, &w, 60); },
            Kind::Malformed(a, t) => { let j = self.junk(); let _ = self.send(a, &t, &j, 60); }
            Kind::Flood(a, t, n) => self.radios[a].flood = (n > 0).then_some((t, n)),
            Kind::Noise(a, n) => self.radios[a].noise = n,
            Kind::Hold(a, ms) => self.radios[a].hold = ms,
            _ => {}
        }
    }

    const fn rand(&mut self) -> u64 { self.rng ^= self.rng << 13; self.rng ^= self.rng >> 7; self.rng ^= self.rng << 17; self.rng }

    /// 40 random bytes; version byte 1 so the receiver gets as far as the tag.
    fn junk(&mut self) -> Vec<u8> {
        let mut v: Vec<u8> = (0..40).map(|_| self.rand().to_le_bytes()[0]).collect();
        v[0] = 1;
        v
    }

    /// A flood past 1000 a second, the old cap, where a line a wire would be the log: each step's junk lands at once at every
    /// radio listening over a live path, one line a receiver, wearing the version byte its radio last heard.
    fn blast(&mut self, from: usize, topic: &str, n: u64) {
        let version = self.radios[from].last.as_ref().and_then(|l| l.1.first().copied()).unwrap_or(1);
        let junk: Vec<Vec<u8>> = (0..n).map(|_| { let mut j = self.junk(); j[0] = version; j }).collect();
        let to: Vec<usize> = (0..self.radios.len()).filter(|&r| r != from && self.listens(from, r, topic) && self.links.path(from, r)).collect();
        for r in to {
            self.radios[r].inbox.extend(junk.iter().map(|j| Owned::Message(topic.into(), j.clone())));
            self.line(&format!("deliver {from} -> {r} {topic:?} 40B ×{n}"));
        }
    }

    /// Does radio `r` listen on `topic`, as far as radio `from` has heard?
    fn listens(&self, from: usize, r: usize, topic: &str) -> bool {
        let delay = if self.profile == Profile::Reticulum && from != r { ANNOUNCE_MS } else { 0 };
        self.radios[r].listens.iter().any(|l| l.topic == topic && l.reached[from].is_some_and(|t| t + delay <= self.now))
    }

    /// Moves message `m` one step: `awake` says which radios are up and not silent, `ears` which have a listen on its topic.
    fn spread(&mut self, m: usize, awake: &[bool], ears: &[usize]) {
        let n = self.radios.len();
        let (from, topic) = (self.inflight[m].from, self.inflight[m].topic.clone());
        if self.inflight[m].ready_at > self.now { return; }
        let holders = self.inflight[m].holders.clone(); // this step's holders: one a wire reaches now spreads it next step
        let live = |h: usize| holders.get(h) && awake[h];
        match self.profile {
            Profile::Dtn => {
                for r in (0..n).filter(|&r| r != from && !holders.get(r) && !self.links.is_down(r)) { if (0..n).any(|h| live(h) && self.links.link(h, r)) { self.inflight[m].holders.set(r, true); } }
                for &r in ears.iter().filter(|&&r| r != from) {
                    if self.inflight[m].holders.get(r) && !self.inflight[m].delivered.get(r) && self.listens(r, r, &topic) { self.deliver(m, r); }
                }
            }
            Profile::Reticulum => for h in (0..n).filter(|&h| live(h)) {
                for &r in ears.iter().filter(|&&r| r != from) {
                    if !self.inflight[m].delivered.get(r) && self.listens(h, r, &topic) && self.links.path(h, r) { self.deliver(m, r); }
                }
            },
        }
    }

    fn deliver(&mut self, m: usize, r: usize) {
        self.inflight[m].delivered.set(r, true);
        let (from, topic, wire) = (self.inflight[m].from, self.inflight[m].topic.clone(), self.inflight[m].wire.clone());
        if self.radios[r].hold > 0 && self.radios[r].kept.insert(wire.clone()) { let due = self.now + self.radios[r].hold; self.radios[r].held.push((due, topic.clone(), wire.clone())); }
        let (mut heard, mut flipped) = (wire.clone(), 0); // the radio keeps what was on the air; noise is on the peer's copy
        if self.radios[r].noise > 0 { // the receiver's front end: a byte is hit with probability noise/1000, one bit of it
            let noise = u64::from(self.radios[r].noise);
            for b in &mut heard { if self.rand() % 1000 < noise { *b ^= 1 << (self.rand() % 8); flipped += 1; } }
        }
        self.line(&format!("deliver {from} -> {r} {topic:?} {}B{}", wire.len(), if flipped > 0 { format!(" with {flipped} bit(s) flipped") } else { String::new() }));
        self.radios[r].inbox.push_back(Owned::Message(topic.clone(), heard));
        self.radios[r].last = Some((topic, wire));
    }

    fn send(&mut self, from: usize, topic: &str, wire: &[u8], ttl_secs: u32) -> Result<(), Down> {
        if self.links.is_down(from) { return Err(Down); }
        if self.inflight.len() >= MAX_INFLIGHT { self.line(&format!("full {from} {topic:?} {}B not queued, {MAX_INFLIGHT} in flight", wire.len())); return Ok(()); }
        let n = self.radios.len();
        let mut holders = Bits::new(n);
        holders.set(from, true);
        let radio = &mut self.radios[from];
        let ready_at = if radio.bps == 0 { self.now } else { radio.busy_until.max(self.now) + wire.len() as u64 * 1000 / u64::from(radio.bps) };
        radio.busy_until = ready_at;
        self.inflight.push(Msg { from, topic: topic.into(), wire: wire.into(), ready_at, expires: self.now + u64::from(ttl_secs) * 1000, holders, delivered: Bits::new(n) });
        self.line(&format!("send {from} {topic:?} {}B ttl {ttl_secs}", wire.len()));
        Ok(())
    }

    fn listen(&mut self, r: usize, topic: &str) -> Result<(), Down> {
        if self.links.is_down(r) { return Err(Down); }
        let n = self.radios.len();
        self.radios[r].listens.push(Listen { topic: topic.into(), reached: vec![None; n] });
        self.line(&format!("listen {r} {topic:?}"));
        Ok(())
    }
}

fn tally(counts: &mut Vec<(String, u64)>, kind: &str) { match counts.iter_mut().find(|u| u.0 == kind) { Some(u) => u.1 += 1, None => counts.push((kind.into(), 1)) } }
fn listed(counts: &[(String, u64)]) -> String { counts.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ") }

/// A whole log as its peer lines, the harness's, with the world's held by a last line counting them by kind in the cap's
/// form: how the two largest campaign logs are checked in.
#[must_use] pub fn peer_lines(log: &[String]) -> Vec<String> {
    let (mut out, mut counts) = (Vec::new(), Vec::new());
    for l in log {
        match l.split_whitespace().nth(1) { Some(k) if WORLD.contains(&k) => tally(&mut counts, k), _ => out.push(l.clone()) }
    }
    let at = log.last().and_then(|l| l.split_whitespace().next()).unwrap_or("0");
    out.push(format!("{at:>9} world lines counted, not written: {}", listed(&counts)));
    out
}

impl Transport for Sim {
    type Error = Down;
    fn send(&mut self, topic: &str, wire: &[u8], ttl_secs: u32) -> Result<(), Down> { self.world.borrow_mut().send(self.id, topic, wire, ttl_secs) }
    fn listen(&mut self, topic: &str) -> Result<(), Down> { self.world.borrow_mut().listen(self.id, topic) }
    fn poll(&mut self) -> Option<Rx<'_>> {
        self.cur = self.world.borrow_mut().radios[self.id].inbox.pop_front();
        Some(match self.cur.as_ref()? {
            Owned::Message(t, w) => Rx::Message { topic: t, wire: w },
            Owned::OnMesh => Rx::OnMesh,
            Owned::OffMesh => Rx::OffMesh,
        })
    }
    fn silent(&mut self, on: bool) { self.world.borrow_mut().radios[self.id].silent = on; }
}
