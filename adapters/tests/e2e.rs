//! End-to-end runs of the standard scenarios on the simulator. Not part of every build: run at PR and wave level with
//! `cargo test -- --include-ignored`. Regenerate a checked-in log with
//! `cargo run -p mudrpc-adapters --example scenario -- scenarios/<name>.scenario [--gcm] [--signed] > scenarios/<name>[.gcm][.signed].log`
//! and review the diff; fleet-of-1024's and churn-of-1024's with `--peer-lines`.
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
    clippy::many_single_char_names, // test scripts: `a` and `b` are the peers, `w` a wire, `e` an envelope
)]
use mudrpc_adapters::harness::P;
use mudrpc_adapters::sim::{self, Scenario};
use mudrpc_adapters::transport::{pump, Transport};
use mudrpc_core::rpc::Incoming;
use mudrpc_core::seal::Key;
use std::cell::RefCell;
use std::rc::Rc;

mod support;
use support::{at, messages, run, scenario, text, world, DIR};

// ───────────────────────── the standard scenarios ─────────────────────────

const SCENARIOS: [&str; 16] = ["partition-heal", "jamming-window", "hostile-peer", "bandwidth-collapse", "rollback", "insider-flood", "rotation", "hostile-method", "noisy-radio", "stale-epoch", "torn-save", "fleet-of-40", "stranger", "hub-telemetry", "junk-at-bandwidth", "relay-holds-past-ttl"];

fn parse(name: &str) -> Scenario { Scenario::parse(&std::fs::read_to_string(format!("{DIR}{name}.scenario")).unwrap()).unwrap() }
fn harness(name: &str) -> Vec<String> { mudrpc_adapters::harness::run(&parse(name)) }

/// Lines containing `needle`, counting a collapsed "×N" line as N.
fn count(log: &[String], needle: &str) -> usize {
    log.iter().filter(|l| l.contains(needle)).map(|l| l.rsplit_once(" ×").and_then(|(_, n)| n.parse::<usize>().ok()).unwrap_or(1)).sum()
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn every_standard_scenario_is_deterministic_and_matches_its_checked_in_log() {
    for name in SCENARIOS {
        let (a, b) = (harness(name), harness(name));
        assert_eq!(a, b, "{name}: same seed, same log");
        let want = std::fs::read_to_string(format!("{DIR}{name}.log")).unwrap_or_else(|_| panic!("{name}.log missing: regenerate with the scenario example and review it"));
        assert_eq!(a.join("\n") + "\n", want, "{name}: log changed; if intended, regenerate with the scenario example and review the diff");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn partition_heal_expires_one_call_and_lands_the_rest_after_heal() {
    let log = harness("partition-heal");
    assert_eq!(count(&log, "peer0 expired"), 1, "only Ping#2 (ttl 10) dies");
    assert_eq!(count(&log, "peer1 call \"Ping\" \"2\""), 0, "and never reaches the drone");
    assert!(log.iter().any(|l| { let t = at(l); (30_000..=31_100).contains(&t) && l.contains("deliver 0 -> 1") }), "Ping#1 lands at the heal, once the announce has crossed the healed link");
    assert!(count(&log, "peer0 reply") >= 4);
    assert_eq!(count(&log, "pressure 0"), 2);
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn jamming_window_is_answered_by_the_unjammed_drone_and_late_answers_find_no_call() {
    let log = harness("jamming-window");
    let jammed = |l: &&String| (5000..25000).contains(&at(l)) && l.contains("deliver 0 -> 1");
    assert_eq!(log.iter().filter(jammed).count(), 0, "nothing reaches drone-1 while jammed");
    assert!(count(&log, "peer2 call \"Ping\"") >= 2, "drone-2 answers during the jam");
    assert!(count(&log, "peer1 call \"Ping\"") >= 1, "drone-1 gets the waiting calls after the jam");
    assert!(count(&log, "peer0 dropped NoPending") >= 1, "the late answers find their call already resolved at gcs");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn hostile_peer_costs_one_tag_check_per_junk_packet_and_nothing_else() {
    let log = harness("hostile-peer");
    let junk = count(&log, "deliver 2 -> 1 \"nav\"");
    assert!(junk >= 150, "the flood actually happened: {junk}");
    assert_eq!(count(&log, "peer1 dropped Open(Tag)") + count(&log, "peer1 dropped Duplicate"), junk, "every junk or replayed packet was dropped, each exactly once");
    assert_eq!(count(&log, "peer1 dropped Duplicate"), 1, "the replayed call is a duplicate, not junk");
    assert_eq!(count(&log, "pressure 0"), 3, "no table was touched");
    assert!(count(&log, "peer0 reply") >= 4, "legitimate calls complete under flood");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn bandwidth_collapse_delays_calls_by_their_airtime_and_recovers() {
    let log = harness("bandwidth-collapse");
    let sends: Vec<u64> = log.iter().filter(|l| l.contains("send 0 \"nav\"")).map(|l| at(l)).collect();
    let lands: Vec<u64> = log.iter().filter(|l| l.contains("deliver 0 -> 1 \"nav\"")).map(|l| at(l)).collect();
    let delays: Vec<u64> = sends.iter().zip(&lands).map(|(s, l)| l - s).collect();
    assert!(delays.iter().any(|&d| d >= 10_000), "a 49-byte call at 4 B/s lands about 12 s after it was made: {delays:?}");
    assert!(delays.iter().skip_while(|&&d| d < 10_000).skip(2).all(|&d| d == 0), "instant again once the limit lifts: {delays:?}");
    assert_eq!(count(&log, "peer0 expired"), 0, "nothing expires: ttl 20 survives 4 B/s, and Ping#2 was answered before the collapse");
    assert_eq!(count(&log, "pressure 0"), 2);
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_minute_offline_then_instant_catch_up() {
    let s = scenario(2, "reticulum", "15000 partition 0 1\n75000 heal 0 1");
    let (w, mut sims) = sim::world(&s);
    let mut a = P::new("gcs", Key::new([1; 32]), 1, 2).unwrap();
    let mut b = P::new("drone", Key::new([1; 32]), 2, 2).unwrap();
    b.listen("nav").unwrap();
    let mut events = Vec::new();
    let mut t = 0u64;
    while t <= 85_000 {
        let now = (t / 1000) as u32;
        if t == 20_000 { a.call(now, "nav", "Mission", b"upload", 120).unwrap(); a.call(now, "nav", "Steer", b"left", 2).unwrap(); }
        if t.is_multiple_of(10_000) { let _ = a.publish(now, "nav", b"pos", 15); }
        pump(&mut a, &mut sims[0], now, |_, i| match i { Incoming::Reply { bytes, .. } => events.push((t, format!("reply {:?}", String::from_utf8_lossy(bytes)))), Incoming::Expired { .. } => events.push((t, "expired".into())), _ => {} }).unwrap();
        pump(&mut b, &mut sims[1], now, |b, i| match i { Incoming::Call(c) => b.reply(now, &c, 0, 0, "", b"ok", true).unwrap(), Incoming::Event { .. } => events.push((t, "pos".into())), _ => {} }).unwrap();
        w.borrow_mut().step(100);
        t += 100;
    }
    let positions_before = events.iter().filter(|(t, e)| e == "pos" && *t < 15_000).count();
    let positions_after = events.iter().filter(|(t, e)| e == "pos" && *t >= 75_000).count();
    let positions_during = events.iter().filter(|(t, e)| e == "pos" && (15_000..75_000).contains(t)).count();
    assert_eq!((positions_before, positions_during), (2, 0), "telemetry with ttl 15: two before the cut, none while offline");
    assert!(positions_after >= 1, "and resumes at once");
    assert_eq!(events.iter().filter(|(_, e)| e == "expired").count(), 1, "the 2 s steer command expires; the 2 min mission upload does not");
    let reply = events.iter().find(|(_, e)| e.starts_with("reply")).expect("the mission upload is answered");
    assert!((75_000..76_000).contains(&reply.0), "within a second of reconnecting: {}", reply.0);
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_relay_that_dies_reboots_drifts_out_of_range_and_is_finally_lost() {
    let events = "0 partition 0 2\n5000 peer_down 1\n20000 peer_up 1\n60000 jam 1\n80000 heal 1\n100000 peer_down 1";
    let (w, mut s) = world(3, "dtn", events);
    s[2].listen("t").unwrap();
    run(&w, 100);
    let step_to = |w: &Rc<RefCell<sim::World>>, until: u64| while w.borrow().now_ms() < until { w.borrow_mut().step(100); };
    s[0].send("t", b"before death", 60).unwrap();
    step_to(&w, 10_000);
    assert!(messages(&mut s[2]).iter().any(|m| m.1 == b"before death"), "relayed in the 5 s before the relay died");
    s[0].send("t", b"while dead", 60).unwrap();
    step_to(&w, 25_000);
    assert!(messages(&mut s[2]).iter().any(|m| m.1 == b"while dead"), "held at the sender, relayed after the reboot");
    step_to(&w, 61_000);
    s[0].send("t", b"out of range", 60).unwrap();
    step_to(&w, 65_000);
    s[0].send("t", b"too short", 5).unwrap();
    step_to(&w, 85_000);
    let got = messages(&mut s[2]);
    assert!(got.iter().any(|m| m.1 == b"out of range"), "held through the jam, relayed when back in range");
    assert!(!got.iter().any(|m| m.1 == b"too short"), "a 5 s message cannot wait 15 s for a relay to come back in range");
    step_to(&w, 110_000);
    s[0].send("t", b"after the loss", 60).unwrap();
    step_to(&w, 175_000);
    assert!(messages(&mut s[2]).is_empty(), "no relay, no path, nothing arrives; the sender held it until ttl");
}

/// Every `rust` code block in adapters/SKILL.md compiles against the crate and runs, its call answered after the heal.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_adapter_skill_examples_run() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let skill = std::fs::read_to_string(format!("{dir}/SKILL.md")).unwrap();
    let mut body = String::new();
    let mut rest = skill.as_str();
    while let Some(i) = rest.find("```rust\n") {
        let start = i + 8;
        let end = start + rest[start..].find("```").unwrap();
        body.push_str(&rest[start..end]);
        body.push('\n');
        rest = &rest[end + 3..];
    }
    assert!(body.contains("sim::world"), "the skill has lost its example");
    let tmp = std::env::temp_dir().join("mudrpc-skill-adapters");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("Cargo.toml"), format!("[package]\nname = \"skill\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nmudrpc-core = {{ path = {:?} }}\nmudrpc-adapters = {{ path = {:?} }}\n[workspace]\n", format!("{dir}/../core"), dir)).unwrap();
    std::fs::write(tmp.join("src/main.rs"), format!("#![allow(unused)]\nfn main() {{\n{body}\n}}\n")).unwrap();
    let out = std::process::Command::new("cargo").args(["run", "-q"]).current_dir(&tmp).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "gcs: status 0 armed at 31300 ms\n");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn rollback_a_clone_from_an_old_image_is_named_after_the_floor_has_advanced() {
    let log = harness("rollback");
    assert_eq!(count(&log, "host: radio 2 is a clone of peer1"), 1);
    assert_eq!(count(&log, "peer0 reply"), 2, "the two answers before the reflash");
    assert!(count(&log, "peer0 rollback from sender 2") >= 3, "every answer from the clone carries epoch 1 under a floor at epoch 2\n{}", log.join("\n"));
    assert_eq!(count(&log, "peer0 dropped Replay"), 0, "a lower epoch is a rollback, never mistaken for a replay");
    assert!(count(&log, "peer0 expired") >= 2, "with no honest drone left, the calls expire (the last one after the run ends)");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn insider_flood_is_reported_once_a_second_rate_dropped_and_answered_around() {
    let log = harness("insider-flood");
    let floods = count(&log, "peer1 flood from sender 3");
    assert!((4..=6).contains(&floods), "one Flood per second for five seconds: {floods}");
    assert!(count(&log, "peer1 dropped Rate") > 100, "the rest of the flood is dropped as Rate");
    assert!(count(&log, "peer0 reply") >= 4, "gcs's calls are answered through the flood");
    assert_eq!(count(&log, "peer0 flood"), 0, "the flood never reached gcs's inbox");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn rotation_loses_nothing_and_names_the_slot_that_opened() {
    let log = harness("rotation");
    assert!(count(&log, " via Next") >= 1, "drone-1 rotated early: its answers opened via Next at gcs\n{}", log.join("\n"));
    assert!(count(&log, " via Previous") >= 1, "a wire under the old key after gcs rotated opened via Previous");
    assert_eq!(count(&log, "dropped Open(Tag)"), 0, "no wire was lost to the rotation");
    assert!(count(&log, "peer0 reply") >= 5);
}

/// A log line with its byte counts blanked: the signed wire is 64 bytes longer and the simulator prints sizes.
fn sizeless(l: &str) -> String {
    l.split(' ').map(|t| if t.ends_with('B') && t[..t.len() - 1].bytes().all(|b| b.is_ascii_digit()) && t.len() > 1 { "?B" } else { t }).collect::<Vec<_>>().join(" ")
}

/// A run held to its checked-in log: the first of `logs` that exists, byte for byte, as its peer lines where it was checked
/// in so; where none does, the unsigned log with byte counts blanked, since a signature is 64 bytes the unsigned run never carried.
fn held_to(name: &str, run: &[String], logs: &[&str]) {
    if let Some((log, want)) = logs.iter().find_map(|l| std::fs::read_to_string(format!("{DIR}{name}.{l}")).ok().map(|w| (l, w))) {
        let run = if want.contains(" world lines counted, not written: ") { sim::peer_lines(run) } else { run.to_vec() };
        assert_eq!(run.join("\n") + "\n", want, "{name}: differs from {name}.{log}; if intended, regenerate it with the scenario example and review the diff");
    } else {
        let want = std::fs::read_to_string(format!("{DIR}{name}.log")).unwrap();
        let (got, want): (Vec<_>, Vec<_>) = (run.iter().map(|l| sizeless(l)).collect(), want.lines().map(sizeless).collect());
        assert_eq!(got, want, "{name}: the 64 bytes changed what happened; if that is the scenario's point, check in {name}.signed.log, reviewed");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn every_standard_scenario_under_the_signed_sealer_matches_the_unsigned_log_but_for_sizes() {
    for name in SCENARIOS { held_to(name, &mudrpc_adapters::harness::run_signed(&parse(name)), &["signed.log"]); }
}

/// The same scenarios under AES-256-GCM, unsigned and signed: the logs `Key` wrote where the version byte changes no verdict;
/// where it does (the simulator's junk carries version byte 1, which a version-3 receiver drops as `Version(1)` before any
/// tag work), a `.gcm.log` or `.gcm.signed.log`, reviewed.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn every_standard_scenario_under_the_gcm_sealer_matches_the_chacha_log_or_its_reviewed_gcm_log() {
    for name in SCENARIOS {
        let s = parse(name);
        held_to(name, &mudrpc_adapters::harness::run_gcm(&s), &["gcm.log", "log"]);
        held_to(name, &mudrpc_adapters::harness::run_gcm_signed(&s), &["gcm.signed.log", "signed.log"]);
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_noisy_radio_loses_wires_and_nothing_corrupt_reaches_product_code() {
    let log = harness("noisy-radio");
    let drops: Vec<&String> = log.iter().filter(|l| l.contains(" dropped ")).collect();
    assert!(!drops.is_empty() && drops.iter().all(|l| l.contains("Open(Tag)")), "every drop is the tag refusing a damaged wire: {drops:?}");
    assert!(count(&log, "bit(s) flipped") > 0);
    assert!(log.iter().any(|l| at(l) < 30_000 && l.contains("peer0 reply")), "some calls get through the noise");
    assert!(log.iter().filter(|l| at(l) > 30_000 && l.contains("peer0 reply")).count() >= 2, "and all of them after it stops");
    assert!(log.iter().filter(|l| l.contains(" call ") || l.contains(" reply ")).all(|l| l.contains("\"Ping\"") || l.contains("reply seq=0 end=true status=0")), "no call or reply carries damaged content");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_stale_epoch_shows_as_duplicates_of_wires_never_sent_twice_and_a_torn_block_never_traps() {
    let log = harness("stale-epoch");
    let dup_after = log.iter().filter(|l| at(l) > 50_000 && l.contains("peer0 dropped Duplicate")).count();
    assert!(dup_after >= 1, "the second life's first replies wear nonces the first life used: {log:?}");
    assert!(log.iter().filter(|l| at(l) > 50_000 && l.contains("peer0 expired")).count() >= 1, "and those calls expire");
    assert!(log.iter().any(|l| at(l) > 50_000 && l.contains("peer0 reply")), "traffic recovers once the counter passes the first life");
    assert!(!log.iter().any(|l| at(l) > 20_000 && at(l) < 50_000 && l.contains("Duplicate")), "the first reboot, an honest one, loses nothing to dedupe");
    let log = harness("torn-save");
    assert!(log.iter().any(|l| l.contains("torn block")), "the host said what it did");
    assert!(log.iter().filter(|l| at(l) > 20_000 && l.contains("peer0 reply")).count() >= 3, "and the fleet carries on");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_hub_holds_a_fleets_telemetry_without_eviction_and_an_unsigned_leaf_says_it_cannot() {
    let s = parse("hub-telemetry");
    for log in [mudrpc_adapters::harness::run_signed(&s), mudrpc_adapters::harness::run_gcm_signed(&s)] {
        assert_eq!(count(&log, "peer0 event \"gcs\" \"pos\""), 120, "every report reached the hub once");
        assert_eq!(count(&log, "peer0 pressure 0"), 1, "the hub evicted no window and no seen entry");
        assert_eq!(count(&log, "Duplicate"), 0, "no honest wire is a duplicate");
    }
    let log = harness("hub-telemetry");
    assert_eq!(count(&log, "peer0 event \"gcs\" \"pos\""), 120);
    assert_eq!(count(&log, "peer0 pressure 0"), 0, "unsigned, gcs is a leaf: 40 senders through 16 windows, and its pressure says so");
    assert_eq!(count(&log, "peer0 pressure "), 1);
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn junk_at_a_radios_rate_costs_gcs_a_tag_check_a_wire_and_every_report_lands_every_second() {
    use mudrpc_adapters::harness::{run, run_gcm, run_gcm_signed, run_signed};
    let s = parse("junk-at-bandwidth");
    let quiet = Scenario { events: s.events.iter().filter(|e| !matches!(e.kind, sim::Kind::Flood(..))).cloned().collect(), ..s.clone() };
    let pressure = |log: &[String]| log.iter().filter(|l| l.contains(" pressure ")).map(|l| text(l).to_string()).collect::<Vec<_>>();
    for (mode, run) in [("Key", run as fn(&Scenario) -> Vec<String>), ("Signed", run_signed), ("Gcm", run_gcm), ("Signed<Gcm>", run_gcm_signed)] {
        let log = run(&s);
        let landed: Vec<(u64, &str)> = log.iter().filter(|l| l.contains("honest events landed")).map(|l| (at(l), text(l))).collect();
        let want: Vec<(u64, &str)> = (3..=32).map(|s| (s * 1000, "peer0 honest events landed this second: 4")).collect();
        assert_eq!(landed, want, "{mode}: all four drones' reports land in each of the flood's thirty seconds, and only gcs takes events");
        let junk = count(&log, "\"gcs\" 40B ×");
        assert_eq!(junk, 2 * (15 * 2000 + 15 * 20_000), "{mode}: two radios, 15 s at 2 000 a second and 15 s at 20 000");
        assert_eq!(count(&log, "peer0 dropped Open(Tag)"), junk, "{mode}: every junk wire is one failed tag check");
        assert!(log.iter().filter(|l| l.contains(" dropped ")).all(|l| l.contains("Open(Tag)") || l.contains("NoPending")), "{mode}: no other verdict but the late answers to a shared call");
        assert_eq!(pressure(&log), pressure(&run(&quiet)), "{mode}: the junk touched no table; a leaf's pressure is its honest telemetry's, as without the flood");
        assert!(count(&log, "peer0 reply") >= 3, "{mode}: gcs's calls are answered through the flood");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_relay_holding_wires_past_their_ttl_is_replay_where_the_sender_is_known_and_runs_once_at_a_stranger() {
    use mudrpc_adapters::harness::{run, run_gcm, run_gcm_signed, run_signed};
    let s = parse("relay-holds-past-ttl");
    for (mode, run) in [("Key", run as fn(&Scenario) -> Vec<String>), ("Signed", run_signed), ("Gcm", run_gcm), ("Signed<Gcm>", run_gcm_signed)] {
        let log = run(&s);
        let after = &log[log.iter().position(|l| l.contains("host: the clock jumps")).unwrap()..];
        let held = count(after, "send 3 \"nav\"");
        assert_eq!(held, 3, "{mode}: the relay sends gcs's first three calls again a week after it heard them");
        assert_eq!(count(after, "peer1 dropped Replay"), held, "{mode}: drone-1 keeps gcs's floor, so every held call is Replay");
        assert_eq!(count(after, "peer1 call \"Ping\" \"1\"") + count(after, "peer1 call \"Ping\" \"2\"") + count(after, "peer1 call \"Ping\" \"3\""), 0, "{mode}: and none runs twice there");
        for n in 1..=3 { assert_eq!(count(&log, &format!("peer2 call \"Ping\" \"{n}\"")), 1, "{mode}: drone-2 never heard gcs, so held Ping#{n} runs there exactly once"); }
        assert_eq!(count(&log, "peer2 call \"Ping\" \"4\""), 0, "{mode}: Ping#4, which the relay did not keep, never reaches drone-2");
        assert_eq!(count(&log, "peer2 dropped"), 0, "{mode}: no second copy, so nothing for drone-2's window to refuse");
        assert_eq!(count(after, "peer0 dropped NoPending ×3"), 3, "{mode}: drone-2's answers to week-old calls find none at gcs");
        assert_eq!(count(&log, "peer2 pressure 0"), 1, "{mode}: drone-2 had no window for gcs to lose, so pressure counts nothing; the held calls ran once, not twice");
        assert!(count(after, "peer0 reply") >= 3, "{mode}: fresh calls after the week are answered");
    }
}

// ───────────────────────── the campaign (item 14a): many peers running at once ─────────────────────────

/// A checked-in log, the artifact the campaign's assertions read; `the_campaigns_logs_match_the_code_under_every_sealer`
/// and the long tests hold each to the code.
fn kept(name: &str, suffix: &str) -> Vec<String> { std::fs::read_to_string(format!("{DIR}{name}.{suffix}")).unwrap().lines().map(str::to_string).collect() }

/// Whether this is the long run before a release, which sets `MUDRPC_FUZZ` as the long fuzz does: the runs past the push gate's budget.
fn long() -> bool { std::env::var_os("MUDRPC_FUZZ").is_some() }

/// A campaign scenario's four runs, each held to its checked-in log.
fn held_under_every_sealer(name: &str) {
    use mudrpc_adapters::harness::{run, run_gcm, run_gcm_signed, run_signed};
    let s = parse(name);
    held_to(name, &run(&s), &["log"]);
    held_to(name, &run_signed(&s), &["signed.log"]);
    held_to(name, &run_gcm(&s), &["gcm.log", "log"]);
    held_to(name, &run_gcm_signed(&s), &["gcm.signed.log", "signed.log"]);
}

/// The campaign's runs that fit the push gate, about 4 s; fleet-of-1024 (about 47 s under the dev profile) and all-at-once (19 s) are held before a release.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_campaigns_logs_match_the_code_under_every_sealer() {
    for name in ["fleet-of-256", "reflash-within-the-week"] { held_under_every_sealer(name); }
}

/// Before a release: about 47 s in release.
#[test]
#[ignore = "end to end, long, before a release: MUDRPC_FUZZ=1 cargo test --release -p mudrpc-adapters --test e2e -- --ignored long_"]
fn long_the_campaigns_largest_logs_match_the_code_under_every_sealer() {
    if !long() { return; }
    for name in ["fleet-of-1024", "all-at-once"] { held_under_every_sealer(name); }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_fleet_of_1024_has_every_call_answered_at_a_leaf_under_its_fleets_telemetry_and_the_hubs_roster_ends_at_255() {
    let log = kept("fleet-of-1024", "log");
    assert_eq!(count(&log, "peer0 event \"gcs\" \"pos\""), 3 * 80, "a leaf takes its 16 windows' reports of 1 023 in one second and 64 from the drones with none, who share one sender's cap");
    assert_eq!((count(&log, "peer0 flood from sender 82"), count(&log, "peer0 dropped Rate")), (3, 3 * 942), "the 65th of those named by the number it wrote, the other 942 reports a round Rate, and no answer is");
    assert!(log.contains(&"    15300 peer0 reply seq=0 end=true status=0 \"3\"".to_string()), "Ping#3 is answered in the second its fleet's telemetry was refused: an answer to our own call is outside both caps");
    assert_eq!((count(&log, "peer0 reply"), count(&log, "peer0 expired")), (6, 0), "six calls, six answered");
    assert_eq!(count(&log, "peer0 dropped NoPending"), 2044 + 4 * 1022, "the other answers find no pending call");
    assert_eq!(count(&log, "Duplicate"), 0);
    assert!(log.contains(&"    65100 peer0 pressure 368".to_string()), "honest: 64 newcomers refused a window three times, then 16, 80 and 80 seen entries evicted");
    let log = kept("fleet-of-1024", "signed.log");
    assert_eq!(count(&log, "peer0 event \"gcs\" \"pos\""), 3 * 255, "the hub takes every believed drone's report");
    assert_eq!(count(&log, "peer0 dropped Open(Roster)"), 3 * 768, "the 768 drones past the 256-line roster are strangers, nameless in the log");
    assert_eq!((count(&log, "peer0 reply"), count(&log, "peer0 expired")), (6, 0), "every call answered, outside the cap the hub's 255 reports a second leave one under");
    assert_eq!(count(&log, "peer0 dropped NoPending"), 2044 + 4 * 1022, "a stranger's answer to a resolved call is NoPending before the roster, indistinguishable from an honest late one");
    assert!(log.contains(&"    65100 peer0 pressure 0".to_string()));
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_fleet_of_256_lands_every_report_at_the_hub_and_one_cap_of_the_windowless_at_the_leaf_and_every_answer_at_both() {
    for (suffix, landed, rated, pressure) in [("log", 80, 174, "368"), ("signed.log", 255, 0, "0")] {
        let log = kept("fleet-of-256", suffix);
        assert_eq!((count(&log, "peer0 event \"gcs\" \"pos\""), count(&log, "Rate")), (3 * landed, 3 * rated), "{suffix}: the hub's 255 reports a round, one under RX_PER_SEC; the leaf's 16 windows' and 64 of the rest, the 65th named");
        assert_eq!((count(&log, "peer0 reply"), count(&log, "peer0 expired")), (6, 0), "{suffix}: every call answered");
        assert_eq!(count(&log, "peer0 dropped NoPending"), 2 * 254 + 4 * 254, "{suffix}");
        assert!(log.contains(&format!("    65100 peer0 pressure {pressure}")), "{suffix}: the leaf refuses 64 windows a round and evicts 16, 80 and 80 seen entries; the hub nothing");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_reflash_within_the_week_is_named_rollback_at_every_answer_as_after_the_week() {
    let log = kept("reflash-within-the-week", "log");
    assert_eq!(count(&log, "peer2 call \"Ping\""), 5, "the clone at epoch 1 hears every call");
    assert_eq!((count(&log, "peer0 reply"), count(&log, "peer0 dropped NoPending")), (2, 2), "drone-1's two answers; the clone's to the same calls find none pending");
    assert_eq!(count(&log, "peer0 rollback from sender 2"), 3, "each answer to a call made after gcs first heard epoch 2 is named inside its ttl, as rollback.log names the clone after the week");
    assert_eq!(count(&kept("rollback", "log"), "peer0 rollback from sender 2"), 3);
    assert_eq!(count(&log, "peer0 expired"), 2, "Ping#3 and Ping#4; Ping#5 outlives the run");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn everything_at_once_lands_every_report_it_has_room_for_answers_every_call_and_believes_the_clone_only_while_its_reports_could_straggle() {
    for (suffix, landed, pressure) in [("log", 80, 5225), ("signed.log", 119, 3047)] {
        let log = kept("all-at-once", suffix);
        let (before, after): (Vec<String>, Vec<String>) = log.iter().cloned().partition(|l| at(l) < 604_800_000);
        assert_eq!(count(&log, "peer0 dropped Open(Tag)"), 3 * 20_000 * 30, "{suffix}: 1.8 million junk wires, one tag check each, 6 000 a round");
        assert_eq!((count(&log, &format!("peer0 honest events landed this second: {landed}")), count(&log, &format!("peer0 honest events landed this second: {}", landed + 1))), (8, 22), "{suffix}: through the flood every report the receiver has room for lands every second, every drone's at the hub, the leaf's 16 windows' and the 64 the rest share, the clone's from 11 s under drone-5's window, the spammer's throughout");
        assert_eq!(count(&log, "peer0 event \"gcs\" \"pos\""), 34 * landed + 25 + 3 * landed, "{suffix}: 34 rounds of the drones it takes, the clone's 25 before the week, three rounds after it");
        assert_eq!(count(&log, "peer0 flood from sender 82"), if landed == 80 { 37 } else { 0 }, "{suffix}: at the leaf the 65th windowless drone named in every round, by the number it wrote");
        assert_eq!((count(&log, "peer0 reply"), count(&log, "peer0 expired")), (7, 0), "{suffix}: gcs is answered through all of it");
        assert_eq!((count(&before, "rollback"), count(&after, "peer0 rollback from sender 6")), (0, 3), "{suffix}: the clone of drone-5 speaks from 10 s, inside the 600 s an honest report of epoch 1 could arrive after gcs first heard epoch 2, and its answers lose to drone-5's; after the week its epoch is below the floor");
        assert_eq!(count(&after, "dropped Replay"), 2 * 119, "{suffix}: the relay's two held calls come back a week later as Replay at every drone that keeps gcs's floor");
        assert_eq!(count(&before, "dropped Duplicate"), 119, "{suffix}: the replayed Ping#3 is a duplicate at every drone");
        assert!(log.contains(&format!("604888100 peer0 pressure {pressure}")), "{suffix}: honest: the leaf's 16 windows and 64 seen entries against 120 senders; the hub's 1 024 seen entries against 120 a second with a 600 s ttl");
    }
    let log = kept("all-at-once", "log");
    assert_eq!((count(&log, "peer1 event \"nav\" \"spam\""), count(&log, "peer1 flood"), count(&log, "peer1 dropped Rate")), (74, 1, 25), "unsigned, a key holder's 100 wires a second are 64 events, one Flood and 25 Rate in the second they land in whole, 10 events in the next");
    let log = kept("all-at-once", "signed.log");
    assert_eq!((count(&log, "peer1 dropped Open(Roster)"), count(&log, "flood")), (100, 0), "signed, the spammer is outside the leaf's 16-line roster: every spam wire is Open(Roster), none a Flood");
}

/// Each (millisecond, sender) of gcs's reports, counted.
fn reports(log: &[String]) -> std::collections::BTreeMap<(u64, u32), usize> {
    let mut m = std::collections::BTreeMap::new();
    for l in log.iter().filter(|l| l.contains("peer0 event \"gcs\" \"pos\" from sender ")) {
        let sender = l.split("from sender ").nth(1).and_then(|s| s.split(' ').next()).and_then(|s| s.parse().ok()).unwrap();
        *m.entry((at(l), sender)).or_insert(0) += l.rsplit_once(" ×").and_then(|(_, n)| n.parse().ok()).unwrap_or(1);
    }
    m
}

/// Before a release: about 20 s a mode in release. A thousand drones joining late, dying, rebooting from their boot image and
/// reflashed, telemetry every 5 s for a minute, against the hub and the leaf.
#[test]
#[ignore = "end to end, long, before a release: MUDRPC_FUZZ=1 cargo test --release -p mudrpc-adapters --test e2e -- --ignored long_"]
fn long_churn_of_1024_the_hub_runs_no_reused_nonce_twice_and_the_leaf_names_each_it_did() {
    if !long() { return; }
    held_under_every_sealer("churn-of-1024");
    let (leaf, hub) = (kept("churn-of-1024", "log"), kept("churn-of-1024", "signed.log"));
    assert_eq!((count(&hub, "reboots from its boot image"), count(&hub, "is a clone of")), (400, 100));
    assert_eq!(count(&hub, "peer0 dropped Duplicate"), 200, "the hub keeps a window for every believed drone: each nonce the second stale reboot reuses, whose first copy it ran, is refused");
    let (l, h) = (reports(&leaf), reports(&hub));
    let twice: Vec<(u64, u32)> = l.iter().filter(|&(k, &n)| k.0 >= 45_000 && k.1 <= 256 && n > h.get(k).copied().unwrap_or(0)).map(|(k, _)| *k).collect();
    assert_eq!(twice.len(), 125, "a report from a drone the hub believes, run at the leaf after the second reboots and refused at the hub, is a reused nonce the leaf ran twice: its 16 windows hold none of these senders, the stated cost");
    let first = |m: &std::collections::BTreeMap<(u64, u32), usize>, s: u32| m.iter().filter(|(k, _)| k.1 == s && (20_000..45_000).contains(&k.0)).map(|(k, n)| (*k, *n)).collect::<Vec<_>>();
    assert!(twice.iter().all(|&(_, s)| first(&l, s) == first(&h, s)), "and the first life's reports of each ran at the leaf as at the hub, the first copy among them");
    assert_eq!(count(&leaf, "peer0 dropped Duplicate"), 33, "of the hub's 200 the leaf refused 25 reports and 8 answers; the other 42 answers found no call; no reused nonce fell to the cap");
    assert_eq!((count(&hub, "rollback"), count(&leaf, "rollback")), (0, 0), "a hundred clones at epoch 1 from 45 s, each report inside the 600 s an honest one of epoch 1 could arrive, each answer behind its original's: the stated cost");
    assert_eq!((count(&hub, "peer0 reply"), count(&hub, "peer0 expired")), (9, 0), "the hub answers every call whose ttl ended");
    assert_eq!((count(&leaf, "peer0 reply"), count(&leaf, "peer0 expired")), (9, 0), "and so does the leaf, each in a second of refused reports");
    assert_eq!((count(&leaf, "peer0 dropped Rate"), count(&leaf, "peer0 flood")), (8564, 15), "honest reports past the 16 windows and the 64 the rest share, lost, not delayed, the 65th named in each second that has one");
    assert_eq!(count(&hub, "peer0 dropped Open(Roster)"), 7901, "the drones past the roster, counted and not named: a refused wire's number is unauthenticated");
    assert!(leaf.contains(&"   110100 peer0 pressure 3085".to_string()) && hub.contains(&"   110100 peer0 pressure 1146".to_string()), "pressure counts every refused window and evicted seen entry");
}

/// Before a release: about a minute in release. The fleet of 1 024 reporting every second for a minute.
#[test]
#[ignore = "end to end, long, before a release: MUDRPC_FUZZ=1 cargo test --release -p mudrpc-adapters --test e2e -- --ignored long_"]
fn long_a_fleet_of_1024_reporting_for_a_minute_writes_every_peer_line_and_counts_the_world_lines_past_the_cap() {
    if !long() { return; }
    let s = Scenario { events: (1..=60).map(|s| sim::Event { at_ms: s * 1000, kind: sim::Kind::Telemetry, line: 0 }).collect(), ..parse("fleet-of-1024") };
    let log = mudrpc_adapters::harness::run(&s);
    assert_eq!(log.last().map(String::as_str), Some("   110100 world lines past 100000 counted, not written: send 24859, event 20, deliver 30690, expire 5121"), "one line a wire: the world's own lines end at 41 s, and the last line says how many of which kind followed");
    assert_eq!((count(&log, " pressure "), log[log.len() - 2].as_str()), (1024, "   110100 peer1023 pressure 0"), "every peer's pressure line, the summary, written past the cap");
    assert_eq!((count(&log, "peer0 event \"gcs\" \"pos\" from sender "), count(&log, "peer0 dropped Rate")), (60 * 80, 60 * 942), "every report the leaf took, named, and every one past its caps");
    assert_eq!((count(&log, "peer0 reply"), count(&log, "peer0 expired")), (9, 0), "every call answered");
    assert!(log.contains(&"   110100 peer0 pressure 8576".to_string()));
}

/// Before a release: about 50 s in release, its two runs held to counts, not to a checked-in log (137 000 lines and 5.2 MB a run).
/// Twelve minutes of a 200-fleet reporting every 5 s while 60 drones die for 30 s each, 60 reboot twice from their boot
/// image a minute apart, the second time reusing the first's nonces, and 20 clones at epoch 1 come up beside their
/// originals, one every 35 s.
#[test]
#[ignore = "end to end, long, before a release: MUDRPC_FUZZ=1 cargo test --release -p mudrpc-adapters --test e2e -- --ignored long_"]
fn long_churn_at_200_runs_no_nonce_twice_at_the_hub_answers_every_call_and_states_its_pressure() {
    if !long() { return; }
    let rows: [Vec<String>; 4] = [
        (1..=144).map(|k| format!("{} telemetry", k * 5000)).collect(),
        (1..=60).map(|d| format!("{} peer_down {d}\n{} peer_up {d}", d * 9000 + 1250, d * 9000 + 31_250)).collect(),
        (61..=120).map(|d| format!("{} stale_reboot {d}\n{} stale_reboot {d}", (d - 60) * 8000 + 2500, (d - 60) * 8000 + 62_500)).collect(),
        (0..20).map(|k| format!("{} clone {} {}", k * 35_000 + 23_750, 121 + k, 180 + k)).collect(),
    ];
    let s = scenario(200, "dtn", &rows.concat().join("\n"));
    let (leaf, hub) = (mudrpc_adapters::harness::run(&s), mudrpc_adapters::harness::run_signed(&s));
    let reused: usize = (61..=120u64).map(|d| { let t = (d - 60) * 8000 + 2500; s.events.iter().filter(|e| e.kind == sim::Kind::Telemetry && (t..t + 60_000).contains(&e.at_ms)).count() }).sum();
    assert_eq!((count(&hub, "peer0 dropped Duplicate"), count(&hub, "Replay"), reused), (720, 0, 720), "the hub keeps a window for every believed drone: each wire of a second stale life that reuses the nonce of a report of the first, which ran, is refused, and nothing else is; an answer of the first life was NoPending, never run, so its nonce's second wire runs once");
    assert_eq!((count(&leaf, "peer0 dropped Duplicate"), count(&leaf, "rollback")), (0, 0), "the leaf's 16 windows for 199 senders refuse none of the 720 and name no clone: the stated cost");
    assert_eq!(count(&hub, "peer0 rollback from sender "), 17 * 23 + 21 + 14 + 7, "a clone's reports are believed while an honest one of epoch 1 could arrive, 600 s from the hub's first report of epoch 2 at 5 s, and named from 610 s: 17 clones up by then, three later");
    for log in [&leaf, &hub] {
        assert_eq!((count(log, "gcs calls Ping#"), count(log, "peer0 reply"), count(log, "peer0 expired")), (52, 53, 0), "52 rounds of calls, two in the first: every call answered through the churn");
    }
    assert!(leaf.contains(&"   770100 peer0 pressure 21104".to_string()) && hub.contains(&"   770100 peer0 pressure 25069".to_string()), "pressure counts every refused window and evicted seen entry: 199 senders against the leaf's 16 windows, the strangers' 64 a second and 64 entries, and reports with a 600 s ttl against the hub's 1 024");
}

/// Peer `i`'s clock at world millisecond `t`, skewed: `i` × 3 days ahead and drifting (`i` × 37 mod 101 − 50) / `PER` of
/// a second a second.
#[allow(clippy::cast_possible_wrap)] // a peer index and hours of milliseconds
fn skewed<const PER: i64>(i: usize, t: u64) -> u64 { (i as u64 * 3 * 86_400_000).wrapping_add_signed(t as i64 + t as i64 * (i as i64 * 37 % 101 - 50) / PER) }

/// Before a release: about 6 s in release, three runs. Nothing on the wire is absolute, so skew between peers has no protocol
/// meaning: sixteen peers on a custody mesh, their clocks up to 45 days apart and drifting up to 5% against each other,
/// through six hours of gcs's calls every 15 s, reports every 5 s and gcs jammed for 5 s and 40 s in turn, see every
/// verdict they see on one clock, in order; at ten times the drift they do not. Over the `World` alone, since the harness
/// has one clock for every peer.
#[test]
#[ignore = "end to end, long, before a release: MUDRPC_FUZZ=1 cargo test --release -p mudrpc-adapters --test e2e -- --ignored long_"]
fn long_peers_whose_clocks_are_weeks_apart_and_drift_see_what_they_see_on_one_clock() {
    if !long() { return; }
    let jams: Vec<String> = (1..216u64).map(|k| format!("{} jam 0\n{} heal 0", k * 100_000 + 2500, k * 100_000 + 2500 + if k % 2 == 0 { 5000 } else { 40_000 })).collect();
    let run = |clock: fn(usize, u64) -> u64| {
        let (w, mut sims) = sim::world(&scenario(16, "dtn", &jams.join("\n")));
        let mut peers: Vec<P> = (0..16u32).map(|i| P::new(&if i == 0 { "gcs".into() } else { format!("drone-{i}") }, Key::new([1; 32]), i + 1, 2).unwrap()).collect();
        for p in &mut peers[1..] { p.listen("nav").unwrap(); }
        let mut seen = vec![Vec::new(); 16];
        for t in (0..21_600_000).step_by(100) {
            for (i, (p, radio)) in peers.iter_mut().zip(&mut sims).enumerate() {
                let now = (clock(i, t) / 1000) as u32;
                if i == 0 && t % 15_000 == 0 { p.call(now, "nav", "Ping", &t.to_le_bytes(), 20).unwrap(); }
                if i > 0 && t % 5000 == 2500 { p.publish(now, "gcs", b"pos", 30).unwrap(); }
                pump(p, radio, now, |p, inc| { seen[i].push(format!("{inc:?}")); if let Incoming::Call(c) = inc { p.reply(now, &c, 0, 0, "", b"pong", true).unwrap(); } }).unwrap();
            }
            w.borrow_mut().step(100);
        }
        seen
    };
    let one = run(|_, t| t);
    let n = |s: &str| one[0].iter().filter(|v| v.starts_with(s)).count();
    assert_eq!((n("Reply"), n("Expired"), n("Event"), n("Dropped(NoPending)")), (1296, 144, 59_940, 18_144), "gcs: of 1 440 calls the 144 made over 20 s before a 40 s jam ends expire and every other is answered by one drone first and fourteen after; 59 940 of 64 800 reports land, the rest dying in the jams");
    assert!(run(skewed::<1000>) == one, "a verdict moved with the skew");
    assert!(run(skewed::<100>) != one, "the control: drifting up to 50%, gcs's calls outlive or die inside jams they did not on one clock");
}

// ───────────────────────── the second reader (item 14a): five holes, each asserted as fixed ─────────────────────────

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_noisy_holding_ring_sends_each_wire_once_a_relay() {
    held_under_every_sealer("noisy-relay-ring");
    let log = kept("noisy-relay-ring", "log");
    let sent: Vec<usize> = (1..4).map(|r| count(&log, &format!("send {r} \"nav\""))).collect();
    assert_eq!(sent, [2, 2, 2], "14a.6: each relay heard gcs's two calls and sends each once, holding the wire as it was on the air");
    assert!(count(&log, "bit(s) flipped") > 0 && count(&log, "deliver") < 40, "the noise still damages the peers' copies, and the air stays bounded");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_warp_off_the_round_grid_keeps_gcs_calling_every_150_rounds() {
    held_under_every_sealer("warp-off-the-round");
    let log = kept("warp-off-the-round", "log");
    let calls: Vec<u64> = log.iter().filter(|l| l.contains("gcs calls Ping#")).map(|l| at(l)).collect();
    assert_eq!(calls, [0, 15_000, 604_830_001, 604_845_001, 604_860_001, 604_875_001, 604_890_001, 604_905_001, 604_920_001], "14a.7: the cadence counts rounds, so after a warp of 604 800 001 ms gcs calls on, a millisecond off the grid");
    assert!(log.contains(&"604880001 host: every drone publishes its position to gcs".to_string()) && count(&log, "peer0 event") == 2 && count(&log, "peer0 reply") == 10, "and every call is answered with the fleet alive");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_refused_call_or_answer_has_its_line() {
    held_under_every_sealer("full-at-both-ends");
    let log = kept("full-at-both-ends", "log");
    assert_eq!((count(&log, "host: peer0 calls a hostile method"), log.iter().filter(|l| at(l) == 500 && l.contains("send 0 \"nav\"")).count(), log.iter().filter(|l| at(l) == 500 && l.ends_with(" peer0 refused call Full")).count()), (18, 16, 2), "14a.8: the host called eighteen times, sixteen left, and the two Full for want of a pending slot each say so");
    assert_eq!((log.iter().filter(|l| at(l) == 604_821_100 && l.contains("peer2 call")).map(|l| l.rsplit_once(" ×").and_then(|(_, n)| n.parse::<usize>().ok()).unwrap_or(1)).sum::<usize>(), log.iter().filter(|l| at(l) == 604_821_200 && l.contains("send 2 \"gcs\"")).count()), (18, 16), "drone 2 ran the eighteen held calls in one round and sixteen answers left: its outbox is sixteen");
    assert!(log.contains(&"604821100 peer2 refused reply Full ×2".to_string()) && log.contains(&"604821300 peer0 dropped NoPending ×16".to_string()), "and the two refused answers have their line");
    assert_eq!(count(&log, " refused "), 4, "nothing else was refused");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn five_senders_at_the_cap_spend_every_receivers_second_and_each_is_named_there() {
    held_under_every_sealer("starved-command");
    let log = kept("starved-command", "log");
    let ran = |t: u64| (1..8).filter(|p| log.iter().any(|l| at(l) == t && l.contains(&format!("peer{p} call \"\\u{{1b}}")))).count();
    assert_eq!((ran(10_100), ran(10_800)), (7, 0), "the stated cost: the call at 10.0 s is first in its second and all seven peers run it; the one at 10.7 s arrives after the five spent the global cap, and nobody runs it");
    assert_eq!((count(&log, "peer0 reply"), count(&log, "peer0 expired"), log.contains(&"    30000 peer0 expired".to_string())), (7, 1, true), "the operator's second call expires");
    let named = |p: u32, s: u64| log.iter().filter(|l| at(l) / 1000 == s && l.contains(&format!("peer{p} flood from sender "))).map(|l| l.rsplit(' ').next().unwrap().parse().unwrap()).collect::<std::collections::BTreeSet<u32>>();
    for s in [10, 11] {
        for p in [6, 7] { assert_eq!(named(p, s), (2..=6).collect(), "second {s}: each honest drone names the five as over their share, and not gcs"); }
        for p in 1..=5 { assert_eq!(named(p, s), (2..=6).filter(|&n| n != p + 1).collect(), "second {s}: each flooder names the other four"); }
    }
    assert_eq!(count(&log, "flood from"), 2 * (2 * 5 + 5 * 4), "once a second each");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_caller_whose_radio_is_down_still_ticks() {
    held_under_every_sealer("down-caller");
    let log = kept("down-caller", "log");
    assert_eq!(log.iter().filter(|l| l.contains("expired")).map(|l| at(l)).collect::<Vec<_>>(), [35_000], "14a.10: Ping#3 (15 s, ttl 20) expires at 35 s with the radio down: a transport error ends the drain, not the round, as in every harness");
    assert!(log.contains(&"    40400 peer0 reply seq=0 end=true status=0 \"4\"".to_string()), "and Ping#4, queued while down, leaves when the radio is up and is answered");
}

// ───────────────────────── the third reader (item 14a): three holes in the tools, each asserted as fixed ─────────────────────────

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_warp_on_the_call_round_runs_that_round_again_at_the_new_clock_and_both_calls_are_answered() {
    held_under_every_sealer("warp-on-the-call");
    let log = kept("warp-on-the-call", "log");
    assert!(log.contains(&"604815300 peer0 reply seq=0 end=true status=0 \"x\"".to_string()) && log.contains(&"604815300 peer0 reply seq=0 end=true status=0 \"3\"".to_string()) && count(&log, "peer0 expired") == 0, "14a.12: the round a warp lands on runs again at the new now in every harness, so the host's call and Ping#3 are made a week on and answered");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn every_call_a_round_holds_is_answered_or_refused_and_host_events_keep_file_order_within_a_tick() {
    held_under_every_sealer("many-calls-one-round");
    let log = kept("many-calls-one-round", "log");
    assert!(log.contains(&"      600 peer1 refused reply Full ×64".to_string()) && log.iter().any(|l| at(l) == 600 && l.ends_with("\"x\" ×80") && l.contains("peer1 call")), "14a.13: eighty calls in one round at drone 1, sixteen answered and 64 refused with their line, in the C harness as in Rust");
    held_under_every_sealer("unsorted-host-events");
    let log = kept("unsorted-host-events", "log");
    let host: Vec<&str> = log.iter().filter(|l| at(l) == 1000).map(|l| text(l)).filter(|t| t.starts_with("host:")).collect();
    assert_eq!(host, ["host: every drone publishes its position to gcs", "host: peer0 calls a hostile method"], "14a.13: file order within a tick, as the world keeps it, in every harness");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_simulator_delivers_nothing_across_a_warp_that_died_inside_it() {
    held_under_every_sealer("expired-across-the-warp");
    let log = kept("expired-across-the-warp", "log");
    assert!(count(&log, "604805000 expire 0 \"nav\"") == 2 && log.contains(&"604805000 peer0 expired ×2".to_string()) && !log.iter().any(|l| l.contains("call \"Ping\" \"1\"") || l.contains("call \"Ping\" \"2\"")), "14a.14: Ping#1 (ttl 60) and Ping#2 (ttl 10), in custody for a drone that was down, died inside the week's warp, so neither is delivered when it comes up");
    assert!(log.contains(&"604815100 peer1 call \"Ping\" \"3\"".to_string()), "and the next call after the warp runs");
}

// ───────────────────────── the last reader (item 14a): the round restart and the ttl edge, pinned in every harness ─────────────────────────

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_warp_at_zero_and_two_in_one_round_each_run_their_round_again_and_a_wire_is_read_in_its_last_second_alive() {
    held_under_every_sealer("warps-in-one-round");
    let log = kept("warps-in-one-round", "log");
    let jumps: Vec<u64> = log.iter().filter(|l| l.contains("host: the clock jumps")).map(|l| at(l)).collect();
    assert_eq!((jumps, count(&log, "gcs calls Ping#1")), (vec![0, 5000, 6000], 1), "the round a warp lands on runs again at the new clock: at t = 0, and twice in one round");
    assert!(log.contains(&"     1000 gcs calls Ping#1 ttl 60 and Ping#2 ttl 10".to_string()) && log.contains(&"    19000 gcs calls Ping#3 ttl 20".to_string()), "gcs's first calls are made a second on, and its cadence counts rounds across both warps");
    assert!(log.contains(&"    10900 peer1 call \"Ping\" \"2\"".to_string()) && log.contains(&"    11000 peer0 expired".to_string()) && log.contains(&"    11100 peer0 dropped NoPending".to_string()), "Ping#2 (ttl 10, made at 1 s) runs at drone-1 at 10.9 s, its last second alive, and its answer finds the call expired");
    assert!(log.contains(&"    61000 expire 0 \"nav\"".to_string()) && count(&log, "peer2 call \"Ping\" \"1\"") == 0, "Ping#1 (ttl 60) is dead at the end of the step the heal to drone-2 lands on, so it never moves");
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn a_warp_past_the_runs_end_carries_the_end_with_it_and_the_host_events_at_its_millisecond_are_performed() {
    held_under_every_sealer("warp-past-the-end");
    let log = kept("warp-past-the-end", "log");
    assert!(["604805000 host: every drone publishes its position to gcs", "604805000 host: peer0 calls a hostile method", "604805000 host: the clock jumps 1000 ms"].iter().all(|l| log.contains(&(*l).to_string())), "the three host events at the last event's millisecond, which the first warp outran, are performed in the restarted round");
    assert_eq!((count(&log, "peer0 event \"gcs\" \"pos\""), count(&log, "peer0 reply seq=0 end=true status=0 \"x\"")), (2, 1), "both reports land and the hostile call is answered");
    assert_eq!(log.last().map(String::as_str), Some("604856100 peer2 pressure 0"), "the run ends 50 s after the clock the last warp left, not 50 s after the last event's 5 s");
}
