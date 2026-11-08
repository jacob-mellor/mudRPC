//! `mud provision` run as an operator runs it, its files read as a host reads them at boot.
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(clippy::many_single_char_names)] // test scripts: `a` and `b` are the peers, `w` a wire, `c` a call
use mudrpc_auth::signed::Signed;
use mudrpc_core::rpc::{Dropped, Incoming, Outgoing, Peer};
use mudrpc_core::seal::{self, Key};
use std::path::{Path, PathBuf};

type N = Peer<Signed<Key, 16>, 512, 16, 64, 16, 16>;

fn fresh(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mud-provision-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}
fn mud(args: &[&str]) -> std::process::Output { std::process::Command::new(env!("CARGO_BIN_EXE_mud")).args(args).output().unwrap() }
fn provision(d: &Path, n: &str) -> Option<i32> { mud(&["provision", d.to_str().unwrap(), n]).status.code() }
fn read(d: &Path, f: &str) -> String { std::fs::read_to_string(d.join(f)).unwrap() }

/// What any host does at boot: split each line on its space, decode the hex.
fn value(text: &str, name: &str) -> String { text.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')).unwrap().into() }
fn hex(s: &str) -> [u8; 32] {
    assert_eq!(s.len(), 64);
    std::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
}
fn boot(d: &Path, device: u32, believe: &[u32]) -> N {
    let dev = read(d, &format!("device-{device}"));
    let mut s = Signed::new(Key::new(hex(&value(&read(d, "fleet.key"), "key"))), hex(&value(&dev, "seed")), 1000);
    for l in read(d, "roster").lines() {
        let (n, pk) = l.split_once(' ').unwrap();
        if believe.contains(&n.parse().unwrap()) { s.roster(n.parse().unwrap(), hex(pk), None).unwrap(); }
    }
    N::new(&format!("device-{device}"), s, value(&dev, "sender").parse().unwrap(), value(&dev, "epoch").parse().unwrap()).unwrap()
}
fn wire(n: &mut N) -> (String, Vec<u8>) {
    let mut v = Vec::new();
    while let Some(o) = n.outgoing(0) {
        if let Outgoing::Send { topic, wire, .. } = o { v.push((topic.to_string(), wire.to_vec())); }
        n.sent();
    }
    assert_eq!(v.len(), 1);
    v.remove(0)
}
fn deliver(n: &mut N, w: &(String, Vec<u8>)) -> Option<Dropped> {
    match n.deliver(0, &w.0, &w.1, &mut [0; 512]) { Incoming::Dropped(d) => Some(d), _ => None }
}
/// `a` calls `b` and `b` answers: each believes the other.
fn calls(a: &mut N, b: &mut N) {
    let id = a.call(0, "nav", "Ping", b"", 30).unwrap();
    let w = wire(a);
    let mut buf = [0; 512];
    let Incoming::Call(c) = b.deliver(0, &w.0, &w.1, &mut buf) else { panic!("refused") };
    b.reply(0, &c, 0, 0, "", b"pong", true).unwrap();
    let r = wire(b);
    assert!(matches!(a.deliver(0, &r.0, &r.1, &mut buf), Incoming::Reply { id: got, bytes: b"pong", .. } if got == id));
}

#[test]
fn two_provisioned_devices_believe_each_other_and_refuse_a_third_and_another_fleet() {
    let (d, other) = (fresh("fleet"), fresh("other"));
    assert_eq!(provision(&d, "2"), Some(0));
    let key = read(&d, "fleet.key");
    assert_eq!(provision(&d, "1"), Some(0));
    assert_eq!(read(&d, "fleet.key"), key, "never rewritten");
    assert!(key.starts_with("key ") && key.len() == 69);
    let roster = read(&d, "roster");
    assert_eq!(roster.lines().map(|l| l.split(' ').next().unwrap()).collect::<Vec<_>>(), ["1", "2", "3"]);
    for s in 1..=3 { assert_eq!(read(&d, &format!("device-{s}")).lines().take(2).collect::<Vec<_>>(), [format!("sender {s}"), "epoch 1".into()]); }
    #[cfg(unix)]
    for f in ["fleet.key", "device-1", "device-2", "device-3"] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(d.join(f)).unwrap().permissions().mode() & 0o777, 0o600, "{f}");
    }
    let (mut one, mut two) = (boot(&d, 1, &[1, 2]), boot(&d, 2, &[1, 2]));
    calls(&mut one, &mut two);
    calls(&mut two, &mut one);
    let mut three = boot(&d, 3, &[1, 2, 3]);
    three.publish(0, "nav", b"", 30).unwrap();
    let w = wire(&mut three);
    assert_eq!((deliver(&mut one, &w), deliver(&mut two, &w)), (Some(Dropped::Open(seal::Error::Roster)), Some(Dropped::Open(seal::Error::Roster))), "in the fleet, not in their rosters");
    assert_eq!(provision(&other, "1"), Some(0));
    let mut stranger = boot(&other, 1, &[1]);
    stranger.publish(0, "nav", b"", 30).unwrap();
    let w = wire(&mut stranger);
    assert_eq!((deliver(&mut one, &w), deliver(&mut two, &w)), (Some(Dropped::Open(seal::Error::Tag)), Some(Dropped::Open(seal::Error::Tag))), "another fleet key");
    let mut secrets: Vec<String> = [&d, &other].iter().flat_map(|x| [value(&read(x, "fleet.key"), "key"), value(&read(x, "device-1"), "seed"), read(x, "roster")[2..66].into()]).collect();
    secrets.sort();
    secrets.dedup();
    assert_eq!(secrets.len(), 6, "two runs share no key, seed or public key");
    for x in [&d, &other] { std::fs::remove_dir_all(x).unwrap(); }
}

#[test]
fn an_existing_file_or_a_roster_without_its_key_refuses_the_run_and_no_dir_is_usage() {
    let d = fresh("refuse");
    assert_eq!(provision(&d, "3"), Some(0));
    std::fs::write(d.join("device-4"), "mine").unwrap();
    let (roster, files) = (read(&d, "roster"), std::fs::read_dir(&d).unwrap().count());
    let out = mud(&["provision", d.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("device-4 exists"));
    assert_eq!((read(&d, "roster"), read(&d, "device-4"), std::fs::read_dir(&d).unwrap().count()), (roster, "mine".into(), files), "nothing written");
    std::fs::remove_file(d.join("device-4")).unwrap();
    std::fs::rename(d.join("fleet.key"), d.join("kept")).unwrap();
    assert_eq!(provision(&d, "1"), Some(1), "a roster without its fleet key: a new key would split the fleet");
    std::fs::rename(d.join("kept"), d.join("fleet.key")).unwrap();
    std::fs::write(d.join("roster"), "4 x\nfour x\n").unwrap();
    assert_eq!(provision(&d, "1"), Some(1), "a roster that does not parse numbers nothing");
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), files - 1, "and writes nothing");
    for n in ["0", "x", "-1"] { assert_eq!(provision(&d, n), Some(2), "{n}"); }
    std::fs::remove_dir_all(&d).unwrap();
    let out = mud(&["provision"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("usage: mud gen") && String::from_utf8_lossy(&out.stderr).contains("mud provision <dir> [n]"));
}

#[test]
fn a_next_key_is_written_once_beside_its_fleet_and_a_unit_that_rotates_to_it_talks_to_one_booted_from_it() {
    let (d, none) = (fresh("next"), fresh("none"));
    assert_eq!(provision(&d, "2"), Some(0));
    let (key, roster) = (read(&d, "fleet.key"), read(&d, "roster"));
    assert_eq!(provision(&d, "--key"), Some(0));
    let next = read(&d, "fleet.key.next");
    assert!(next.starts_with("key ") && next.len() == 69 && next != key, "a new key in the fleet key's form");
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(d.join("fleet.key.next")).unwrap().permissions().mode() & 0o777, 0o600); }
    assert_eq!(provision(&d, "--key"), Some(1), "a next key is never replaced");
    assert_eq!((read(&d, "fleet.key"), read(&d, "roster"), read(&d, "fleet.key.next"), std::fs::read_dir(&d).unwrap().count()), (key, roster, next.clone(), 5), "and nothing else is touched");
    let mut old = boot(&d, 2, &[1, 2]);
    std::fs::rename(d.join("fleet.key.next"), d.join("fleet.key")).unwrap(); // the playbook's last step, by hand
    let mut new = boot(&d, 1, &[1, 2]);
    old.sealer_mut().inner().stage(hex(&value(&next, "key")));
    old.sealer_mut().inner().rotate();
    calls(&mut new, &mut old);
    calls(&mut old, &mut new);
    assert_eq!(provision(&none, "--key"), Some(1), "no fleet.key, nothing to rotate from");
    assert!(!none.exists(), "and nothing written");
    assert_eq!(mud(&["provision", d.to_str().unwrap(), "--key", "x"]).status.code(), Some(2));
    std::fs::remove_dir_all(&d).unwrap();
}
