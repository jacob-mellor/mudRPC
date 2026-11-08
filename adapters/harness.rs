//! The standard harness every scenario, every rung and every binding runs.
//!
//! Peer 0 is `gcs`; the rest are `drone-N`,
//! listening on `nav` and echoing `Ping`. gcs calls Ping#1 (ttl 60) and Ping#2 (ttl 10) in the first round, then one Ping
//! every 150 rounds, 15 s but for a warp, with ttl 20. Host events in the scenario (`clone`, `spam`, `stage`, `rotate`,
//! `retire`, `warp`, `hostile`, `stale_reboot`, `torn_save`, `revoke`, `telemetry`) are performed here. Every Incoming is
//! logged, drops and floods included, and every call, publish or reply the peer refuses, `peer2 refused reply Full`; every
//! wire-derived string escaped, the key slot named when it is not the current one; while a flood is on, every ten rounds,
//! every peer that took events says how many; pressure per peer closes the log.
//! Identical peer logs across bindings is the conformance test for a binding; the same peer events across rungs is the
//! test for a rung.
use crate::sim::{self, Kind, Scenario};
use crate::transport::{pump, Transport};
use mudrpc_auth::ed25519::public_key;
use mudrpc_auth::signed::Signed;
use mudrpc_core::rpc::{Incoming, Peer};
use mudrpc_core::seal::{self, Cipher, Key, Sealer, Slot, Slots};
use mudrpc_gcm::Gcm;

/// The same sizes as the C ABI's Peer, so every binding's log is byte-identical to this one.
pub type P = Peer<Key, 512, 16, 64, 16, 16>;
/// Counting bytes, never 32 equal ones: the ABI refuses a constant secret, and every harness derives these the same way.
pub const KEY: [u8; 32] = counting(0);
/// The next fleet key, which a scenario's `stage` hands every peer.
pub const NEXT_KEY: [u8; 32] = counting(32);
#[allow(clippy::cast_possible_truncation)] // i < 32
const fn counting(start: u8) -> [u8; 32] { let mut k = [0; 32]; let mut i = 0; while i < 32 { k[i] = start.wrapping_add(i as u8); i += 1; } k }
/// One round of the harness, in scenario milliseconds.
pub const STEP_MS: u64 = 100;
/// How long the run goes on after the last scenario event, or after the clock a warp left if that is later.
pub const TAIL_MS: u64 = 50_000;
/// Peers boot at epoch 2 so a clone from an old image (epoch 1) is a rollback, not just a replay.
pub const EPOCH: u32 = 2;

/// What the harness asks of a sealer: one booted for a roster number among `peers`, the shape gcs boots, and the fleet-key events.
pub trait Fleet: Sealer + Sized {
    /// The hub's sealer: the same under `Key`; the 256-line roster under `Signed`, as the ABI's hub block.
    type Hub: Fleet;
    /// Whether gcs boots as the hub, with the hub's tables: as the ABI, only a signed fleet has one.
    const HUB: bool;
    /// The sealer for roster number `sender`, under `KEY` and believing every peer that fits.
    fn boot(sender: u32, peers: usize) -> Self;
    /// The scenario's `stage`: `key` becomes the next fleet key.
    fn stage(&mut self, key: [u8; 32]);
    /// The scenario's `rotate`: previous ← current ← next.
    fn rotate(&mut self);
    /// The scenario's `retire`: the previous fleet key is dropped.
    fn retire(&mut self);
    /// The scenario's `revoke`: `sender`'s roster line is dropped; nothing under `Key`, which has no roster.
    fn revoke(&mut self, sender: u32);
}
impl<C: Cipher> Fleet for Slots<C> {
    type Hub = Self;
    const HUB: bool = false;
    fn boot(_: u32, _: usize) -> Self { Self::new(KEY) }
    fn stage(&mut self, key: [u8; 32]) { Self::stage(self, key) }
    fn rotate(&mut self) { Self::rotate(self) }
    fn retire(&mut self) { Self::retire(self) }
    fn revoke(&mut self, _: u32) {}
}
/// A seed per roster number, so a clone from an old image signs as the device it copies.
#[must_use] pub const fn seed(sender: u32) -> [u8; 32] { counting(sender.to_le_bytes()[0]) }
impl<S: Fleet, const R: usize> Fleet for Signed<S, R> {
    type Hub = Signed<S, 256>;
    const HUB: bool = true;
    #[allow(clippy::cast_possible_truncation)] // a scenario has at most MAX_PEERS peers, 1024
    fn boot(sender: u32, peers: usize) -> Self {
        let mut s = Self::new(S::boot(sender, peers), seed(sender), 1000);
        for n in 1..=peers as u32 { if s.roster(n, public_key(&seed(n)), None) == Err(seal::Error::Full) { break; } } // a leaf believes the hub and the drones that fit
        s
    }
    fn stage(&mut self, key: [u8; 32]) { self.inner().stage(key) }
    fn rotate(&mut self) { self.inner().rotate() }
    fn retire(&mut self) { self.inner().retire() }
    fn revoke(&mut self, sender: u32) { Self::revoke(self, sender) }
}

/// A peer by role: under `Signed` gcs is the hub, as the ABI's hub block (a 256-line roster, 1024 seen, 256 windows), and
/// every drone a leaf; under `Key` every peer is a leaf.
#[allow(clippy::large_enum_variant)] // the hub is the leaf with larger tables
enum Node<F: Fleet> { Hub(Peer<F::Hub, 512, 16, 1024, 256, 16>), Leaf(Peer<F, 512, 16, 64, 16, 16>) }
macro_rules! on { ($n:expr, |$p:ident| $body:expr) => { match $n { Node::Hub($p) => $body, Node::Leaf($p) => $body } }; }
#[allow(clippy::cast_possible_truncation)] // peer indices are below MAX_PEERS, 1024
fn boot<F: Fleet>(i: usize, peers: usize, epoch: u32) -> Node<F> {
    let (name, sender) = (if i == 0 { "gcs".to_string() } else { format!("drone-{i}") }, i as u32 + 1);
    if i == 0 && F::HUB { Node::Hub(Peer::new(&name, F::Hub::boot(sender, peers), sender, epoch).unwrap()) } else { Node::Leaf(Peer::new(&name, F::boot(sender, peers), sender, epoch).unwrap()) }
}

/// Run on the simulator. The World's own log and the peer events interleave in one log.
#[must_use] pub fn run(s: &Scenario) -> Vec<String> { run_as::<Key>(s) }
/// The same run under the signed sealer.
#[must_use] pub fn run_signed(s: &Scenario) -> Vec<String> { run_as::<Signed<Key, 16>>(s) }
/// The same run under AES-256-GCM.
#[must_use] pub fn run_gcm(s: &Scenario) -> Vec<String> { run_as::<Gcm>(s) }
/// The same run under the signed sealer over AES-256-GCM.
#[must_use] pub fn run_gcm_signed(s: &Scenario) -> Vec<String> { run_as::<Signed<Gcm, 16>>(s) }

fn run_as<F: Fleet>(s: &Scenario) -> Vec<String> {
    let (w, mut sims) = sim::world(s);
    run_with::<_, F>(s, &mut sims, |_, tick| match tick { Note(n) => w.borrow_mut().note(n), Step => w.borrow_mut().step(STEP_MS), Warp(ms) => w.borrow_mut().step(ms) });
    let log = w.borrow().log();
    log
}

/// What the harness asks of the clock between rounds: log a note, step one `STEP_MS`, or jump the milliseconds given.
#[allow(missing_docs)] // named above
pub enum Tick<'a> { Note(&'a str), Step, Warp(u64) }
use Tick::*;

/// Run on any transports. `tick` records a note, advances the world one `STEP_MS`, or warps it.
#[allow(clippy::cast_possible_truncation)] // peer indices are below MAX_PEERS, 1024; scenario seconds are below 2^32
pub fn run_with<T: Transport, F: Fleet>(s: &Scenario, transports: &mut [T], mut tick: impl FnMut(u64, Tick)) {
    let mut peers: Vec<Option<Node<F>>> = (0..s.peers).map(|i| Some(boot::<F>(i, s.peers, EPOCH))).collect();
    for p in peers.iter_mut().skip(1).flatten() { on!(p, |p| p.listen("nav").unwrap()); }
    let mut host: Vec<&sim::Event> = s.events.iter().filter(|e| matches!(e.kind, Kind::Clone(..) | Kind::Spam(..) | Kind::Stage | Kind::Rotate(_) | Kind::Retire | Kind::Warp(_) | Kind::Hostile(_) | Kind::StaleReboot(_) | Kind::TornSave(_) | Kind::Revoke(..) | Kind::Telemetry | Kind::Flood(..))).collect();
    host.sort_by_key(|e| e.at_ms);
    for e in &host { if let Kind::Clone(_, b) = e.kind { peers[b] = None; } } // a clone's radio is empty until the event
    let images: Vec<Option<Vec<u8>>> = peers.iter().map(|p| p.as_ref().map(image)).collect(); // what each host wrote to flash at boot
    let mut spam: Vec<(usize, u32, u32)> = Vec::new(); // (peer, per second, lying clock)
    let (mut flooding, mut landed) = (vec![false; s.peers], vec![0u32; s.peers]); // radios flooding; events each peer took this second
    let mut end = s.events.iter().map(|e| e.at_ms).max().unwrap_or(0) + TAIL_MS;
    let (mut t, mut n, mut round) = (0u64, 2, 0u64); // gcs's cadence counts rounds, so a warp off the round grid keeps it
    'round: while t <= end {
        let now = (t / 1000) as u32;
        if round % 10 == 0 {
            for (i, k) in landed.iter_mut().enumerate() { if *k > 0 && flooding.contains(&true) { tick(t, Note(&format!("peer{i} honest events landed this second: {k}"))); } *k = 0; }
        }
        while host.first().is_some_and(|e| e.at_ms <= t) {
            let e = host.remove(0);
            match &e.kind {
                Kind::Clone(a, b) => { let mut c = boot::<F>(*a, s.peers, 1); on!(&mut c, |p| p.listen("nav").unwrap()); peers[*b] = Some(c); tick(t, Note(&format!("host: radio {b} is a clone of peer{a} from an old image"))); }
                Kind::Spam(a, rate) => { spam.retain(|s| s.0 != *a); if *rate > 0 { spam.push((*a, *rate, 1_000_000)); } tick(t, Note(&format!("host: peer{a} spams at {rate}/s"))); }
                Kind::Stage => { for p in peers.iter_mut().flatten() { on!(p, |p| p.sealer_mut().stage(NEXT_KEY)); } tick(t, Note("host: every peer stages the next key")); }
                Kind::Rotate(a) => { if let Some(p) = &mut peers[*a] { on!(p, |p| p.sealer_mut().rotate()); } tick(t, Note(&format!("host: peer{a} rotates"))); }
                Kind::Retire => { for p in peers.iter_mut().flatten() { on!(p, |p| p.sealer_mut().retire()); } tick(t, Note("host: every peer retires the previous key")); }
                Kind::StaleReboot(a) => { if let Some(img) = &images[*a] { peers[*a] = Some(restore::<F>(*a, img, EPOCH + 1).expect("the boot image resumes")); tick(t, Note(&format!("host: peer{a} reboots from its boot image at epoch {}, which its storage never kept", EPOCH + 1))); } }
                Kind::TornSave(a) => {
                    if let (Some(img), Some(p)) = (&images[*a], &peers[*a]) {
                        let (latest, half) = (image(p), img.len() / 2);
                        let torn: Vec<u8> = latest[..half].iter().chain(&img[half..]).copied().collect();
                        peers[*a] = Some(match restore::<F>(*a, &torn, EPOCH + 1) {
                            Ok(p) => { tick(t, Note(&format!("host: peer{a} reboots from a torn block, accepted"))); p }
                            Err(e) => { tick(t, Note(&format!("host: peer{a} reboots from a torn block, refused as {e:?}; booting from the boot image"))); restore::<F>(*a, img, EPOCH + 1).expect("the boot image resumes") }
                        });
                    }
                }
                Kind::Hostile(a) => { tick(t, Note(&format!("host: peer{a} calls a hostile method"))); if let Some(p) = &mut peers[*a] { if let Err(e) = on!(p, |p| p.call(now, "nav", sim::HOSTILE, b"x", 20)) { tick(t, Note(&format!("peer{a} refused call {e:?}"))); } } }
                Kind::Revoke(a, b) => { if let Some(p) = &mut peers[*a] { on!(p, |p| p.sealer_mut().revoke(*b as u32 + 1)); } tick(t, Note(&format!("host: peer{a} revokes peer{b}'s roster line"))); }
                Kind::Telemetry => {
                    tick(t, Note("host: every drone publishes its position to gcs"));
                    for (i, p) in peers.iter_mut().enumerate().skip(1) { if let Some(Err(e)) = p.as_mut().map(|p| on!(p, |p| p.publish(now, "gcs", b"pos", 600))) { tick(t, Note(&format!("peer{i} refused publish {e:?}"))); } }
                }
                Kind::Flood(a, _, rate) => flooding[*a] = *rate > 0,
                Kind::Warp(ms) => { tick(t, Note(&format!("host: the clock jumps {ms} ms"))); tick(t, Warp(*ms)); t += ms; end = end.max(t + TAIL_MS); continue 'round; } // the round again, at the new now, and the tail after it
                _ => {}
            }
        }
        if let Some(gcs) = &mut peers[0] {
            on!(gcs, |gcs| if round == 0 {
                gcs.call(now, "nav", "Ping", b"1", 60).unwrap();
                gcs.call(now, "nav", "Ping", b"2", 10).unwrap();
                tick(t, Note("gcs calls Ping#1 ttl 60 and Ping#2 ttl 10"));
            } else if round % 150 == 0 {
                n += 1;
                let note = match gcs.call(now, "nav", "Ping", n.to_string().as_bytes(), 20) { Ok(_) => format!("gcs calls Ping#{n} ttl 20"), Err(e) => format!("peer0 refused call {e:?}") };
                tick(t, Note(&note));
            });
        }
        for i in 0..s.peers {
            let Some(peer) = &mut peers[i] else { continue };
            on!(peer, |peer| {
                let (mut notes, mut refused) = (Vec::new(), Vec::new()); // a refused reply's line follows the round's incoming
                for (_, rate, clock) in spam.iter_mut().filter(|s| s.0 == i) {
                    for _ in 0..(u64::from(*rate) * STEP_MS / 1000) { *clock += 1; if let Err(e) = peer.publish(*clock, "nav", b"spam", 5) { notes.push(format!("peer{i} refused publish {e:?}")); } }
                }
                let _ = pump(peer, &mut transports[i], now, |peer, inc| match inc {
                    Incoming::Call(c) => {
                        notes.push(format!("peer{i} call {:?} {:?}", c.method, String::from_utf8_lossy(c.bytes)));
                        if let Err(e) = peer.reply(now, &c, 0, 0, "", c.bytes, true) { refused.push(format!("peer{i} refused reply {e:?}")); }
                    }
                    Incoming::Reply { seq, end, status, bytes, .. } => notes.push(format!("peer{i} reply seq={seq} end={end} status={status} {:?}", String::from_utf8_lossy(bytes))),
                    Incoming::Event { topic, sender, bytes } => { landed[i] += 1; notes.push(format!("peer{i} event {topic:?} {:?} from sender {sender}", String::from_utf8_lossy(bytes))) }
                    Incoming::Expired { .. } => notes.push(format!("peer{i} expired")),
                    Incoming::Flood { sender } => notes.push(format!("peer{i} flood from sender {sender}")),
                    Incoming::Rollback { sender } => notes.push(format!("peer{i} rollback from sender {sender}")),
                    Incoming::Dropped(why) => notes.push(format!("peer{i} dropped {why:?}")),
            });
            notes.append(&mut refused);
            let via = match peer.opened() { Slot::Current => "", Slot::Previous => " via Previous", Slot::Next => " via Next" };
            let mut k = 0;
            while k < notes.len() { // repeated identical lines in one round become one line with a count
                let run = notes[k..].iter().take_while(|n| *n == &notes[k]).count();
                let note = &notes[k];
                let line = if run > 1 { format!("{note} ×{run}") } else if matches!(note.split(' ').nth(1), Some("call" | "reply" | "event")) { format!("{note}{via}") } else { note.clone() };
                tick(t, Note(&line));
                k += run;
            }
            });
        }
        tick(t, Step);
        (t, round) = (t + STEP_MS, round + 1);
    }
    for (i, p) in peers.iter().enumerate() { if let Some(p) = p { tick(t, Note(&format!("peer{i} pressure {}", on!(p, |p| p.pressure())))); } }
}

/// A peer's bytes, as a host writes them to flash.
fn image<F: Fleet>(n: &Node<F>) -> Vec<u8> {
    // SAFETY: a Peer is plain `repr(C)` data; its bytes are read within its size and copied out while `n` is borrowed.
    on!(n, |p| unsafe { core::slice::from_raw_parts(core::ptr::from_ref(p).cast::<u8>(), core::mem::size_of_val(p)).to_vec() })
}

/// Peer `i` from flash bytes, resumed at the host's epoch in the shape its role has: what every wrapper's `restore` does.
fn restore<F: Fleet>(i: usize, bytes: &[u8], epoch: u32) -> Result<Node<F>, mudrpc_core::rpc::Error> {
    if i == 0 && F::HUB { read(bytes, epoch).map(Node::Hub) } else { read(bytes, epoch).map(Node::Leaf) }
}
fn read<S: Fleet, const SEEN: usize, const SENDERS: usize>(bytes: &[u8], epoch: u32) -> Result<Peer<S, 512, 16, SEEN, SENDERS, 16>, mudrpc_core::rpc::Error> {
    assert!(bytes.len() >= core::mem::size_of::<Peer<S, 512, 16, SEEN, SENDERS, 16>>(), "a saved block is a whole peer");
    // SAFETY: the slice holds a whole Peer, plain `repr(C)` data read unaligned; `resume` then checks every field of it.
    let mut p: Peer<S, 512, 16, SEEN, SENDERS, 16> = unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<Peer<S, 512, 16, SEEN, SENDERS, 16>>()) };
    p.resume(epoch).map(|()| p)
}
