//! The product's UDP over loopback: what it refuses in a default build, a call answered through it, and what it drops
//! unread. `cargo test -p mudrpc-adapters --features internet --test udp` runs the build that may go public.
#![warn(clippy::pedantic, clippy::nursery)]
use mudrpc_adapters::harness::P;
use mudrpc_adapters::transport::{pump, Rx, Transport};
use mudrpc_adapters::udp::Udp;
use mudrpc_core::rpc::Incoming;
use mudrpc_core::seal::Key;
use std::net::{SocketAddr, UdpSocket};

const KEY: [u8; 32] = [7; 32];

fn addr(s: &str) -> SocketAddr { s.parse().unwrap() }

/// What `bind` makes of two loopback ports chosen free, chosen again while another test takes one between the choosing
/// and the binding.
fn ports<T>(bind: impl Fn(SocketAddr, SocketAddr) -> std::io::Result<T>) -> T {
    for _ in 0..50 {
        let (a, b) = { let (x, y) = (UdpSocket::bind("127.0.0.1:0").unwrap(), UdpSocket::bind("127.0.0.1:0").unwrap()); (x.local_addr().unwrap(), y.local_addr().unwrap()) }; // both let go here
        match bind(a, b) { Ok(t) => return t, Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {} Err(e) => panic!("{e}") }
    }
    panic!("no two free loopback ports in fifty tries")
}

fn wait_for(u: &mut Udp) -> Option<(String, Vec<u8>)> {
    for _ in 0..200 {
        if let Some(Rx::Message { topic, wire }) = u.poll() { return Some((topic.to_string(), wire.to_vec())); }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    None
}

#[test]
#[cfg(not(feature = "internet"))]
fn a_default_build_binds_and_sends_to_private_addresses_alone() {
    for public in ["8.8.8.8:5000", "0.0.0.0:0", "[::]:0", "[2001:db8::1]:5000", "100.64.0.1:5000"] {
        let e = Udp::new(addr(public), &[addr("127.0.0.1:9")]).err().unwrap_or_else(|| panic!("{public} bound"));
        assert!(e.to_string().contains("the cargo feature `internet`"), "{public}: {e}");
        assert!(Udp::new(addr("127.0.0.1:0"), &[addr(public)]).is_err(), "a public peer, {public}");
    }
    assert!(Udp::new(addr("127.0.0.1:0"), &[]).is_err(), "no peer listed: a socket that hears no one");
    for private in ["127.0.0.1:0", "[::1]:0"] { assert!(Udp::new(addr(private), &[addr("10.1.2.3:5000"), addr("192.168.1.9:5000"), addr("172.16.0.1:5000"), addr("169.254.0.1:5000"), addr("[fd00::1]:5000"), addr("[fe80::1]:5000")]).is_ok(), "{private}"); }
}

#[test]
#[cfg(feature = "internet")]
fn a_build_with_internet_binds_every_interface_and_says_so() {
    assert!(Udp::new(addr("0.0.0.0:0"), &[addr("8.8.8.8:5000")]).is_ok());
    let v4 = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut both = Udp::new(addr("[::]:0"), &[v4.local_addr().unwrap()]).unwrap();
    both.listen("nav").unwrap();
    let port = both.local_addr().unwrap().port();
    v4.send_to(b"\x03navwire", ("127.0.0.1", port)).unwrap();
    assert_eq!(wait_for(&mut both), Some(("nav".to_string(), b"wire".to_vec())), "an IPv4 peer heard on a dual-stack socket");
    both.send("nav", b"back", 1).unwrap();
}

#[test]
fn a_call_is_answered_through_it() {
    let (mut gu, mut du) = ports(|ga, da| Ok((Udp::new(ga, &[da])?, Udp::new(da, &[ga])?)));
    let (mut gcs, mut drone) = (P::new("gcs", Key::new(KEY), 1, 1).unwrap(), P::new("drone", Key::new(KEY), 2, 1).unwrap());
    drone.listen("nav").unwrap();
    let id = gcs.call(0, "nav", "Arm", b"now", 30).unwrap();
    let mut answer = None;
    for _ in 0..400 {
        pump(&mut gcs, &mut gu, 0, |_, i| if let Incoming::Reply { id: r, bytes, .. } = i { answer = Some((r, bytes.to_vec())); }).unwrap();
        pump(&mut drone, &mut du, 0, |d, i| if let Incoming::Call(c) = i { d.reply(0, &c, 0, 0, "", b"armed", true).unwrap(); }).unwrap();
        if answer.is_some() { break; }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(answer, Some((id, b"armed".to_vec())));
}

#[test]
fn junk_strangers_and_unlistened_topics_are_dropped_unread_and_one_poll_reads_past_them() {
    let (mut u, listed, a) = ports(|a, b| Ok((Udp::new(a, &[b])?, UdpSocket::bind(b)?, a)));
    u.listen("nav").unwrap();
    listed.send_to(b"\x05otherwire", a).unwrap(); // a topic this radio does not listen to
    let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
    stranger.send_to(b"\x03navwire", a).unwrap();
    let mut long = vec![1u8; 577];
    long[1] = b'x';
    for junk in [&b""[..], b"\x00x", b"\x09nav", b"\x41aaaa", b"\x03\xff\xfe\xfdwire", &[0x02; 600], &long] { listed.send_to(junk, a).unwrap(); }
    for _ in 0..200 { listed.send_to(b"\x05", a).unwrap(); }
    listed.send_to(b"\x03navwire", a).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut nones = 0;
    let got = loop {
        match u.poll() { Some(Rx::Message { topic, wire }) => break (topic.to_string(), wire.to_vec()), Some(_) => {}, None => nones += 1 }
        assert!(nones < 10, "the honest datagram never came");
    };
    assert_eq!(got, ("nav".to_string(), b"wire".to_vec()));
    assert_eq!(nones, 0, "one poll reads past 209 bad datagrams, a 575-byte wire and a topic not listened to among them");
    assert!(u.poll().is_none(), "the stranger's datagram was never read as a message");
}

#[test]
fn one_dead_peer_stalls_nothing_and_silence_keeps_the_wire_at_the_sender() {
    let dead = addr("127.0.0.1:9");
    let (mut u, mut v) = ports(|a, b| Ok((Udp::new(a, &[dead, b])?, Udp::new(b, &[a])?)));
    v.listen("nav").unwrap();
    u.send("nav", b"one", 10).unwrap();
    assert_eq!(wait_for(&mut v), Some(("nav".to_string(), b"one".to_vec())));
    u.silent(true);
    assert!(u.send("nav", b"two", 10).is_err(), "silent: the pump keeps the item queued");
    u.silent(false);
    u.send("nav", b"two", 10).unwrap();
    assert_eq!(wait_for(&mut v), Some(("nav".to_string(), b"two".to_vec())));
    for (topic, wire) in [("", &b"w"[..]), (&"t".repeat(65)[..], b"w"), ("nav", b""), ("nav", &[0u8; 513])] { assert!(u.send(topic, wire, 1).is_err(), "{} bytes of topic, {} of wire", topic.len(), wire.len()); }
}

/// The feature takes two keys: set alone, as any crate in a build could set it, it fails the build; with the builder's
/// environment beside it, it builds and says so. End to end: it runs cargo.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_feature_alone_fails_the_build_and_the_builders_environment_beside_it_builds_it() {
    let build = |env: Option<&str>| {
        let mut c = std::process::Command::new(env!("CARGO"));
        c.args(["build", "--locked", "-p", "mudrpc-adapters", "--features", "internet"]).current_dir(env!("CARGO_MANIFEST_DIR")).env_remove("MUDRPC_INTERNET");
        if let Some(v) = env { c.env("MUDRPC_INTERNET", v); }
        let o = c.output().unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned())
    };
    for env in [None, Some("yes"), Some("1")] {
        let (ok, err) = build(env);
        assert!(!ok && err.contains("has not set MUDRPC_INTERNET") && !err.contains("listen-on-public-addresses"), "{env:?}: {err}");
    }
    let (ok, err) = build(Some("listen-on-public-addresses"));
    assert!(ok && err.contains("its UDP transport may listen on a public address"), "{err}");
}
