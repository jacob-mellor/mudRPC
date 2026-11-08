//! The loopback rung: real UDP sockets on one machine, the sim's fault model, the standard harness.
#![warn(clippy::pedantic, clippy::nursery)]
use mudrpc_adapters::lan::Lan;
use mudrpc_adapters::sim::Scenario;
use mudrpc_adapters::transport::{Rx, Transport};
use mudrpc_adapters::harness;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

mod support;
use support::{at, messages, text, DIR};
const SPEED: u64 = 200;

fn scenario(text: &str) -> Scenario { Scenario::parse(text).unwrap() }

/// One Lan per radio on loopback, all knowing each other, sharing t = 0.
fn radios(s: &Scenario) -> (Vec<Lan>, Instant) {
    let start = Instant::now();
    let mut lans: Vec<Lan> = (0..s.peers).map(|i| Lan::new(s, i, "127.0.0.1:0".parse().unwrap(), SPEED, start).unwrap()).collect();
    let addrs: Vec<SocketAddr> = lans.iter().map(Lan::local_addr).collect();
    for l in &mut lans { for (p, a) in addrs.iter().enumerate() { l.peer(p, *a).unwrap(); } }
    (lans, start)
}

fn now_ms(start: Instant) -> u64 { u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX) * SPEED }

fn wait_until(start: Instant, t_ms: u64) { while now_ms(start) < t_ms { std::thread::sleep(Duration::from_micros(200)); } }

/// Drive every radio (a Lan only moves its queue when called) until radio `i` has a message, or 50 ms pass.
fn settle(l: &mut [Lan], i: usize) -> Vec<(String, Vec<u8>)> {
    for _ in 0..25 {
        for (j, r) in l.iter_mut().enumerate() { if j != i { while r.poll().is_some() {} } }
        let v = messages(&mut l[i]);
        if !v.is_empty() { return v; }
        std::thread::sleep(Duration::from_millis(2));
    }
    vec![]
}

#[test]
fn refuses_anything_but_private_and_loopback_addresses() {
    let s = scenario("seed 1\npeers 2\nprofile dtn\n");
    assert!(Lan::new(&s, 0, "8.8.8.8:0".parse().unwrap(), 1, Instant::now()).is_err());
    assert!(Lan::new(&s, 0, "0.0.0.0:0".parse().unwrap(), 1, Instant::now()).is_err(), "all interfaces includes public ones");
    let mut ok = Lan::new(&s, 0, "127.0.0.1:0".parse().unwrap(), 1, Instant::now()).unwrap();
    assert!(ok.peer(1, "1.1.1.1:5000".parse().unwrap()).is_err());
    assert!(ok.peer(1, "10.0.0.7:5000".parse().unwrap()).is_ok());
    assert!(ok.peer(1, "192.168.1.9:5000".parse().unwrap()).is_ok());
}

#[test]
fn refuses_a_scenario_with_a_kind_only_the_simulator_applies_naming_it_and_its_line() {
    for (row, kind) in [("malformed 1 nav", "malformed"), ("replay 1", "replay"), ("flood 1 nav 100", "flood"), ("noise 1 5", "noise"), ("hold 1 1000", "hold"), ("bandwidth 1 4", "bandwidth")] {
        let s = scenario(&format!("seed 1\npeers 2\nprofile dtn\n0 partition 0 1\n\n5000 {row}\n"));
        let e = Lan::new(&s, 0, "127.0.0.1:0".parse().unwrap(), 1, Instant::now()).err().unwrap_or_else(|| panic!("{kind} accepted"));
        assert_eq!(e.to_string(), format!("line 6: `{kind}` is the simulator's to apply, and over loopback the run would be a clean one"));
    }
    let s = scenario("seed 1\npeers 3\nprofile dtn\n0 partition 0 1\n0 jam 2\n0 heal 2\n0 peer_down 1\n0 peer_up 1\n0 heal 0 1\n0 telemetry\n0 spam 1 5\n0 warp 1000\n");
    assert!(Lan::new(&s, 0, "127.0.0.1:0".parse().unwrap(), 1, Instant::now()).is_ok(), "the link kinds and the host events are loopback's");
}

#[test]
fn partitioned_message_waits_at_the_sender_and_lands_after_heal_within_ttl() {
    let s = scenario("seed 1\npeers 2\nprofile dtn\n0 partition 0 1\n3000 heal 0 1");
    let (mut l, start) = radios(&s);
    l[1].listen("t").unwrap();
    l[0].send("t", b"long", 60).unwrap();
    l[0].send("t", b"short", 1).unwrap();
    wait_until(start, 2500);
    assert!(messages(&mut l[1]).is_empty(), "still partitioned");
    wait_until(start, 3100);
    assert_eq!(settle(&mut l, 1), [("t".to_string(), b"long".to_vec())], "the 60 s message landed; the 1 s one never will");
}

#[test]
fn two_listeners_both_receive_and_a_non_listener_hears_nothing() {
    let s = scenario("seed 1\npeers 3\nprofile dtn\n");
    let (mut l, _) = radios(&s);
    l[1].listen("t").unwrap();
    l[2].listen("other").unwrap();
    l[0].send("t", b"x", 10).unwrap();
    assert_eq!(settle(&mut l, 1).len(), 1);
    assert!(settle(&mut l, 2).is_empty(), "filtered by topic at the receiver");
    assert!(messages(&mut l[0]).is_empty(), "a sender does not hear itself");
}

#[test]
fn silent_holds_outgoing_and_still_receives() {
    let s = scenario("seed 1\npeers 2\nprofile dtn\n");
    let (mut l, _) = radios(&s);
    l[0].listen("in").unwrap();
    l[1].listen("out").unwrap();
    l[0].silent(true);
    l[0].send("out", b"held", 60).unwrap();
    l[1].send("in", b"arrives", 60).unwrap();
    assert_eq!(settle(&mut l, 0).len(), 1);
    assert!(settle(&mut l, 1).is_empty());
    l[0].silent(false);
    assert_eq!(settle(&mut l, 1).len(), 1, "released");
}

#[test]
fn link_events_and_a_dead_radio() {
    let s = scenario("seed 1\npeers 2\nprofile dtn\n1000 peer_down 1\n2000 peer_up 1");
    let (mut l, start) = radios(&s);
    assert_eq!(l[0].poll(), Some(Rx::OnMesh));
    wait_until(start, 1100);
    assert_eq!(l[0].poll(), Some(Rx::OffMesh), "the only other radio died");
    l[1].listen("t").unwrap();
    l[0].send("t", b"while dead", 1).unwrap();
    wait_until(start, 2100);
    assert_eq!(l[0].poll(), Some(Rx::OnMesh));
    assert!(settle(&mut l, 1).is_empty(), "a 1 s message did not survive a 1 s outage");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn partition_heal_over_loopback_shows_the_same_peer_events_as_the_simulator() {
    let s = scenario(&std::fs::read_to_string(format!("{DIR}partition-heal.scenario")).unwrap());
    let (mut l, start) = radios(&s);
    let mut log = Vec::new();
    harness::run_with::<_, mudrpc_core::seal::Key>(&s, &mut l, |t, tick| match tick {
        harness::Tick::Note(n) => log.push(format!("{t:>9} {n}")),
        harness::Tick::Step => wait_until(start, t + harness::STEP_MS),
        harness::Tick::Warp(_) => unreachable!("partition-heal has no warp"),
    });
    let text = log.join("\n");
    assert_eq!(text.matches("peer0 expired").count(), 1, "Ping#2 (ttl 10) expires exactly once\n{text}");
    let heal = 30_000 - 2 * harness::STEP_MS; // the wall clock may cross the heal inside a round
    assert!(log.iter().any(|l| l.contains("peer1 call \"Ping\" \"1\"") && at(l) >= heal), "Ping#1 lands at the heal, not before\n{text}");
    assert!(!log.iter().any(|l| l.contains("peer1 call \"Ping\" \"2\"")), "Ping#2 (ttl 10) never lands\n{text}");
    assert!(text.matches("peer0 reply").count() >= 4, "{text}");
    assert_eq!(text.matches("pressure 0").count(), 2);
}

#[test]
fn a_datagram_from_an_unlisted_socket_or_longer_than_a_wire_never_reaches_poll() {
    let s = scenario("seed 1\npeers 2\nprofile dtn\n");
    let (mut l, _) = radios(&s);
    l[1].listen("t").unwrap();
    let stranger = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut frame = vec![0u8, 1, b't'];
    frame.extend_from_slice(b"from nowhere");
    stranger.send_to(&frame, l[1].local_addr()).unwrap();
    assert!(settle(&mut l, 1).is_empty(), "a well-formed frame naming radio 0 from a socket that is not radio 0's");
    let listed = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    l[1].peer(0, listed.local_addr().unwrap()).unwrap();
    let mut long = vec![0u8, 1, b't'];
    long.resize(mudrpc_adapters::lan::MAX_DATAGRAM + 1, 0xAB);
    listed.send_to(&long, l[1].local_addr()).unwrap();
    assert!(settle(&mut l, 1).is_empty(), "one byte longer than any wire, from the listed peer");
    listed.send_to(&frame, l[1].local_addr()).unwrap();
    assert_eq!(settle(&mut l, 1), vec![("t".to_string(), b"from nowhere".to_vec())], "the same frame from the listed socket lands");
}

/// A harness note's text with its "×N" count split off, so two runs compare by how often each event happened, not by
/// which round collapsed it: the loopback rung is real time, and a burst the simulator lands in one round lands in two here.
/// An event's or a flood's sender is blanked too: which of 256 equal drones is the 65th windowless one named in a second
/// is the order their wires arrive in, the sockets' to decide, so the counts hold and the attribution is the simulator's.
fn tally(log: &[String]) -> std::collections::BTreeMap<String, usize> {
    let mut m = std::collections::BTreeMap::new();
    for l in log {
        let note = text(l).to_string();
        let (mut text, n) = note.rsplit_once(" ×").and_then(|(t, n)| n.parse::<usize>().ok().map(|n| (t.to_string(), n))).unwrap_or((note, 1));
        if let Some((head, tail)) = text.rsplit_once(" from sender ") {
            if tail.bytes().all(|b| b.is_ascii_digit()) && (head.contains(" flood") || head.contains(" event ")) { text = format!("{head} from sender ?"); }
        }
        *m.entry(text).or_insert(0) += n;
    }
    m
}

/// The rung's ceiling: 256 radios, the most a one-byte radio index allows, each a UDP socket in this process (a stock
/// macOS shell's `ulimit -n` of 256 is too few). Every peer event the simulator logs happens as often over loopback.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_fleet_of_256_over_loopback_shows_the_simulators_peer_events_as_often() {
    let s = scenario(&std::fs::read_to_string(format!("{DIR}fleet-of-256.scenario")).unwrap());
    let (mut l, start) = radios(&s);
    let mut log = Vec::new();
    let began = Instant::now();
    harness::run_with::<_, mudrpc_core::seal::Key>(&s, &mut l, |t, tick| match tick {
        harness::Tick::Note(n) => log.push(format!("{t:>9} {n}")),
        harness::Tick::Step => wait_until(start, t + harness::STEP_MS),
        harness::Tick::Warp(_) => unreachable!("fleet-of-256 has no warp"),
    });
    let wall = began.elapsed();
    let sim: Vec<String> = harness::run(&s).into_iter().filter(|l| { let n = text(l); n.starts_with("peer") || n.starts_with("gcs ") || n.starts_with("host:") }).collect();
    let (want, got) = (tally(&sim), tally(&log));
    let diff: Vec<String> = want.iter().filter(|(k, v)| got.get(*k) != Some(v)).map(|(k, v)| format!("{k}: simulator {v}, loopback {}", got.get(k).copied().unwrap_or(0))).chain(got.iter().filter(|(k, _)| !want.contains_key(*k)).map(|(k, v)| format!("{k}: simulator 0, loopback {v}"))).collect();
    assert!(diff.is_empty(), "peer events differ over loopback ({} radios, {wall:?}):\n{}", s.peers, diff.join("\n"));
    eprintln!("fleet-of-256 over loopback: {} notes in {wall:?}", log.len());
}
