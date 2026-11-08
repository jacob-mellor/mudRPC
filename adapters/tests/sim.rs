//! Unit tests for the simulator: parser, the layer 3 conformance set, sim behaviour. Scenario runs are in e2e.rs.
//! Contract assumed here (sim.rs):
//! ```text
//!   Scenario::parse(&str) -> Result<Scenario, (line, msg)> · sim::world(&Scenario) -> (Rc<RefCell<World>>, Vec<Sim>)
//!   World: now_ms() · step(dt_ms) · log() -> &[String] · note(&str)
//!   Sim: Transport · Error = Down
//! ```
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
)]
use mudrpc_adapters::sim::{self, Kind, Profile, Scenario};
use mudrpc_adapters::transport::{pump, Rx, Transport};
use mudrpc_adapters::harness::P;
use mudrpc_core::rpc::Incoming;
use mudrpc_core::seal::Key;

mod support;
use support::{messages, run, scenario, text, world, DIR};

// ───────────────────────── scenario files ─────────────────────────

#[test]
fn parse_accepts_the_standard_file_and_rejects_bad_lines() {
    let s = Scenario::parse(&std::fs::read_to_string(format!("{DIR}partition-heal.scenario")).unwrap()).unwrap();
    assert_eq!((s.seed, s.peers, s.profile), (7, 2, Profile::Reticulum));
    assert_eq!(s.events.iter().map(|e| (e.at_ms, e.kind.clone())).collect::<Vec<_>>(), [(0, Kind::Partition(0, 1)), (30000, Kind::Heal(0, 1))]);
    let bad = |text: &str| Scenario::parse(text).unwrap_err().0;
    assert_eq!(bad("seed 1\npeers 2\nprofile dtn\n100 teleport 0 1"), 4);
    assert_eq!(bad("seed 1\npeers 2\nprofile dtn\nabc partition 0 1"), 4);
    assert_eq!(bad("seed 1\npeers 2\nprofile dtn\n0 partition 0"), 4);
    assert_eq!(bad("seed 1\npeers 2\nprofile dtn\n0 partition 0 5"), 4);
    assert_eq!(bad("seed 1\npeers 2\nprofile mesh\n"), 3);
    assert!(Scenario::parse("peers 2\nprofile dtn\n").is_err());
    assert_eq!(bad("seed 1\npeers 10\n0 jam 9\npeers 2\nprofile dtn"), 4, "a repeated header cannot defeat the index check");
    assert_eq!(bad("seed 1\npeers 100000000\nprofile dtn"), 2);
    assert_eq!(bad("seed 1\npeers 1025\nprofile dtn"), 2, "one past the ceiling is refused with its line");
    assert_eq!(bad("seed 1\npeers 0\nprofile dtn"), 2);
    assert_eq!(bad("seed 1\npeers 2\nprofile dtn\n99999999999999 jam 0"), 4);
    assert_eq!(bad("seed 1\npeers 2\nprofile dtn\n0 flood 1 nav 20001"), 4);
    assert_eq!(scenario(2, "dtn", "0 flood 1 nav 20000").events[0].kind, Kind::Flood(1, "nav".into(), 20_000));
    assert_eq!(scenario(2, "dtn", "0 hold 1 691200000").events[0].kind, Kind::Hold(1, sim::MAX_MS));
    assert_eq!(bad("seed 1\npeers 2\nprofile dtn\n0 hold 1 691200001"), 4);
    let all = scenario(3, "dtn", "0 partition 0 1\n1 heal 0 1\n2 jam 2\n3 heal 2\n4 malformed 2 nav\n5 replay 2\n6 bandwidth 1 500\n7 peer_down 0\n8 peer_up 0\n9 clone 1 2\n10 spam 2 100\n11 stage\n12 rotate 1\n13 retire\n14 warp 604800000");
    assert_eq!(all.events.len(), 15);
    assert_eq!(bad("seed 1\npeers 2\nprofile dtn\n0 spam 1 5000"), 4);
    assert_eq!(bad("seed 1\npeers 2\nprofile dtn\n0 clone 1 5"), 4);
}

/// A clone's radio is empty until its first `clone` row in the order the harnesses perform them, and has no boot image:
/// A stranger once cut a scenario to `torn_save 2` at 5 s before `clone 1 2` at 10 s, which Rust and C ran on nobody
/// and four harnesses trapped on. Every host event its peer performs before the clone is refused at its line, a storage
/// fault there at any time, one parser for six harnesses; a clone from it boots a roster line and the world's kinds act
/// on the radio, so those stand.
#[test]
fn a_host_event_on_a_radio_its_clone_has_not_filled_is_refused_at_its_line() {
    let parse = |rows: &str| Scenario::parse(&format!("seed 1\npeers 3\nprofile dtn\n{rows}")).map(|s| s.events.len()).map_err(|e| e.0);
    assert_eq!(parse("5000 torn_save 2\n10000 clone 1 2"), Err(4), "the stranger's scenario");
    for row in ["stale_reboot 2", "spam 2 10", "rotate 2", "hostile 2", "revoke 2 0"] { assert_eq!(parse(&format!("10000 clone 1 2\n5000 {row}")), Err(5), "{row}"); }
    assert_eq!(parse("10000 rotate 2\n10000 clone 1 2"), Err(4), "at one millisecond, file order");
    assert_eq!(parse("5000 clone 2 0\n10000 clone 1 2"), Ok(2), "a clone boots its source's roster line, not its peer");
    assert_eq!(parse("10000 clone 1 2\n20000 torn_save 2\n"), Err(5), "a storage fault after the clone: no boot image of its own");
    assert_eq!(parse("20000 clone 0 2\n10000 clone 1 2\n15000 rotate 2\n15000 spam 2 1\n15000 clone 2 0\n15000 revoke 2 1\n15000 hostile 2"), Ok(7), "filled by its earliest clone");
    assert_eq!(parse("0 peer_down 2\n0 partition 2 0\n0 flood 2 nav 10\n0 revoke 0 2\n0 stage\n0 telemetry\n10000 clone 1 2\n10000 rotate 2"), Ok(8), "the world's kinds and the fleet's");
}

// ───────────────────────── conformance (SPEC layer 3) ─────────────────────────

#[test]
fn send_returns_at_once_even_off_mesh_and_poll_never_blocks() {
    for p in ["reticulum", "dtn"] {
        let (w, mut s) = world(2, p, "0 partition 0 1");
        run(&w, 100);
        s[1].listen("t").unwrap();
        s[0].send("t", b"hello", 60).unwrap();
        assert!(messages(&mut s[1]).is_empty());
        run(&w, 5000);
        assert!(messages(&mut s[1]).is_empty(), "{p}: still partitioned");
        assert!(s[0].poll().is_none());
    }
}

#[test]
fn sending_to_a_topic_nobody_listens_on_is_not_an_error() {
    let (w, mut s) = world(2, "reticulum", "");
    s[0].send("nobody", b"x", 5).unwrap();
    run(&w, 10000);
    assert!(messages(&mut s[1]).is_empty());
}

#[test]
fn partitioned_message_arrives_after_heal_within_ttl_and_never_past_ttl() {
    for p in ["reticulum", "dtn"] {
        let (w, mut s) = world(2, p, "0 partition 0 1\n30000 heal 0 1");
        s[1].listen("t").unwrap();
        run(&w, 100);
        s[0].send("t", b"long", 60).unwrap();
        s[0].send("t", b"short", 10).unwrap();
        run(&w, 29900);
        assert!(messages(&mut s[1]).is_empty(), "{p}");
        run(&w, 1200); // the announcing mesh: the announce needs a second to cross the healed link
        assert_eq!(messages(&mut s[1]), [("t".to_string(), b"long".to_vec())], "{p}");
        run(&w, 60000);
        assert!(messages(&mut s[1]).is_empty(), "{p}: nothing more, ever");
        let (w, mut s) = world(2, p, "1100 peer_down 1\n604801200 peer_up 1");
        s[1].listen("t").unwrap();
        run(&w, 1200); // the listen is heard, then radio 1 goes down
        s[0].send("t", b"week", 60).unwrap();
        w.borrow_mut().step(604_800_000); // a warp: the wire dies inside it
        run(&w, 2000);
        assert!(messages(&mut s[1]).is_empty(), "{p}: nor across a warp");
    }
}

/// Provisional for both real substrates: an announced destination is bound to one identity, and a bundle is routed to one endpoint.
/// The first question for each community is whether a shared name may have several listeners.
#[test]
fn two_listeners_on_one_topic_both_receive_one_publish() {
    for p in ["reticulum", "dtn"] {
        let (w, mut s) = world(3, p, "");
        s[1].listen("t").unwrap();
        s[2].listen("t").unwrap();
        run(&w, 1100);
        s[0].send("t", b"all", 10).unwrap();
        run(&w, 500);
        assert_eq!(messages(&mut s[1]).len(), 1, "{p}");
        assert_eq!(messages(&mut s[2]).len(), 1, "{p}");
        assert!(messages(&mut s[0]).is_empty(), "{p}: a sender does not receive its own publish");
    }
}

#[test]
fn silent_means_nothing_leaves_but_messages_still_arrive() {
    for p in ["reticulum", "dtn"] {
        let (w, mut s) = world(2, p, "");
        s[0].listen("in").unwrap();
        s[1].listen("out").unwrap();
        run(&w, 1100);
        s[0].silent(true);
        s[0].send("out", b"held", 60).unwrap();
        s[1].send("in", b"arrives", 60).unwrap();
        run(&w, 2000);
        assert!(messages(&mut s[1]).is_empty(), "{p}");
        assert_eq!(messages(&mut s[0]).len(), 1, "{p}");
        s[0].silent(false);
        run(&w, 500);
        assert_eq!(messages(&mut s[1]).len(), 1, "{p}: released");
    }
}

#[test]
fn malformed_bytes_from_a_hostile_radio_reach_poll_as_bytes() {
    let (w, mut s) = world(2, "dtn", "500 malformed 1 t");
    s[0].listen("t").unwrap();
    run(&w, 1000);
    let m = messages(&mut s[0]);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].0, "t");
    assert!(!m[0].1.is_empty());
}

// ───────────────────────── sim behaviour ─────────────────────────

#[test]
fn events_at_the_same_tick_run_in_file_order() {
    let (w, mut s) = world(2, "dtn", "0 peer_down 1\n0 peer_up 1");
    run(&w, 100);
    assert!(s[1].send("t", b"alive", 5).is_ok());
}

#[test]
fn a_transport_error_ends_the_drain_not_the_round_and_keeps_the_message_for_later() {
    let (w, mut sims) = world(2, "dtn", "0 peer_down 0\n2000 peer_up 0");
    let mut a = P::new("gcs", Key::new([1; 32]), 1, 2).unwrap();
    let mut b = P::new("drone", Key::new([1; 32]), 2, 2).unwrap();
    b.listen("nav").unwrap();
    pump(&mut b, &mut sims[1], 0, |_, _| {}).unwrap();
    run(&w, 100);
    a.call(0, "nav", "Ping", b"", 60).unwrap();
    a.call(0, "nav", "Ping", b"short", 1).unwrap();
    assert_eq!(pump(&mut a, &mut sims[0], 0, |_, _| {}), Err(sim::Down), "radio is down");
    let mut expired = 0;
    assert_eq!(pump(&mut a, &mut sims[0], 1, |_, i| expired += usize::from(matches!(i, Incoming::Expired { .. }))), Err(sim::Down), "still down");
    assert_eq!(expired, 1, "and the clock still reached the peer: the short call expired while the radio was down");
    run(&w, 2000);
    pump(&mut a, &mut sims[0], 2, |_, _| {}).unwrap();
    run(&w, 500);
    let mut got = false;
    pump(&mut b, &mut sims[1], 3, |_, i| got |= matches!(i, Incoming::Call(_))).unwrap();
    assert!(got, "the call waited in the peer and left when the radio came back");
}

#[test]
fn link_events_report_on_and_off_mesh() {
    let (w, mut s) = world(2, "dtn", "1000 partition 0 1\n2000 heal 0 1");
    run(&w, 100);
    assert_eq!(s[0].poll(), Some(Rx::OnMesh));
    assert!(s[0].poll().is_none());
    run(&w, 1000);
    assert_eq!(s[0].poll(), Some(Rx::OffMesh));
    run(&w, 1000);
    assert_eq!(s[0].poll(), Some(Rx::OnMesh));
}

#[test]
fn dtn_custody_survives_the_sender_but_reticulum_waits_at_the_sender() {
    for (p, delivered) in [("dtn", true), ("reticulum", false)] {
        let (w, mut s) = world(3, p, "0 partition 1 2\n0 partition 0 2\n1000 peer_down 0\n2000 heal 1 2");
        s[2].listen("t").unwrap();
        run(&w, 500);
        s[0].send("t", b"via relay", 60).unwrap();
        run(&w, 500);
        assert!(messages(&mut s[2]).is_empty());
        run(&w, 3000);
        assert_eq!(!messages(&mut s[2]).is_empty(), delivered, "{p}");
    }
}

#[test]
fn a_dead_radio_cannot_send_and_hears_nothing_until_it_is_back() {
    let (w, mut s) = world(2, "dtn", "0 peer_down 0\n5000 peer_up 0");
    s[0].listen("t").unwrap();
    run(&w, 100);
    assert!(s[0].send("t", b"x", 60).is_err());
    s[1].send("t", b"while down", 1).unwrap();
    run(&w, 5000);
    assert!(messages(&mut s[0]).is_empty());
    s[1].send("t", b"back", 60).unwrap();
    run(&w, 500);
    assert_eq!(messages(&mut s[0]), [("t".to_string(), b"back".to_vec())]);
}

#[test]
fn bandwidth_collapse_delays_delivery() {
    let (w, mut s) = world(2, "dtn", "0 bandwidth 0 20");
    s[1].listen("t").unwrap();
    run(&w, 100);
    s[0].send("t", &[0u8; 40], 60).unwrap();
    run(&w, 1000);
    assert!(messages(&mut s[1]).is_empty(), "40 bytes at 20 B/s do not fit in one second");
    run(&w, 1500);
    assert_eq!(messages(&mut s[1]).len(), 1);
}

#[test]
fn replay_reinjects_the_last_message_a_radio_saw() {
    let (w, mut s) = world(3, "dtn", "2000 replay 1");
    s[1].listen("t").unwrap();
    s[2].listen("t").unwrap();
    run(&w, 100);
    s[0].send("t", b"once", 60).unwrap();
    run(&w, 500);
    assert_eq!(messages(&mut s[2]).len(), 1);
    run(&w, 2000);
    assert_eq!(messages(&mut s[2]), [("t".to_string(), b"once".to_vec())], "the replayed copy");
}

#[test]
fn a_warp_during_a_flood_is_a_jump_in_time_not_days_of_junk() {
    let (w, _) = world(2, "dtn", "1000 flood 1 nav 1000\n2000 warp 604800000");
    let t = std::time::Instant::now();
    for _ in 0..30 { w.borrow_mut().step(100); }
    w.borrow_mut().step(604_800_000);
    assert!(t.elapsed().as_secs() < 5, "bounded: at most one second of flood per step, however long the step");
}

#[test]
fn a_flood_past_the_old_cap_lands_each_step_at_once_in_one_line_wearing_the_version_it_heard() {
    let (w, mut s) = world(2, "reticulum", "2000 flood 1 t 20000\n3000 flood 1 t 0");
    s[0].listen("t").unwrap();
    s[1].listen("u").unwrap();
    run(&w, 1500);
    s[0].send("u", &[3; 40], 10).unwrap();
    run(&w, 1500);
    let got = messages(&mut s[0]);
    assert_eq!(got.len(), 20_000, "a second at 20 000 a second");
    assert!(got.iter().all(|m| m.1.len() == 40 && m.1[0] == 3), "junk wears the version byte radio 1 last heard");
    let log = w.borrow().log();
    assert_eq!(log.iter().filter(|l| l.ends_with("deliver 1 -> 0 \"t\" 40B ×2000")).count(), 10, "one line a step");
    assert!(!log.iter().any(|l| l.contains("send 1 \"t\"") || l.contains("expire 1")), "nothing in flight to log");
}

#[test]
fn a_holding_relay_sends_each_wire_it_heard_once_again_as_it_was_on_the_air_after_the_hold_and_across_a_reboot() {
    let (w, mut s) = world(3, "reticulum", "0 hold 1 5000\n2500 hold 1 0\n5000 peer_down 1\n8000 peer_up 1");
    s[1].listen("t").unwrap();
    s[2].listen("t").unwrap();
    run(&w, 1100);
    let wires: [&[u8]; 2] = [b"first", &[1, 0, 255, 7]];
    for wire in wires { s[0].send("t", wire, 1).unwrap(); }
    run(&w, 1500);
    s[0].send("t", b"after the hold stopped", 1).unwrap();
    run(&w, 4400);
    assert_eq!(messages(&mut s[2]).len(), 3, "the originals; the held two were due at 6100, with the relay down");
    run(&w, 1200);
    let got: Vec<Vec<u8>> = messages(&mut s[2]).into_iter().map(|m| m.1).collect();
    assert_eq!(got, wires.map(<[u8]>::to_vec), "kept through the reboot and sent once the relay is up, byte for byte, seven seconds past a one-second ttl");
    run(&w, 10_000);
    assert!(messages(&mut s[2]).is_empty(), "each held wire goes out once");
    assert_eq!(w.borrow().log().iter().filter(|l| l.contains("send 1 \"t\" ")).count(), 2, "from the relay, through the mesh, as replay sends");
    for p in ["reticulum", "dtn"] { // three noisy relays in a ring, each hearing every wire again in the other two's resends
        let (w, mut s) = world(5, p, "0 partition 0 2\n0 partition 0 3\n0 partition 0 4\n0 hold 1 100\n0 hold 2 100\n0 hold 3 100\n0 noise 1 500\n0 noise 2 500\n0 noise 3 500");
        for r in &mut s[1..] { r.listen("t").unwrap(); }
        run(&w, 1100);
        let t = std::time::Instant::now();
        let wires: Vec<Vec<u8>> = (0..10).map(|i| vec![i; 8]).collect();
        for wire in &wires { s[0].send("t", wire, 60).unwrap(); }
        run(&w, 10_000);
        let log = w.borrow().log();
        let sent: Vec<usize> = (0..5).map(|r| log.iter().filter(|l| text(l).starts_with(&format!("send {r} "))).count()).collect();
        assert_eq!(sent, [10, 10, 10, 10, 0], "{p}: each relay sends each wire once, so 40 in flight, not twice as many each hop, its own noise or not");
        assert!(log.iter().any(|l| l.contains("bit(s) flipped")) && messages(&mut s[4]).iter().all(|m| wires.contains(&m.1)), "{p}: the relays' peers took damaged copies, and a quiet radio hearing only their resends took the sender's bytes");
        assert!(t.elapsed().as_millis() < 500, "{p}");
    }
}

#[test]
fn past_max_inflight_a_send_is_not_queued_and_the_tail_counts_it() {
    let (w, mut s) = world(2, "reticulum", "");
    for i in 0..=sim::MAX_INFLIGHT as u32 + 1 { s[0].send("t", &i.to_le_bytes(), 1).unwrap(); }
    w.borrow_mut().step(1000);
    w.borrow_mut().step(1000);
    s[0].send("t", b"room again", 1).unwrap();
    assert_eq!(text(w.borrow().log().last().unwrap()), "world lines past 100000 counted, not written: send 948577, full 2, radio 2, expire 1048576", "two not queued, the rest expired, then room");
}

struct Rng(u64);
impl Rng {
    const fn next(&mut self) -> u64 { let mut x = self.0; x ^= x << 13; x ^= x >> 7; x ^= x << 17; self.0 = x; x }
    const fn below(&mut self, n: usize) -> usize { if n == 0 { 0 } else { (self.next() % n as u64) as usize } }
}
fn env(name: &str, default: u64) -> u64 { std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default) }

/// Every checked-in scenario, mutated at random, through the parser and, when it parses, the world: a code or a run, never a trap.
#[test]
fn the_parser_and_the_world_survive_mutated_scenarios() {
    const ALPHABET: &[u8] = b" 0123456789\nabcdefghijklmnopqrstuvwxyz_#";
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../scenarios/");
    let files: Vec<Vec<u8>> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "scenario")).map(|p| std::fs::read(p).unwrap()).collect();
    assert!(files.len() >= 8);
    let mut rng = Rng(env("MUDRPC_SEED", 1) ^ 5);
    for _ in 0..env("MUDRPC_FUZZ", 2_000) {
        let mut text = files[rng.below(files.len())].clone();
        match rng.below(3) {
            0 => for _ in 0..=rng.below(4) { let i = rng.below(text.len()); text[i] = rng.next() as u8; },
            1 => text.truncate(rng.below(text.len() + 1)),
            _ => { let i = rng.below(text.len() + 1); let ins: Vec<u8> = (0..rng.below(12)).map(|_| ALPHABET[rng.below(ALPHABET.len())]).collect(); text.splice(i..i, ins); }
        }
        if let Ok(s) = Scenario::parse(&String::from_utf8_lossy(&text)) {
            let (w, _) = sim::world(&s);
            for _ in 0..3 { w.borrow_mut().step(100); }
        }
    }
}
