//! The C ABI, exercised from Rust (every build) and from the C example in SKILL.md (end to end).
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::cast_possible_truncation, // test values are small literals and loop indices
    clippy::cast_sign_loss, // as above
)]
use mudrpc::*;
use std::ffi::c_void;
use std::ptr;

const fn b(s: &[u8]) -> mud_buf { mud_buf { ptr: s.as_ptr(), len: s.len() } }
const fn m(s: &mut [u8]) -> mud_mutbuf { mud_mutbuf { ptr: s.as_mut_ptr(), len: s.len() } }

struct Block(Vec<u64>);
impl Block {
    fn new() -> Self { Self(vec![0u64; mud_size() / 8 + 1]) }
    fn hub() -> Self { Self(vec![0u64; mud_hub_size() / 8 + 1]) }
    const fn mem(&mut self) -> *mut c_void { self.0.as_mut_ptr().cast::<c_void>() }
    const fn len(&self) -> usize { self.0.len() * 8 }
}

/// 32 bytes counting up from `start`: never 32 equal ones, which the ABI refuses.
fn k(start: u8) -> [u8; 32] { core::array::from_fn(|i| start.wrapping_add(i as u8)) }

/// An unsigned ChaCha20-Poly1305 leaf, the shape most tests need.
fn peer(name: &str, sender: u32) -> Block { under(MUD_CIPHER_CHACHA20_POLY1305, name, sender, &[], 0) }

const fn blank() -> mud_incoming {
    mud_incoming { kind: 0, id: mud_id { bytes: [0; 12] }, topic: b(&[]), method: b(&[]), reply_to: b(&[]), detail: b(&[]), bytes: b(&[]), ttl_secs: 0, seq: 0, status: 0, end: 0, dropped: 0, sender: 0 }
}

#[test]
fn new_rejects_bad_memory_and_bad_config_without_crashing() {
    let key = k(1);
    let cfg = mud_config { name: b(b"gcs"), key: b(&key), sender: 1, epoch: 1, seed: b(&[]), budget: 0, hub: 0, cipher: 0 };
    let mut blk = Block::new();
    unsafe {
        let blank = mud_config { name: b(b"image"), key: b(&key), sender: 0, epoch: 1, seed: b(&[]), budget: 0, hub: 0, cipher: 0 };
        assert_eq!(mud_new(blk.mem(), mud_size(), &raw const blank), MUD_OK, "an unprovisioned peer exists");
        let mut id = mud_id { bytes: [0; 12] };
        assert_eq!(mud_call(blk.mem(), blk.len(), 0, b(b"nav"), b(b"M"), b(b""), 10, &raw mut id), MUD_EUNPROVISIONED, "but cannot send");
        assert_eq!(mud_resume(blk.mem(), mud_size(), 1), MUD_EEPOCH, "an epoch must climb");
        assert_eq!(mud_renew(blk.mem(), blk.len(), 2), MUD_OK);
        assert_eq!(mud_new(ptr::null_mut(), mud_size(), &raw const cfg), MUD_EMEM);
        assert_eq!(mud_new(blk.mem(), mud_size() - 1, &raw const cfg), MUD_EMEM);
        assert_eq!(mud_new(blk.mem().cast::<u8>().add(1).cast::<c_void>(), mud_size(), &raw const cfg), MUD_EMEM, "misaligned");
        assert_eq!(mud_new(blk.mem(), mud_size(), ptr::null()), MUD_EINVAL);
        let mut raw = [0u64; size_of::<mud_config>() / 8 + 1];
        let odd = raw.as_mut_ptr().cast::<mud_config>().byte_add(1);
        odd.write_unaligned(cfg);
        assert_eq!(mud_new(blk.mem(), mud_size(), odd), MUD_EINVAL, "a config misaligned for its type is refused, never read");
        let bad = mud_config { name: b(&[0xFF, 0xFE]), key: b(&key), sender: 1, epoch: 1, seed: b(&[]), budget: 0, hub: 0, cipher: 0 };
        assert_eq!(mud_new(blk.mem(), mud_size(), &raw const bad), MUD_EINVAL, "name must be UTF-8");
        let nokey = mud_config { name: b(b"gcs"), key: mud_buf { ptr: ptr::null(), len: 32 }, sender: 1, epoch: 1, seed: b(&[]), budget: 0, hub: 0, cipher: 0 };
        assert_eq!(mud_new(blk.mem(), mud_size(), &raw const nokey), MUD_EINVAL);
        let long = [b'x'; 65];
        let toolong = mud_config { name: b(&long), key: b(&key), sender: 1, epoch: 1, seed: b(&[]), budget: 0, hub: 0, cipher: 0 };
        assert_eq!(mud_new(blk.mem(), mud_size(), &raw const toolong), MUD_ETOOLARGE);
        assert_eq!(mud_call(ptr::null_mut(), mud_size(), 0, b(b"nav"), b(b"M"), b(b""), 10, ptr::null_mut()), MUD_EINVAL);
        assert_eq!(mud_sent(ptr::null_mut(), mud_size()), MUD_EINVAL);
        assert_eq!(mud_pressure(ptr::null_mut(), mud_size()), 0);
    }
    assert!(mud_size() > 512 * 16);
}

#[test]
fn call_and_reply_round_trip_through_the_abi() {
    let (mut gcs, mut drone) = (peer("gcs", 2), peer("drone", 3));
    let mut scratch = vec![0u8; 64 + 512];
    let mut plain = vec![0u8; 512];
    unsafe {
        assert_eq!(mud_listen(drone.mem(), drone.len(), b(b"nav")), MUD_OK);
        let mut id = mud_id { bytes: [0; 12] };
        assert_eq!(mud_call(gcs.mem(), gcs.len(), 0, b(b"nav"), b(b"Arm"), b(b"\x08\x01"), 30, &raw mut id), MUD_OK);
        assert_eq!(id.bytes[..8], [2, 0, 0, 0, 1, 0, 0, 0], "the id is the nonce: sender 2, epoch 1, counter");
        let mut o = mud_out { kind: 0, topic: b(&[]), wire: b(&[]), ttl_secs: 0 };
        assert_eq!(mud_outgoing(gcs.mem(), gcs.len(), 0, m(&mut scratch), &raw mut o), MUD_LISTEN, "its own inbox first");
        assert_eq!(mud_sent(gcs.mem(), gcs.len()), MUD_OK);
        assert_eq!(mud_outgoing(gcs.mem(), gcs.len(), 0, m(&mut scratch), &raw mut o), MUD_SEND);
        assert_eq!(std::slice::from_raw_parts(o.topic.ptr, o.topic.len), b"nav");
        assert_eq!(o.ttl_secs, 30);
        let mut inc = blank();
        assert_eq!(mud_deliver(drone.mem(), drone.len(), 1, o.topic, o.wire, m(&mut plain), &raw mut inc), MUD_CALL);
        assert_eq!(std::slice::from_raw_parts(inc.method.ptr, inc.method.len), b"Arm");
        assert_eq!(std::slice::from_raw_parts(inc.reply_to.ptr, inc.reply_to.len), b"gcs");
        assert_eq!(std::slice::from_raw_parts(inc.bytes.ptr, inc.bytes.len), b"\x08\x01");
        assert_eq!((inc.ttl_secs, inc.seq, inc.end), (30, 0, 1));
        assert_eq!(mud_sent(gcs.mem(), gcs.len()), MUD_OK);
        let queued = mud_queued(drone.mem(), drone.len());
        assert_eq!((mud_reply(drone.mem(), drone.len(), 1, &raw const inc, 0, 7, b(&[0xff, 0xfe]), b(b"armed"), 1), mud_queued(drone.mem(), drone.len())), (MUD_EINVAL, queued), "a detail is UTF-8, and nothing is kept of the refusal");
        assert_eq!(mud_reply(drone.mem(), drone.len(), 1, &raw const inc, 0, 7, b(b"not armed"), b(b"armed"), 1), MUD_OK);
        let mut dscratch = vec![0u8; 64 + 512];
        while mud_outgoing(drone.mem(), drone.len(), 1, m(&mut dscratch), &raw mut o) == MUD_LISTEN { mud_sent(drone.mem(), drone.len()); }
        assert_eq!(o.kind, MUD_SEND);
        let mut r = blank();
        assert_eq!(mud_deliver(gcs.mem(), gcs.len(), 2, o.topic, o.wire, m(&mut plain), &raw mut r), MUD_REPLY);
        assert_eq!((r.status, r.seq, r.end), (7, 0, 1));
        assert_eq!(std::slice::from_raw_parts(r.bytes.ptr, r.bytes.len), b"armed");
        assert_eq!(std::slice::from_raw_parts(r.detail.ptr, r.detail.len), b"not armed");
        assert_eq!(r.id.bytes, id.bytes);
        assert_eq!(mud_deliver(gcs.mem(), gcs.len(), 2, o.topic, o.wire, m(&mut plain), &raw mut r), MUD_DROPPED);
        assert_eq!(r.dropped, 5, "the call is resolved; nothing to land on");
        let mut ex = blank();
        assert_eq!(mud_tick(gcs.mem(), gcs.len(), 1000, &raw mut ex), MUD_NONE, "resolved: nothing to expire");
        let mut other = blank();
        let junk = [1u8; 40];
        assert_eq!(mud_deliver(gcs.mem(), gcs.len(), 2, b(b"nav"), b(&junk), m(&mut plain), &raw mut other), MUD_DROPPED);
        assert_eq!((other.dropped, other.status), (2, 2), "open failed on the tag");
        let mut small = vec![0u8; 100];
        assert_eq!(mud_deliver(gcs.mem(), gcs.len(), 2, b(b"nav"), b(&junk), m(&mut small), &raw mut other), MUD_EINVAL, "scratch must hold MUD_MSG");
        assert_eq!(mud_outgoing(gcs.mem(), gcs.len(), 2, m(&mut small), &raw mut o), MUD_EINVAL, "scratch must hold MUD_MAX_TOPIC + MUD_MSG");
        assert_eq!(mud_reply(drone.mem(), drone.len(), 1, &raw const other, 0, 0, b(b""), b(b""), 1), MUD_EINVAL, "only a MUD_CALL can be answered");
        assert_eq!(mud_pressure(gcs.mem(), gcs.len()), 0);
    }
}

#[test]
fn a_stream_through_the_abi_and_a_tampered_block_on_resume() {
    let (mut gcs, mut drone) = (peer("gcs", 2), peer("drone", 3));
    let (mut scratch, mut plain) = (vec![0u8; 64 + 512], vec![0u8; 512]);
    unsafe {
        mud_listen(drone.mem(), drone.len(), b(b"cam"));
        let mut id = mud_id { bytes: [0; 12] };
        assert_eq!(mud_call(gcs.mem(), gcs.len(), 0, b(b"cam"), b(b"Frames"), b(b""), 60, &raw mut id), MUD_OK);
        let mut o = mud_out { kind: 0, topic: b(&[]), wire: b(&[]), ttl_secs: 0 };
        while mud_outgoing(gcs.mem(), gcs.len(), 0, m(&mut scratch), &raw mut o) == MUD_LISTEN { mud_sent(gcs.mem(), gcs.len()); }
        let mut inc = blank();
        assert_eq!(mud_deliver(drone.mem(), drone.len(), 1, o.topic, o.wire, m(&mut plain), &raw mut inc), MUD_CALL);
        mud_sent(gcs.mem(), gcs.len());
        for seq in 0..3u32 { assert_eq!(mud_reply(drone.mem(), drone.len(), 1, &raw const inc, seq, 0, b(b""), b(&[seq as u8]), i32::from(seq == 2)), MUD_OK); }
        let mut got = Vec::new();
        let mut ds = vec![0u8; 64 + 512];
        loop {
            let k = mud_outgoing(drone.mem(), drone.len(), 1, m(&mut ds), &raw mut o);
            if k == MUD_NONE { break; }
            if k == MUD_SEND {
                let mut r = blank();
                assert_eq!(mud_deliver(gcs.mem(), gcs.len(), 2, o.topic, o.wire, m(&mut plain), &raw mut r), MUD_REPLY);
                got.push((r.seq, r.end, std::slice::from_raw_parts(r.bytes.ptr, r.bytes.len).to_vec()));
            }
            mud_sent(drone.mem(), drone.len());
        }
        assert_eq!(got, [(0, 0, vec![0]), (1, 0, vec![1]), (2, 1, vec![2])]);
        let mut ex = blank();
        assert_eq!(mud_tick(gcs.mem(), gcs.len(), 1000, &raw mut ex), MUD_NONE, "complete");
        let saved = std::slice::from_raw_parts(gcs.mem() as *const u8, mud_size()).to_vec();
        let mut copy = Block::new();
        std::ptr::copy_nonoverlapping(saved.as_ptr(), copy.mem().cast::<u8>(), mud_size());
        assert_eq!(mud_resume(copy.mem(), mud_size(), 2), MUD_OK);
        let mut tampered = saved;
        tampered[0] ^= 0x40;
        std::ptr::copy_nonoverlapping(tampered.as_ptr(), copy.mem().cast::<u8>(), mud_size());
        assert_eq!(mud_resume(copy.mem(), mud_size(), 2), MUD_ECORRUPT, "a block with the wrong magic is refused");
        let garbage = vec![0xFFu8; mud_size()];
        std::ptr::copy_nonoverlapping(garbage.as_ptr(), copy.mem().cast::<u8>(), mud_size());
        assert_eq!(mud_resume(copy.mem(), mud_size(), 2), MUD_ECORRUPT);
        assert_eq!(mud_resume(gcs.mem(), mud_size(), 1), MUD_EEPOCH, "the same epoch would replay nonces");
        let next = k(9);
        assert_eq!(mud_stage(gcs.mem(), gcs.len(), b(&next)), MUD_OK);
        assert_eq!(mud_rotate(gcs.mem(), gcs.len()), MUD_OK);
        assert_eq!(mud_opened(gcs.mem(), gcs.len()), 0);
        assert_eq!(mud_retire(gcs.mem(), gcs.len()), MUD_OK);
    }
}

#[test]
fn full_too_large_and_rate_are_codes() {
    let mut gcs = peer("gcs", 2);
    let big = vec![0u8; 600];
    let (mut scratch, mut out) = ([0u8; 64 + 512], mud_out { kind: 0, topic: b(&[]), wire: b(&[]), ttl_secs: 0 });
    unsafe {
        assert_eq!(mud_publish(gcs.mem(), gcs.len(), 0, b(b"t"), b(&big), 10), MUD_ETOOLARGE);
        let mut n = 0;
        while mud_publish(gcs.mem(), gcs.len(), 0, b(b"t"), b(b"x"), 10) == MUD_OK { n += 1; assert!(n < 100); }
        assert_eq!(n, 15, "16 outbox slots, one holds the inbox listen");
        assert_eq!(mud_publish(gcs.mem(), gcs.len(), 0, b(b"t"), b(b"x"), 10), MUD_EFULL);
        let mut drain = |g: &mut Block| while mud_outgoing(g.mem(), g.len(), 0, m(&mut scratch), &raw mut out) > 0 { mud_sent(g.mem(), g.len()); };
        drain(&mut gcs);
        for _ in n..32 { assert_eq!(mud_publish(gcs.mem(), gcs.len(), 0, b(b"t"), b(b"x"), 10), MUD_OK); drain(&mut gcs); }
        assert_eq!(mud_publish(gcs.mem(), gcs.len(), 0, b(b"t"), b(b"x"), 10), MUD_ERATE, "the 33rd: nothing is full");
        assert_eq!(mud_publish(gcs.mem(), gcs.len(), 0, b(b"t"), b(b"x"), 10), MUD_ERATE, "the 34th as the 33rd");
        assert_eq!(mud_publish(gcs.mem(), gcs.len(), 1, b(b"t"), b(b"x"), 10), MUD_OK, "the next second starts fresh");
    }
}

/// The example in SKILL.md, compiled against the static library and run. End to end: needs a C compiler and a build.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_skill_example_compiles_and_runs() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let skill = std::fs::read_to_string(format!("{dir}/SKILL.md")).unwrap();
    let start = skill.find("```c\n").expect("a ```c block") + 5;
    let end = start + skill[start..].find("```").unwrap();
    let out = std::env::temp_dir().join("mudrpc-skill-c");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("example.c"), &skill[start..end]).unwrap();
    let status = std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(format!("{dir}/../..")).status().unwrap();
    assert!(status.success());
    let lib = format!("{dir}/../../target/debug/libmudrpc.a");
    let mut cc = std::process::Command::new("cc");
    cc.args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I", dir, out.join("example.c").to_str().unwrap(), &lib, "-o", out.join("example").to_str().unwrap()]);
    if cfg!(target_os = "linux") { cc.args(["-lpthread", "-ldl", "-lm"]); }
    let built = cc.output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let run = std::process::Command::new(out.join("example")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "drone: Arm\ngcs: status 0 armed\n");
    assert!(run.status.success());
}

/// The example in the Python skill, run against the shared library. End to end: needs python3 and a build.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_python_skill_example_runs() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let py = format!("{dir}/../python");
    let skill = std::fs::read_to_string(format!("{py}/SKILL.md")).unwrap();
    let start = skill.find("```python\n").expect("a ```python block") + 10;
    let end = start + skill[start..].find("```").unwrap();
    let out = std::env::temp_dir().join("mudrpc-skill-py");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("example.py"), &skill[start..end]).unwrap();
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(format!("{dir}/../..")).status().unwrap().success());
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    let boot = format!("import sys, runpy; sys.path.insert(0, {py:?}); runpy.run_path({:?}, run_name='__main__')", out.join("example.py"));
    let run = std::process::Command::new("python3").args(["-I", "-c", &boot])
        .env("MUDRPC_LIB", format!("{dir}/../../target/debug/libmudrpc.{ext}")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "drone: Arm\ngcs: status 0 armed\n", "{}", String::from_utf8_lossy(&run.stderr));
    assert!(run.status.success());

    let extra = r#"
import mud
key = bytes(range(32))
a = mud.Peer("gcs", key, 1, 1); b = mud.Peer("drone", key, 2, 1); b.listen("nav")
assert a.outgoing(0).kind == "listen"; a.sent()
cid = a.call(0, "nav", "Arm", b"", 30); o = a.outgoing(0); a.sent()
assert mud.CIPHER_DEFAULT == mud.CIPHER_AES_256_GCM and o.wire[0] == 3, "the default is AES, version 3"
saved = a.save(); assert len(saved) > 8192
inc = b.deliver(1, o.topic, o.wire); assert inc.kind == "call" and inc.method == "Arm" and inc.reply_to == "gcs"
assert b.deliver(1, o.topic, o.wire).dropped == "duplicate"
assert b.deliver(1, o.topic, b"\x00" * 40).dropped == "version"
assert b.deliver(1, o.topic, b"\x03" * 40).dropped == "open"
b.reply(1, inc, status=3, payload=b"no", detail="not armed")
while (r := b.outgoing(1)).kind == "listen": b.sent()
b.sent(); got = a.deliver(1, r.topic, r.wire); assert (got.kind, got.status, got.detail, got.bytes) == ("reply", 3, "not armed", b"no"), got
restored = mud.Peer.restore(saved, 2)
ex = restored.tick(2); assert ex.kind == "expired" and ex.id == cid, ex
assert restored.deliver(2, r.topic, r.wire).dropped == "no_pending", "a late reply after a restore finds no call"
restored.close()
try:
    restored.tick(3); raise SystemExit("used after close")
except mud.MudError as e:
    assert e.code == -1, e.code
try:
    mud.Peer.restore(saved[:-1], 2); raise SystemExit("short block accepted")
except mud.Corrupt: pass
try:
    mud.Peer.restore(saved, 1); raise SystemExit("same epoch accepted")
except mud.MudError as e:
    assert e.code == -6, e.code
try:
    mud.Peer("x", key, 0, 1).publish(0, "t", b"x", 1); raise SystemExit("sender 0 sent")
except mud.MudError as e:
    assert e.code == -7, e.code
a.stage(bytes(range(9, 41))); a.rotate(); assert a.opened == "current"; a.retire()
try:
    mud.Peer.restore(bytes(len(saved)), 2)
except mud.Corrupt: pass
try:
    a.publish(5, "t", b"x" * 600, 10); raise SystemExit("too large accepted")
except mud.TooLarge: pass
n = 0
try:
    while True: a.publish(5, "t", b"x", 10); n += 1
except mud.Full: pass
assert n == 16, n  # the outbox was drained; all 16 slots are free
while a.outgoing(6) is not None: a.sent()
for _ in range(32): a.publish(6, "t", b"x", 10); a.outgoing(6); a.sent()
try:
    a.publish(6, "t", b"x", 10); raise SystemExit("33rd send accepted")
except mud.MudError as e:
    assert e.code == -9, e.code  # rate: this second's sends are spent, nothing is full
counting = lambda s: bytes((s + i) & 0xFF for i in range(32))
hub = mud.Peer("gcs", key, 1, 1, seed=counting(101), hub=True); leaf = mud.Peer("drone", key, 2, 1, seed=counting(102))
for s in range(1, 17): leaf.roster(s, mud.Peer.public_key(counting(100 + s)))
try:
    leaf.roster(17, mud.Peer.public_key(counting(117))); raise SystemExit("17th line accepted")
except mud.Full: pass
for s in range(1, 41): hub.roster(s, mud.Peer.public_key(counting(100 + s)))
hs = hub.save(); assert len(hs) > len(saved), "a hub saves at its own size"
assert mud.Peer.restore(hs, 2).counts["managed"] == 40, "and restores at it"
def code(f):
    try: f()
    except mud.MudError as e: return e.code
c, ttls = mud.Peer("c", key, 3, 1), []; c.publish(0, "t", b"x", 0); c.publish(0, "t", b"x", 2**32 - 1)
for want, f in [(-1, lambda: mud.Peer("x", bytes(32), 1, 1)), (-1, lambda: mud.Peer("x", key, 1, 1, seed=bytes(32))), (-1, lambda: mud.Peer("x", key, 1, 1, hub=True)), (-4, lambda: mud.Peer.restore(bytes(1) + saved[1:], 2)),
                (-5, lambda: mud.Peer.restore(hs[:len(saved)], 2)), (-3, lambda: c.publish(0, "t" * 65, b"x", 10)), (-1, lambda: c.publish(0, "", b"x", 10)), (-1, lambda: mud.Peer("", key, 4, 1))]: assert code(f) == want, want
while (q := c.outgoing(0)) is not None: c.sent(); ttls += [q.ttl_secs] if q.kind == "send" else []
assert ttls == [1, 604800], ttls  # clamped, not refused
for _ in range(16): c.call(1, "nav", "M", b"", 10); c.outgoing(1); c.sent()
assert code(lambda: c.call(1, "nav", "M", b"", 10)) == -2 and (c.pending, c.queued) == (16, 0), "16 calls pending, counted"
nav = mud.load_proto("syntax = \"proto3\"; package nav; message Ack { bool ok = 1; repeated uint32 ids = 2; }")
ack = nav.Ack(ok=True, ids=[1, 300])
assert nav.Ack.decode(ack.encode()) == ack
try:
    mud.load_proto("syntax = \"proto3\"; message A {\n map<string,int32> m = 1; }"); raise SystemExit("bad proto loaded")
except mud.ProtoError as e:
    assert e.line == 2, e.line
print("ok")
"#;
    std::fs::write(out.join("extra.py"), extra).unwrap();
    let boot = format!("import sys, runpy; sys.path.insert(0, {py:?}); runpy.run_path({:?}, run_name='__main__')", out.join("extra.py"));
    let run = std::process::Command::new("python3").args(["-I", "-c", &boot])
        .env("MUDRPC_LIB", format!("{dir}/../../target/debug/libmudrpc.{ext}")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "ok\n", "{}", String::from_utf8_lossy(&run.stderr));
}

// ───────────────────────── the simulator and the generator over the ABI ─────────────────────────

#[test]
fn the_simulator_crosses_the_abi() {
    let text = b"seed 1\npeers 2\nprofile dtn\n0 partition 0 1\n3000 heal 0 1\n";
    let mut line = 0i32;
    unsafe {
        assert!(mud_sim_new(b(b"seed 1\npeers 2\nprofile dtn\n0 teleport 0"), &raw mut line).is_null());
        assert_eq!(line, 4, "a parse error names its line");
        let sim = mud_sim_new(b(text), &raw mut line);
        assert!(!sim.is_null());
        assert_eq!(mud_sim_peers(sim), 2);
        assert_eq!(mud_sim_last_event_ms(sim), 3000);
        assert_eq!(mud_sim_events(sim), 2);
        let mut ev = mud_event { at_ms: 0, kind: 0, a: 0, b: 0, text: b(&[]) };
        assert_eq!(mud_sim_event(sim, 1, &raw mut ev), MUD_OK);
        assert_eq!((ev.at_ms, ev.kind, ev.a, ev.b), (3000, 2, 0, 1), "heal 0 1");
        assert_eq!(mud_sim_event(sim, 2, &raw mut ev), MUD_EINVAL);
        assert_eq!(mud_sim_listen(sim, 1, b(b"t")), MUD_OK);
        assert_eq!(mud_sim_send(sim, 0, b(b"t"), b(b"hello"), 60), MUD_OK);
        assert_eq!(mud_sim_send(sim, 9, b(b"t"), b(b"hello"), 60), MUD_EINVAL, "no such radio");
        let mut scratch = vec![0u8; 64 + 512];
        let (mut topic, mut wire) = (b(&[]), b(&[]));
        for _ in 0..25 { mud_sim_step(sim, 100); }
        assert_eq!(mud_sim_poll(sim, 1, m(&mut scratch), &raw mut topic, &raw mut wire), MUD_NONE, "still partitioned");
        for _ in 0..10 { mud_sim_step(sim, 100); }
        assert_eq!(mud_sim_now_ms(sim), 3500);
        let mut got = Vec::new();
        loop {
            let k = mud_sim_poll(sim, 1, m(&mut scratch), &raw mut topic, &raw mut wire);
            if k == MUD_NONE { break; }
            if k == 1 { got.push((std::slice::from_raw_parts(topic.ptr, topic.len).to_vec(), std::slice::from_raw_parts(wire.ptr, wire.len).to_vec())); }
        }
        assert_eq!(got, [(b"t".to_vec(), b"hello".to_vec())]);
        mud_sim_silent(sim, 0, 1);
        mud_sim_note(sim, b(b"a note from the host"));
        let need = mud_sim_log(sim, m(&mut []));
        assert!(need > 0);
        let mut out = vec![0u8; need as usize];
        assert_eq!(mud_sim_log(sim, m(&mut out)), need);
        let log = String::from_utf8(out).unwrap();
        assert!(log.contains("a note from the host") && log.contains("event Heal(0, 1)"), "{log}");
        mud_sim_free(sim);
        mud_sim_free(ptr::null_mut());
    }
}

#[test]
fn the_generator_crosses_the_abi() {
    let proto = b"syntax = \"proto3\";\npackage nav;\nmessage Ack { bool ok = 1; }\n";
    let mut out = vec![0u8; 65_536];
    let mut line = 0i32;
    unsafe {
        let n = mud_gen(b(b"python"), b(proto), m(&mut out), &raw mut line);
        assert!(n > 0, "{n} line {line}");
        let code = std::str::from_utf8(&out[..n as usize]).unwrap();
        assert!(code.contains("class Ack:"), "{code}");
        assert_eq!(mud_gen(b(b"python"), b(b"syntax = \"proto3\";\nmessage A {\n map<string, int32> m = 1; }"), m(&mut out), &raw mut line), MUD_EINVAL);
        assert_eq!(line, 3);
        assert_eq!(mud_gen(b(b"cobol"), b(proto), m(&mut out), &raw mut line), MUD_EINVAL);
        assert_eq!(mud_gen(b(b"c"), b(proto), m(&mut [0u8; 8]), &raw mut line), MUD_ETOOLARGE, "the buffer must hold the output");
        assert!(mud_gen(b(b"rust"), b(proto), m(&mut out), &raw mut line) > 0);
    }
}

/// The .NET harness, built once and run on every scenario. End to end: needs the dotnet SDK and a build.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_dotnet_harness_matches_the_checked_in_logs() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let out = std::env::temp_dir().join("mudrpc-harness-dotnet");
    let built = std::process::Command::new("dotnet").args(["build", "-c", "Release", "-nologo", "-v", "q", "-o", out.to_str().unwrap()]).current_dir(format!("{dir}/tests/dotnet")).output().unwrap();
    assert!(built.status.success(), "{}{}", String::from_utf8_lossy(&built.stdout), String::from_utf8_lossy(&built.stderr));
    for (name, flags) in modes() {
        let want = expected(&root, name, flags);
        let run = std::process::Command::new("dotnet").arg(out.join("harness.dll")).arg(format!("{root}/scenarios/{name}.scenario")).args(flags).env("MUDRPC_LIB", shared_lib(&root)).output().unwrap();
        assert!(run.status.success(), ".NET harness on {name} {flags:?}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, ".NET log for {name} {flags:?} differs from the Rust log");
    }
}

/// The gRPC drop-in: Grpc.Tools' client and service base for `grpc-dotnet/drone.proto`, unchanged, over two peers, then a
/// native server. Every gRPC shape the drop-in claims, and every one it refuses, by its status. End to end: the dotnet SDK and its package feed.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_dotnet_grpc_drop_in_runs_the_code_grpc_tools_generates() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let out = std::env::temp_dir().join("mudrpc-grpc-dotnet");
    let built = std::process::Command::new("dotnet").args(["build", "-c", "Release", "-nologo", "-v", "q", "-o", out.to_str().unwrap()]).current_dir(format!("{dir}/tests/grpc-dotnet")).output().unwrap();
    assert!(built.status.success(), "{}{}", String::from_utf8_lossy(&built.stdout), String::from_utf8_lossy(&built.stderr));
    let run = std::process::Command::new("dotnet").arg(out.join("grpcdrop.dll")).env("MUDRPC_LIB", shared_lib(&root)).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "\
arm: armed True id 7 by mudrpc:1
blocking: armed True
refused: FailedPrecondition battery low
handler throws: Unknown Exception was thrown by handler.
unknown method: Unimplemented no method test.Drone/Fly
a method name with a lone surrogate: InvalidArgument mudRPC: invalid argument
a service name of 65 bytes: InvalidArgument mudRPC: invalid argument
telemetry reversed: 0 1 2 3 4
telemetry empty: 
telemetry fails after: 0 1
telemetry fails: Aborted lost the camera
upload: Unimplemented mudRPC carries no client stream
too large: ResourceExhausted mudRPC: too large
deadline: DeadlineExceeded Deadline Exceeded
deadline in time: True
cancelled: Cancelled Call canceled by the client.
status before the end: InvalidOperationException, trailers InvalidOperationException
unseen: DeadlineExceeded Deadline Exceeded
reading cancelled: Cancelled Call canceled by the client.
after the cut: armed True
local deadline: InvalidOperationException
status set: PermissionDenied not yours
undecodable request: Internal Failed to deserialize request message.
headers after the reply: 0, status OK
cancelled before sending, 16 times: pending 0 more, then armed True
cancel racing a reply: 30 of 30 finished
saved while calling: 50 calls answered, saves made
two full streams back to back: 63 63
a 64th item: ResourceExhausted a stream carries at most 63 items
an item past the wire mid-stream: 0 1, ResourceExhausted the reply is larger than a mudRPC wire, its handler stopped: True
the peer disposed first: Unavailable the channel is closed
unknown method, every free slot at once: all Unimplemented
null detail: Internal 
a null wire: ArgumentNullException
a deadline eight days out: ttl 604800
fifty failed calls, unobserved exceptions: 0
five thousand junk wires: then armed True
a transport refusing a listen: armed True, faults heard yes
disposed from its own send: returns
cancelled with items unread: Cancelled Call canceled by the client.
a slow reader past its deadline: DeadlineExceeded Deadline Exceeded
the slow reader's deadline in time: True
closed with items unread: Unavailable the channel is closed
over UDP: armed True, telemetry 0 1 2 3 4
its socket disposed: Unavailable the channel is closed
a public bind: MudException, a public peer: MudException, this library public: False
three on UDP, one serving nothing: 10 of 10 armed
native stream: 2 1 0
native refusal: FailedPrecondition native says no
a caller behind 48 owed a stream each: armed True
");
}

/// The gRPC drop-in in Python: grpcio-tools' stub and servicer for `grpc-python/drone.proto`, unchanged, over two peers,
/// over UDP and against a native server. Its packages come from the Python package index into `target/pygrpc`, pinned by hash in
/// `grpc-python/requirements.txt`. End to end: Python 3, pip and the network.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_python_drop_in_runs_the_code_grpcio_tools_generates() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let venv = std::path::Path::new(&root).join("target/pygrpc");
    let python = venv.join("bin/python");
    if !python.exists() { assert!(std::process::Command::new("python3").args(["-m", "venv"]).arg(&venv).status().unwrap().success()); }
    let installed = std::process::Command::new(&python).args(["-I", "-m", "pip", "install", "-q", "--disable-pip-version-check", "--require-hashes", "--only-binary=:all:", "-r", &format!("{dir}/tests/grpc-python/requirements.txt")]).output().unwrap();
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
    let out = std::env::temp_dir().join("mudrpc-grpc-python");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let generated = std::process::Command::new(&python).args(["-I", "-m", "grpc_tools.protoc", "-I", &format!("{dir}/tests/grpc-python")]).arg(format!("--python_out={}", out.display())).arg(format!("--grpc_python_out={}", out.display())).arg("drone.proto").output().unwrap();
    assert!(generated.status.success(), "{}", String::from_utf8_lossy(&generated.stderr));
    let script = format!("import sys, runpy; sys.path[:0] = [{:?}, {:?}, {:?}]; runpy.run_path({:?}, run_name='__main__')", format!("{dir}/../python"), out.display().to_string(), format!("{dir}/tests/grpc-python"), format!("{dir}/tests/grpc-python/main.py"));
    let run = std::process::Command::new(&python).args(["-I", "-c", &script]).env("MUDRPC_LIB", shared_lib(&root)).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "\
arm: armed True id 7 by mudrpc:1
with_call: armed True, code OK
future: armed True
refused: FAILED_PRECONDITION battery low
handler throws: UNKNOWN Exception calling application: thirteen
unknown method: UNIMPLEMENTED no method test.Drone/Fly
a service name empty, past 64 bytes or not UTF-8, a method name not UTF-8: INVALID_ARGUMENT mudRPC: invalid argument
a method name empty or of 65 bytes, carried: UNIMPLEMENTED no method test.Drone/ / UNIMPLEMENTED no method test.Drone/yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy, then armed True
telemetry reversed: 0 1 2 3 4
telemetry empty: 
telemetry fails after: 0 1
telemetry fails: ABORTED lost the camera
upload: UNIMPLEMENTED mudRPC carries no client stream
too large: RESOURCE_EXHAUSTED mudRPC: too large
deadline: DEADLINE_EXCEEDED Deadline Exceeded
deadline in time: True
future cancelled: True True FutureCancelledError
past the deadline: DEADLINE_EXCEEDED Deadline Exceeded
after the cut: armed True
status set: PERMISSION_DENIED not yours
undecodable request: INTERNAL Exception deserializing request!
a reply past the wire: RESOURCE_EXHAUSTED the reply is larger than a mudRPC wire
a code with no details: INTERNAL 
cancel racing a reply: 30 of 30 finished
saved while calling: 50 calls answered, saves made
two full streams back to back: 63 63
a 64th item: RESOURCE_EXHAUSTED a stream carries at most 63 items
an item past the wire mid-stream: 0 1, RESOURCE_EXHAUSTED the reply is larger than a mudRPC wire, its handler stopped: True
the peer closed first: UNAVAILABLE the channel is closed
unknown method, every free slot at once: all UNIMPLEMENTED
a null wire: TypeError
a deadline eight days out: ttl 604800
five thousand junk wires: then armed True
cancelled with items unread: CANCELLED Locally cancelled by application!
a slow reader past its deadline: DEADLINE_EXCEEDED Deadline Exceeded
the slow reader's deadline in time: True
a transport refusing a listen: armed True, faults heard yes
closed from its own send: returns
over UDP: armed True, telemetry 0 1 2 3 4
its socket closed: UNAVAILABLE the channel is closed
a public bind: MudError, a public peer: MudError, this library public: False
a server that never answers: DEADLINE_EXCEEDED Deadline Exceeded
the server saw its ttl pass: True
a detail in bytes: FAILED_PRECONDITION battery low
then the next caller: armed True
a detail of 128 emoji: FAILED_PRECONDITION, 32 kept, 128 bytes
code() waits for the end: OK
an infinite timeout: armed True
a timeout of NaN: DEADLINE_EXCEEDED Deadline Exceeded
a service name past 64 bytes: ValueError
through an interceptor: armed True, intercepted 1
forty streams abandoned unread: let go True
closed from inside use(): returns
subscribed after close: READY
three on UDP, one serving nothing: 10 of 10 armed
a stop past its grace: running CANCELLED the server shut down, waiting CANCELLED the server shut down, the servicer heard it: True, stopped True
a call after the stop: DEADLINE_EXCEEDED Deadline Exceeded
native stream: 2 1 0
native refusal: FAILED_PRECONDITION native says no
a caller behind 48 owed a stream each: armed True
");
}

/// The gRPC drop-in for JavaScript and TypeScript: protoc-gen-es' descriptors for `grpc-js/drone.proto`, a Connect client and
/// service, unchanged, over two peers, over UDP and against a native server. Its packages come from npm into
/// `target/connect` by `npm ci`, pinned by `grpc-js/package-lock.json`. End to end: Node, npm and the network.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_connect_drop_in_runs_what_protoc_gen_es_generates() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "--release", "-p", "mudrpc-c", "--target", "wasm32-unknown-unknown"]).current_dir(&root).status().unwrap().success());
    let at = std::path::Path::new(&root).join("target/connect");
    std::fs::create_dir_all(&at).unwrap();
    for f in ["package.json", "package-lock.json"] { std::fs::copy(format!("{dir}/tests/grpc-js/{f}"), at.join(f)).unwrap(); }
    let installed = std::process::Command::new("npm").args(["ci", "--ignore-scripts", "--no-audit", "--no-fund"]).current_dir(&at).output().unwrap();
    assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
    let [(path, sha), _] = tools();
    let protoc = maven(&root, "", path, sha);
    assert!(std::process::Command::new("chmod").arg("+x").arg(&protoc).status().unwrap().success());
    let app = at.join("app");
    let _ = std::fs::remove_dir_all(&app);
    std::fs::create_dir_all(&app).unwrap();
    let generated = std::process::Command::new(&protoc).arg(format!("--plugin=protoc-gen-es={}", at.join("node_modules/.bin/protoc-gen-es").display())).arg(format!("--es_out={}", app.display())).arg("--es_opt=target=js,import_extension=js")
        .args(["-I", &format!("{dir}/tests/grpc-js"), "drone.proto"]).output().unwrap();
    assert!(generated.status.success(), "{}", String::from_utf8_lossy(&generated.stderr));
    for f in [format!("{dir}/../js/mud.js"), format!("{dir}/../js/mud-connect.js"), format!("{dir}/tests/grpc-js/main.js")] { std::fs::copy(&f, app.join(std::path::Path::new(&f).file_name().unwrap())).unwrap(); } // vendored beside the product's code, as a product copies them
    let run = std::process::Command::new("node").arg(app.join("main.js")).env("MUDRPC_WASM", format!("{root}/target/wasm32-unknown-unknown/release/mudrpc.wasm")).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "\
arm: armed true id 7 by mudrpc:1
refused: FailedPrecondition battery low
handler throws: Unknown thirteen
unknown method: Unimplemented no method test.Drone/Fly
telemetry reversed: 0 1 2 3 4
telemetry empty: 
telemetry fails after: 0 1
telemetry fails: Aborted lost the camera
upload: Unimplemented mudRPC carries no client stream
too large: ResourceExhausted mudRPC: too large
deadline: DeadlineExceeded the operation timed out
deadline in time: true
cancelled: Canceled This operation was aborted
past the deadline: DeadlineExceeded the operation timed out
after the cut: armed true
a reply past the wire: ResourceExhausted the reply is larger than a mudRPC wire
an item past the wire mid-stream: 0 1, ResourceExhausted the reply is larger than a mudRPC wire, its handler stopped: true
a method name that is not UTF-8: InvalidArgument mudRPC: invalid argument, then armed true
a service name of 65 bytes: InvalidArgument mudRPC: invalid argument
two full streams back to back: 63 63
a 64th item: ResourceExhausted a stream carries at most 63 items
the peer closed first: Unavailable the transport is closed
unknown method, every free slot at once: all Unimplemented
a null wire: TypeError
a deadline eight days out: ttl 604800
five thousand junk wires: then armed true
cancelled with items unread: Canceled This operation was aborted
a slow reader past its deadline: DeadlineExceeded the operation timed out
the slow reader's deadline in time: true
a transport refusing a listen: armed true, faults heard yes
closed from its own send: no error
over UDP: armed true, telemetry 0 1 2 3 4
its socket closed: Unavailable the transport is closed
a public bind: MudError, a public peer: MudError, this binding public: false
a server that never answers: DeadlineExceeded the operation timed out
the server saw its ttl pass: true
a service name past 64 bytes: ConnectError
a service name with a lone surrogate: ConnectError
three on UDP, one serving nothing: 10 of 10 armed
a throw nothing can print: Unknown the handler threw what cannot be told
an octal or scoped address: MudError, no error, a peer at port 0: MudError
a radio that is neither: TypeError
a timeout of null: armed true
a call right after its peer closed: Unavailable the transport is closed
a handler hears its server close: true
native stream: 2 1 0
native refusal: FailedPrecondition native says no
a reply that does not decode: Internal failed to deserialize response: premature EOF
a caller behind 48 owed a stream each: armed true
");
}

/// The gRPC drop-in for Swift: protoc-gen-swift's messages and protoc-gen-grpc-swift-2's client and service for
/// `grpc-swift/drone.proto`, unchanged, over two peers, over UDP and against a native server. The package builds in
/// `target/grpc-swift` from `grpc-swift/Package.swift`, its dependencies pinned to their commits by `Package.resolved`,
/// with `Mud.swift` and `MudGrpc.swift` vendored beside the product's code. End to end: Swift 6.1, git and the network.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_swift_drop_in_runs_what_protoc_gen_grpc_swift_generates() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let at = std::path::Path::new(&root).join("target/grpc-swift");
    let (c, app, example) = (at.join("Sources/CMudRPC"), at.join("Sources/App"), at.join("Sources/Example"));
    for d in [&app, &example] { let _ = std::fs::remove_dir_all(d); }
    for d in [&c, &app, &example] { std::fs::create_dir_all(d).unwrap(); }
    let skill = std::fs::read_to_string(format!("{dir}/../swift/SKILL.md")).unwrap();
    std::fs::write(example.join("main.swift"), skill.split("```swift\n").nth(1).unwrap().split("```").next().unwrap()).unwrap();
    std::fs::copy(format!("{dir}/../swift/Mud.swift"), example.join("Mud.swift")).unwrap();
    for f in ["Package.swift", "Package.resolved"] { std::fs::copy(format!("{dir}/tests/grpc-swift/{f}"), at.join(f)).unwrap(); }
    for f in ["mudrpc.h", "module.modulemap"] { std::fs::copy(format!("{dir}/{f}"), c.join(f)).unwrap(); }
    let swift = |args: &[&str]| { let o = std::process::Command::new("swift").args(args).current_dir(&at).output().unwrap(); assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)); };
    for p in ["protoc-gen-swift", "protoc-gen-grpc-swift-2"] { swift(&["build", "-c", "release", "--force-resolved-versions", "--product", p]); }
    let [(path, sha), _] = tools();
    let protoc = maven(&root, "", path, sha);
    assert!(std::process::Command::new("chmod").arg("+x").arg(&protoc).status().unwrap().success());
    let generated = std::process::Command::new(&protoc).args(["--plugin=protoc-gen-swift=.build/release/protoc-gen-swift", "--plugin=protoc-gen-grpc-swift-2=.build/release/protoc-gen-grpc-swift-2", "--swift_out=Sources/App", "--grpc-swift-2_out=Sources/App", "-I", &format!("{dir}/tests/grpc-swift"), "drone.proto"]).current_dir(&at).output().unwrap();
    assert!(generated.status.success(), "{}", String::from_utf8_lossy(&generated.stderr));
    for f in [format!("{dir}/../swift/Mud.swift"), format!("{dir}/../swift/MudGrpc.swift"), format!("{dir}/tests/grpc-swift/main.swift")] { std::fs::copy(&f, app.join(std::path::Path::new(&f).file_name().unwrap())).unwrap(); }
    let lib = format!("{root}/target/debug");
    swift(&["build", "--force-resolved-versions", "-Xlinker", &format!("-L{lib}"), "-Xlinker", "-rpath", "-Xlinker", &lib]);
    let example = std::process::Command::new(at.join(".build/debug/Example")).output().unwrap();
    assert!(example.status.success(), "{}", String::from_utf8_lossy(&example.stderr));
    assert_eq!(String::from_utf8_lossy(&example.stdout), "drone: Arm\ngcs: status 0 armed\n");
    let run = std::process::Command::new(at.join(".build/debug/App")).output().unwrap();
    assert!(run.status.success(), "{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "\
arm: armed true id 7 by mudrpc:1
refused: failedPrecondition battery low
handler throws: unknown 
unknown method: unimplemented Requested RPC isn't implemented by this server.
telemetry reversed: 0 1 2 3 4
telemetry empty: 
telemetry fails after: 0 1
telemetry fails: aborted lost the camera
upload: unimplemented mudRPC carries no client stream
too large: resourceExhausted mudRPC: too large
deadline: deadlineExceeded RPC timed out before completing
deadline in time: true
cancelled: cancelled the call was cancelled
past the deadline: deadlineExceeded RPC timed out before completing
after the cut: armed true
a reply past the wire: resourceExhausted the reply is larger than a mudRPC wire
a detail of 100 two-byte characters: failedPrecondition, 64 kept, 128 bytes
two full streams back to back: 63 63
a 64th item: resourceExhausted a stream carries at most 63 items
an item past the wire mid-stream: 0 1, resourceExhausted the reply is larger than a mudRPC wire, its handler stopped: true
a call to an empty service name: invalidArgument mudRPC: invalid argument
a call to a service name of 65 bytes: invalidArgument mudRPC: invalid argument
the peer closed first: unavailable the transport is closed
unknown method, every free slot at once: all unimplemented
a deadline eight days out: ttl 604800
five thousand junk wires: then armed true
cancelled with items unread: cancelled the call was cancelled
a slow reader past its deadline: deadlineExceeded RPC timed out before completing
the slow reader's deadline in time: true
an infinite timeout: armed true
a three-hour ttl reaches the server as: 10800S
a second server on one transport: transportError: a transport serves one server
a server that never answers: deadlineExceeded RPC timed out before completing
the server saw its ttl pass: true
a transport refusing a listen: armed true, faults heard yes
a radio calling back in from another thread: armed true
after sixteen calls timed out: deadlineExceeded RPC timed out before completing
a transport dropped unclosed is freed: true
runConnections after close: returned
closed from its own send: no error
over UDP: armed true, telemetry 0 1 2 3 4
its socket closed: unavailable the transport is closed
a public bind: MudError -1, a public peer: MudError -1, this library public: false
an octal address: MudError -1, no peer: MudError -1
a service name past 64 bytes: transportError: mudRPC carries a service's full name of 1 to 64 bytes: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
three on UDP, one serving nothing: 10 of 10 armed
a call when its transport closes: unavailable the transport is closed
a handler hears its server close: true
a shutdown past its deadline: cancelled the server shut down, the handler heard it: true
native stream: 2 1 0
native refusal: failedPrecondition native says no
a reply that does not decode: invalidArgument Can't deserialize to message of type Test_Sample.
a caller behind 48 owed a stream each: armed true
restored at a higher epoch: no error, at the same: MudError -6, a short copy: MudError -4
a closed peer: MudError -4, pending 0, a short key: MudError -1
every server returned from serve
");
}

/// The gRPC drop-in for C++: protoc's messages and protoc-gen-mud's Stub and Service for `grpc-cpp/drone.proto`, run as
/// gRPC C++ code is with `mud::` for `grpc::`, over two peers, over UDP and against a native peer; built from
/// `bindings/cpp` in `target/grpc-cpp` with the system's protobuf, found by pkg-config, and run plain and under the
/// thread, address and undefined-behaviour sanitizers, at once. End to end: a C++17 compiler,
/// pkg-config, protoc and libprotoc.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_cpp_drop_in_runs_what_protoc_gen_mud_generates() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let at = std::path::Path::new(&root).join("target/grpc-cpp");
    let _ = std::fs::remove_dir_all(&at);
    std::fs::create_dir_all(&at).unwrap();
    let run = |cmd: &str, args: &[&str]| {
        let o = std::process::Command::new(cmd).args(args).current_dir(&at).output().unwrap();
        assert!(o.status.success(), "{cmd} {args:?}\n{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    let flags = run("pkg-config", &["--cflags", "--libs", "protobuf"]);
    let protobuf: Vec<&str> = flags.split_whitespace().flat_map(|f| f.strip_prefix("-I").map_or([f, ""], |p| ["-isystem", p])).filter(|f| !f.is_empty()).collect();   // its headers' warnings are not ours
    let (cpp, lib, rpath) = (format!("{dir}/../cpp"), format!("{root}/target/debug"), format!("-Wl,-rpath,{root}/target/debug"));
    let ours = ["-std=c++17", "-g", "-O1", "-Wall", "-Wextra", "-Wpedantic", "-Werror", "-pthread", "-I.", "-I", &cpp, "-I", dir];
    run("c++", &[&["-std=c++17", "-O1", &format!("{cpp}/protoc-gen-mud.cc")], &protobuf[..], &["-lprotoc", "-o", "protoc-gen-mud"]].concat());
    run("protoc", &["--plugin=protoc-gen-mud=./protoc-gen-mud", "--cpp_out=.", "--mud_out=.", "-I", &format!("{dir}/tests/grpc-cpp"), "drone.proto"]);
    let skill = std::fs::read_to_string(format!("{cpp}/SKILL.md")).unwrap();
    std::fs::write(at.join("example.cc"), skill.split("```cpp\n").nth(1).unwrap().split("```").next().unwrap()).unwrap();
    run("c++", &[&ours[..], &["example.cc", "-L", &lib, "-lmudrpc", &rpath, "-o", "example"]].concat());
    assert_eq!(run("./example", &[]), "drone: Arm\ngcs: status 0 armed\n");
    run("c++", &[&["-std=c++17", "-g", "-O1", "-c", "drone.pb.cc"], &protobuf[..], &["-o", "drone.pb.o"]].concat());   // protoc's code, built as its own
    let builds = [("app", ""), ("tsan", "-fsanitize=thread"), ("asan", "-fsanitize=address,undefined")];
    for (name, sanitize) in builds {
        let sanitize: &[&str] = if sanitize.is_empty() { &[] } else { &[sanitize, "-fno-sanitize-recover=all", "-fno-omit-frame-pointer"] };
        run("c++", &[&ours[..], sanitize, &[&format!("{dir}/tests/grpc-cpp/main.cc"), "drone.pb.o", "-L", &lib, "-lmudrpc", &rpath], &protobuf[..], &["-o", name]].concat());
    }
    let mut running = Vec::new();   // all three started before any is waited on
    for (name, _) in builds { running.push(std::process::Command::new(at.join(name)).env("TSAN_OPTIONS", "halt_on_error=1").stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()); }
    for (child, (name, _)) in running.into_iter().zip(builds) {
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "{name}\n{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        assert!(!String::from_utf8_lossy(&o.stderr).contains("Sanitizer") && !String::from_utf8_lossy(&o.stderr).contains("runtime error"), "{name}\n{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(String::from_utf8_lossy(&o.stdout), "\
arm: armed true id 7 by mudrpc:1
refused: FAILED_PRECONDITION battery low
handler throws: UNKNOWN Unexpected error in RPC handling
unknown method: UNIMPLEMENTED 
a method name that is not UTF-8: INVALID_ARGUMENT mudRPC: invalid argument, a service name of 65 bytes: INVALID_ARGUMENT mudRPC: invalid argument, then armed true
a method the service leaves out: UNIMPLEMENTED 
telemetry reversed: 0 1 2 3 4
telemetry empty: 
telemetry fails after: 0 1
telemetry fails: ABORTED lost the camera
upload: wrote false, UNIMPLEMENTED mudRPC carries no client stream
chat: read false, UNIMPLEMENTED mudRPC carries no client stream
too large: RESOURCE_EXHAUSTED mudRPC: too large
deadline: DEADLINE_EXCEEDED Deadline Exceeded
deadline in time: true
cancelled: CANCELLED Cancelled
cancelled before it starts: CANCELLED Cancelled
past the deadline: DEADLINE_EXCEEDED Deadline Exceeded
after the cut: armed true
a reply past the wire: RESOURCE_EXHAUSTED the reply is larger than a mudRPC wire
a detail of 100 two-byte characters: FAILED_PRECONDITION, 64 kept, 128 bytes
a detail that is not UTF-8: FAILED_PRECONDITION bad  byte
two full streams back to back: 63 63
a 64th item: RESOURCE_EXHAUSTED a stream carries at most 63 items
an item past the wire mid-stream: 0 1, RESOURCE_EXHAUSTED the reply is larger than a mudRPC wire, its handler stopped: true
a cancel after the end: OK 
a handler called directly: OK armed true, cancelled false
the peer closed first: UNAVAILABLE the transport is closed
unknown method, every free slot at once: all unimplemented
a deadline eight days out: ttl 604800
an infinite deadline: OK armed true
five thousand junk wires: then armed true
cancelled with items unread: CANCELLED Cancelled
a slow reader past its deadline: DEADLINE_EXCEEDED Deadline Exceeded
the slow reader's deadline in time: true
a three-hour ttl reaches the server as: 10800s
a second server on one channel: none, a transport serves one server
a server that never answers: DEADLINE_EXCEEDED Deadline Exceeded
the server saw its ttl pass: true
a radio refusing a listen: armed true, faults heard yes
a radio calling back in from another thread: armed true
after sixteen calls timed out: DEADLINE_EXCEEDED Deadline Exceeded
a channel dropped unclosed is freed: true
a channel dropped mid-round is freed: true
closed from its own send: no error
over UDP: armed true, telemetry 0 1 2 3 4
its socket closed: UNAVAILABLE the transport is closed
a public bind: Error -1, a public peer: Error -1, this library public: false
an octal address: Error -1, no peer: Error -1
a service name of 70 bytes: none, mudRPC carries a service's full name of 1 to 64 bytes of UTF-8: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
a service name of 6 bytes, not UTF-8: none, mudRPC carries a service's full name of 1 to 64 bytes of UTF-8: test.
three on UDP, one serving nothing: 10 of 10 armed
a call when its channel closes: UNAVAILABLE the transport is closed
a handler hears its channel close: true
a shutdown past its deadline: CANCELLED the server shut down, the handler heard it: true
a server built after a shutdown, the old one let go: armed true
native stream: 2 1 0, OK 
native refusal: FAILED_PRECONDITION native says no
a reply that does not decode: INTERNAL Failed to parse server response
a client stream from a native peer: status 12 mudRPC carries no client stream
a caller behind 48 owed a stream each: armed true
restored at a higher epoch: no error, at the same: Error -6, a short copy: Error -4
a closed peer: Error -4, pending 0, a short key: Error -1
every server returned from Wait
", "{name}");
    }
}

/// The gRPC drop-in for Go: protoc-gen-go's and protoc-gen-go-grpc's code for `grpc-go/drone.proto`, a client and a server
/// written as grpc-go code is, unchanged, over two peers, over UDP and against a native peer, plain and under the race
/// detector, and the skill's program. Its modules come from the Go proxy, pinned by hash in `grpc-go/go.sum`, and the two
/// plugins are built from them. End to end: Go, protoc and the network.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_go_drop_in_runs_the_code_protoc_gen_go_grpc_generates() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let at = std::path::Path::new(&root).join("target/grpc-go");
    let _ = std::fs::remove_dir_all(&at);
    for d in ["pb", "mud", "example/mud"] { std::fs::create_dir_all(at.join(d)).unwrap(); }
    let copy = |from: &str, to: &str| { std::fs::copy(format!("{dir}/{from}"), at.join(to)).unwrap(); };
    for f in ["go.mod", "go.sum", "main.go"] { copy(&format!("tests/grpc-go/{f}"), f); }
    for (f, from) in [("mud.go", "../go/mud.go"), ("mudrpc.h", "mudrpc.h")] { copy(from, &format!("mud/{f}")); copy(from, &format!("example/mud/{f}")); }
    copy("../go/mud_grpc.go", "mud/mud_grpc.go");   // the example has no gRPC, and leaves it out as the skill says
    let run = |cwd: &std::path::Path, cmd: &str, args: &[&str]| {
        let o = std::process::Command::new(cmd).args(args).current_dir(cwd).env("GOTOOLCHAIN", "local").env("CGO_LDFLAGS", format!("-L{root}/target/debug -Wl,-rpath,{root}/target/debug")).output().unwrap();
        assert!(o.status.success(), "{cmd} {args:?}\n{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    run(&at, "go", &["build", "-o", "bin/", "google.golang.org/grpc/cmd/protoc-gen-go-grpc", "google.golang.org/protobuf/cmd/protoc-gen-go"]);
    run(&at, "protoc", &["--plugin=protoc-gen-go=bin/protoc-gen-go", "--plugin=protoc-gen-go-grpc=bin/protoc-gen-go-grpc", "--go_out=pb", "--go_opt=paths=source_relative", "--go-grpc_out=pb", "--go-grpc_opt=paths=source_relative", "-I", &format!("{dir}/tests/grpc-go"), "drone.proto"]);
    let (skill, example) = (std::fs::read_to_string(format!("{dir}/../go/SKILL.md")).unwrap(), at.join("example"));
    std::fs::write(example.join("main.go"), skill.split("```go\n").nth(1).unwrap().split("```").next().unwrap()).unwrap();
    std::fs::write(example.join("go.mod"), "module example\n\ngo 1.27\n").unwrap();
    assert_eq!(run(&example, "go", &["run", "."]), "drone: Arm\ngcs: status 0 armed\n");
    run(&at, "go", &["vet", "./..."]);
    run(&at, "go", &["build", "-o", "app", "."]);
    run(&at, "go", &["build", "-race", "-o", "race", "."]);
    let mut running = Vec::new();   // both started before either is waited on
    for name in ["app", "race"] { running.push(std::process::Command::new(at.join(name)).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()); }
    for (child, name) in running.into_iter().zip(["app", "race"]) {
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success() && o.stderr.is_empty(), "{name}\n{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        assert_eq!(String::from_utf8_lossy(&o.stdout), "\
arm: armed true id 7 by mudrpc:1, <nil>
refused: FailedPrecondition battery low
handler panics: Unknown panic: thirteen
a method the service leaves out: Unimplemented method Fly not implemented
unknown method: Unimplemented unknown method Land for service test.drone.Drone
a service name empty, past 64 bytes or not UTF-8, a method name not UTF-8: InvalidArgument mudRPC: invalid argument
a method name of 65 bytes, carried: Unimplemented unknown method yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy for service test.drone.Drone
then armed true
an error that is no status: Unknown not a status
a nil reply: OK
telemetry reversed: 0 1 2 3 4, <nil>
telemetry empty: \"\", <nil>
telemetry fails after: 0 1, Aborted lost the camera
upload: Unimplemented mudRPC carries no client stream
chat: Unimplemented mudRPC carries no client stream
too large: ResourceExhausted mudRPC: too large
deadline: DeadlineExceeded context deadline exceeded
deadline in time: true
cancelled: Canceled context canceled
cancelled before it starts: Canceled context canceled
past the deadline: DeadlineExceeded context deadline exceeded
after the cut: armed true
a reply past the wire: ResourceExhausted the reply is larger than a mudRPC wire
a detail of 100 two-byte characters: FailedPrecondition, 64 kept, 128 bytes
a detail that is not UTF-8: FailedPrecondition bad  byte
a code with no message: Internal \"\"
cancel racing a reply: 30 of 30 finished
saved while calling: 50 calls answered, saves made
two full streams back to back: 63 63
a 64th item: 63 then ResourceExhausted a stream carries at most 63 items
an item past the wire mid-stream: 0 1, ResourceExhausted the reply is larger than a mudRPC wire, its handler stopped: true
the peer closed first: Unavailable the channel is closed
unknown method, every free slot at once: all Unimplemented true
a deadline eight days out: ttl 604800
five thousand junk wires: then armed true
cancelled with items unread: Canceled context canceled
a slow reader past its deadline: DeadlineExceeded context deadline exceeded
the slow reader's deadline in time: true
after sixteen calls timed out: armed true
a radio refusing a listen: armed true, <nil>, faults heard true
closed from its own send: returns
over UDP: armed true, <nil>, telemetry 0 1 2 3 4
its socket closed: Unavailable the channel is closed
a public bind: mudRPC: invalid argument, a public peer: mudRPC: invalid argument, this library public: false
an octal address: mudRPC: invalid argument, no peer: mudRPC: invalid argument
a server that never answers: DeadlineExceeded context deadline exceeded
the server saw its ttl pass: true
then the next caller: armed true
an infinite deadline: armed true, <nil>
a service name of 70 bytes: refused by name
a service name not UTF-8: refused by name
the wrong handler type: refused by type
closed from inside Use: returns
three on UDP, one serving nothing: 10 of 10 armed
a stop past its grace: running Canceled the server shut down and Canceled the server shut down, the handler heard it: true
a call after the stop: DeadlineExceeded context deadline exceeded
a handler hears its channel close: true
native stream: 2 1 0, <nil>
native refusal: FailedPrecondition native says no
a reply that does not decode: Internal
a caller behind 48 owed a stream each: armed true, <nil>
restored at a higher epoch: <nil>, at the same: mudRPC: epoch not higher, a short copy: mudRPC: corrupt
a closed peer: mudRPC: corrupt, pending 0, a short key: mudRPC: invalid argument
every channel closed
", "{name}");
    }
}

/// The gRPC drop-in for PHP: the client `grpc_php_plugin` generates from `grpc-php/drone.proto`, unchanged over `Mud\Channel`,
/// against a native PHP server in-process and over UDP to one in its own process. Its packages come from Packagist into
/// `target/grpc-php` by `composer install`, pinned by `grpc-php/composer.lock`. End to end: PHP with FFI, composer, protoc,
/// `grpc_php_plugin` and the network.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_php_drop_in_runs_the_client_grpc_php_plugin_generates() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let at = std::path::Path::new(&root).join("target/grpc-php");
    let _ = std::fs::remove_dir_all(&at);
    for d in ["gen", "mud", "example"] { std::fs::create_dir_all(at.join(d)).unwrap(); }
    let copy = |from: &str, to: &str| { std::fs::copy(format!("{dir}/{from}"), at.join(to)).unwrap(); };
    for f in ["composer.json", "composer.lock", "main.php", "native.php", "server.php"] { copy(&format!("tests/grpc-php/{f}"), f); }
    copy("../php/Mud.php", "mud/Mud.php");
    copy("../php/Mud.php", "example/Mud.php");
    copy("../php/MudGrpc.php", "mud/MudGrpc.php");
    let run = |cwd: &std::path::Path, cmd: &str, args: &[&str]| {
        let o = std::process::Command::new(cmd).args(args).current_dir(cwd).env("MUDRPC_LIB", shared_lib(&root)).output().unwrap();
        assert!(o.status.success() && o.stderr.is_empty(), "{cmd} {args:?}\n{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    let plugin = String::from_utf8(std::process::Command::new("which").arg("grpc_php_plugin").output().unwrap().stdout).unwrap();
    run(&at, "protoc", &["-I", &format!("{dir}/tests/grpc-php"), "--php_out=gen", "--grpc_out=gen", &format!("--plugin=protoc-gen-grpc={}", plugin.trim()), "drone.proto"]);
    run(&at, "composer", &["install", "--quiet", "--no-interaction"]);
    let skill = std::fs::read_to_string(format!("{dir}/../php/SKILL.md")).unwrap();
    std::fs::write(at.join("example/example.php"), skill.split("```php\n").nth(1).unwrap().split("```").next().unwrap()).unwrap();
    assert_eq!(run(&at.join("example"), "php", &["example.php"]), "drone: Arm\ngcs: status 0 armed\n");
    assert_eq!(run(&at, "php", &["main.php"]), "\
arm: armed true id 7 by mudrpc:1
peer: mudrpc:2, metadata [] [], target drone, state 2
refused: FAILED_PRECONDITION battery low
unknown method: UNIMPLEMENTED no method test.Drone/Fly
a service name empty, past 64 bytes or not UTF-8, a method name not UTF-8: INVALID_ARGUMENT mudRPC: invalid argument
a method name of 65 bytes, carried: UNIMPLEMENTED no method test.Drone/yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy
telemetry reversed: 0 1 2 3 4, OK
telemetry empty: , OK
telemetry fails after: 0 1, ABORTED lost the camera
upload: UNIMPLEMENTED mudRPC carries no client stream
chat: NULL, UNIMPLEMENTED mudRPC carries no client stream
too large: RESOURCE_EXHAUSTED mudRPC: too large
deadline: DEADLINE_EXCEEDED Deadline Exceeded
deadline in time: true
cancelled: CANCELLED Cancelled
past the deadline: DEADLINE_EXCEEDED Deadline Exceeded
a timeout of NaN: DEADLINE_EXCEEDED Deadline Exceeded
an infinite timeout: armed true id 1 by mudrpc:1
a timeout that is no number: armed true id 1 by mudrpc:1
after the cut: armed true id 11 by mudrpc:1
a code with no details: INTERNAL \"\"
a reply that does not decode: Error occurred during parsing: Unexpected wire type.
two full streams back to back: 63 63
the peer closed first: UNAVAILABLE the peer is closed: mudRPC: corrupt
a seventeenth call: RESOURCE_EXHAUSTED mudRPC: full
unknown method, every free slot at once: 16 UNIMPLEMENTED no method test.Drone/Fly
a deadline eight days out: ttl 604800
five thousand junk wires: then armed true id 1 by mudrpc:1
cancelled with items unread: 0, more false, CANCELLED Cancelled
a slow reader past its deadline: 0 1, DEADLINE_EXCEEDED Deadline Exceeded
the slow reader's deadline in time: true
saved mid-stream: 0 1 2 3 4, saves made
a radio refusing a listen: armed true id 1 by mudrpc:7, faults heard yes
over UDP: armed true id 1 by mudrpc:11, telemetry 0 1 2 3 4, OK
its socket closed: UNAVAILABLE the socket is closed
a public bind: mudRPC: invalid argument, a public peer: mudRPC: invalid argument, no peer: mudRPC: invalid argument, this library public: false
a server that never answers: DEADLINE_EXCEEDED Deadline Exceeded
restored at a higher epoch: ok, at the same: mudRPC: epoch not higher, a short copy: mudRPC: corrupt
a closed peer: mudRPC: corrupt, pending 0, a short key: the key and the seed are 32 bytes
after the stub closed: UNAVAILABLE the channel is closed
every channel closed
");
}

/// The Android binding on this machine's JVM: `mud_jni.c` built against the JDK's `jni.h` over the shared library, the Java
/// harness over it on every scenario and mode with logs byte-identical to the Rust ones, the Java skill's example, the
/// gRPC drop-in compiled against it, and `libmudrpc.so` for arm64-v8a linked by the NDK. End to end: a JDK, a C compiler,
/// curl and the network, and the NDK at `ANDROID_NDK_HOME` or where Homebrew puts it.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_android_binding_runs_the_java_harness_over_jni() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let out = std::path::Path::new(&root).join("target/android");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(out.join("classes")).unwrap();
    let run = |cmd: &str, args: &[&str], env: &[(&str, &str)]| {
        let mut c = std::process::Command::new(cmd);
        c.args(args).current_dir(&root);
        for (k, v) in env { c.env(k, v); }
        let o = c.output().unwrap();
        assert!(o.status.success(), "{cmd} {args:?}\n{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    let home = std::path::Path::new(&jdk("javac")).parent().unwrap().parent().unwrap().to_path_buf();
    let home = if home.join("include/jni.h").exists() { home } else { std::path::PathBuf::from(run("/usr/libexec/java_home", &[], &[]).trim()) };
    let (os, lib) = if cfg!(target_os = "macos") { ("darwin", out.join("libmudrpc_jni.dylib")) } else { ("linux", out.join("libmudrpc_jni.so")) };
    run("cc", &["-shared", "-fPIC", "-Wall", "-Wextra", "-Wno-unused-parameter", "-Werror", "-o", lib.to_str().unwrap(), &format!("{dir}/../android/mud_jni.c"), "-I", dir, "-I", &format!("{}/include", home.display()), "-I", &format!("{}/include/{os}", home.display()), "-L", &format!("{root}/target/debug"), "-lmudrpc", &format!("-Wl,-rpath,{root}/target/debug")], &[]);
    let classes = out.join("classes");
    let grpc = maven(&root, "", MAVEN[0].1, MAVEN[0].2);
    run(&jdk("javac"), &["-d", classes.to_str().unwrap(), "-cp", grpc.to_str().unwrap(), &format!("{dir}/../android/Mud.java"), &format!("{dir}/../java/MudChannel.java"), &format!("{dir}/tests/java/Harness.java")], &[]);   // the drop-in compiles against the Android binding: one API
    let env = [("MUDRPC_LIB", lib.to_str().unwrap())];
    for (name, flags) in modes() {
        let want = expected(&root, name, flags);
        let got = run(&jdk("java"), &[&["--enable-native-access=ALL-UNNAMED", "-cp", classes.to_str().unwrap(), "Harness", &format!("{root}/scenarios/{name}.scenario")], flags].concat(), &env);
        assert_eq!(got, want, "Android log for {name} {flags:?} differs from the Rust log");
    }
    let skill = std::fs::read_to_string(format!("{dir}/../java/SKILL.md")).unwrap();
    std::fs::write(out.join("Example.java"), skill.split("```java\n").nth(1).unwrap().split("```").next().unwrap()).unwrap();
    run(&jdk("javac"), &["-cp", classes.to_str().unwrap(), "-d", classes.to_str().unwrap(), out.join("Example.java").to_str().unwrap()], &[]);
    assert_eq!(run(&jdk("java"), &["--enable-native-access=ALL-UNNAMED", "-cp", classes.to_str().unwrap(), "Example"], &env), "drone: Arm\ngcs: status 0 armed\n");
    let ndk = std::env::var("ANDROID_NDK_HOME").unwrap_or_else(|_| "/opt/homebrew/share/android-ndk".into());
    let prebuilt = std::fs::read_dir(format!("{ndk}/toolchains/llvm/prebuilt")).expect("the Android NDK: brew install --cask android-ndk, or ANDROID_NDK_HOME").next().unwrap().unwrap().path();
    run("cargo", &["rustc", "--locked", "--release", "-p", "mudrpc-c", "--target", "aarch64-linux-android", "--crate-type", "staticlib"], &[]);
    let so = out.join("libmudrpc.so");
    run(prebuilt.join("bin/aarch64-linux-android24-clang").to_str().unwrap(), &["-shared", "-fPIC", "-Wall", "-Wextra", "-Wno-unused-parameter", "-Werror", "-o", so.to_str().unwrap(), &format!("{dir}/../android/mud_jni.c"), "-I", dir, &format!("{root}/target/aarch64-linux-android/release/libmudrpc.a")], &[]);
    assert!(run(prebuilt.join("bin/llvm-nm").to_str().unwrap(), &["-D", so.to_str().unwrap()], &[]).contains(" T JNI_OnLoad"), "libmudrpc.so for arm64-v8a exports JNI_OnLoad");
}

/// Maven Central artefacts the Java and Kotlin drop-in tests build with, each pinned by its SHA-256: (folder, path).
const MAVEN: [(&str, &str, &str); 18] = [
    ("", "io/grpc/grpc-api/1.85.1/grpc-api-1.85.1.jar", "159371ce7ecb1de72f910571436f7866baa595c820271fcb930961212b7c6363"),
    ("", "io/grpc/grpc-stub/1.85.1/grpc-stub-1.85.1.jar", "755b4dbccbfa3ea68bbd6e8af78ca6997c9b118710e6c194058dd55af1acdbc4"),
    ("", "io/grpc/grpc-protobuf/1.85.1/grpc-protobuf-1.85.1.jar", "08bc136b79e93f8030e167444960fd3fd80fc5423a178a84a32cbdd02de75e55"),
    ("", "io/grpc/grpc-protobuf-lite/1.85.1/grpc-protobuf-lite-1.85.1.jar", "4d93f01cf105edb879a2ec49cf39a370a2603af46d264b4d5cab645289fc8ecb"),
    ("", "com/google/protobuf/protobuf-java/4.36.2/protobuf-java-4.36.2.jar", "0773e72eaf375e1285df330575bfec5bda5da674d544d0e142a17f89c8f754da"),
    ("", "com/google/guava/guava/33.6.0-android/guava-33.6.0-android.jar", "9532ca9ba8aceb4cd2067171f981c799988b7f5844e3e274811a3bb757034bc4"),
    ("", "com/google/guava/failureaccess/1.0.3/failureaccess-1.0.3.jar", "cbfc3906b19b8f55dd7cfd6dfe0aa4532e834250d7f080bd8d211a3e246b59cb"),
    ("", "org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21.jar", "bd8250210584cb659847dce9cda660f8e9906e5def7fbebcea93dc6dcb6a88a3"),
    ("", "org/jetbrains/kotlinx/kotlinx-coroutines-core-jvm/1.10.2/kotlinx-coroutines-core-jvm-1.10.2.jar", "5ca175b38df331fd64155b35cd8cae1251fa9ee369709b36d42e0a288ccce3fd"),
    ("", "io/grpc/grpc-kotlin-stub/1.5.0/grpc-kotlin-stub-1.5.0.jar", "cc482c0e2f6b588ba90c7152d4aeee27cfee4bde1688326327dc74c6284477f0"),
    ("", "javax/annotation/javax.annotation-api/1.3.2/javax.annotation-api-1.3.2.jar", "e04ba5195bcd555dc95650f7cc614d151e4bcd52d29a10b8aa2197f3ab89ab9b"),
    ("kotlin", "org/jetbrains/kotlin/kotlin-compiler-embeddable/2.4.21/kotlin-compiler-embeddable-2.4.21.jar", "ef19419c765e7ac8404465fa026ca8fc4dbb8a822de0fc79aa53c8dce29f1d02"),
    ("kotlin", "org/jetbrains/kotlin/kotlin-build-tools-api/2.4.21/kotlin-build-tools-api-2.4.21.jar", "c5cd6a49bd072b529ae10809111c16a7aa0f92147e95d288354017fd65799461"),
    ("kotlin", "org/jetbrains/kotlin/kotlin-script-runtime/2.4.21/kotlin-script-runtime-2.4.21.jar", "a658cf5902a1525a06c264a5e42b437e97a6679ddeef8ca641246f270cf6a013"),
    ("kotlin", "org/jetbrains/kotlin/kotlin-reflect/1.6.10/kotlin-reflect-1.6.10.jar", "3277ac102ae17aad10a55abec75ff5696c8d109790396434b496e75087854203"),
    ("kotlin", "org/jetbrains/kotlin/kotlin-daemon-embeddable/2.4.21/kotlin-daemon-embeddable-2.4.21.jar", "7b3ef24674e5bcffcadfc4e43202ea314194939ecbcdd90a4770b2f1b04e0d07"),
    ("kotlin", "org/jetbrains/annotations/13.0/annotations-13.0.jar", "ace2a10dc8e2d5fd34925ecac03e4988b2c0f851650c94b8cef49ba1bd111478"),
    ("plugin", "io/grpc/protoc-gen-grpc-kotlin/1.5.0/protoc-gen-grpc-kotlin-1.5.0-jdk8.jar", "da938e9047a7973d53916eee33a1465adce1e72dce75127474b8808388023026"),
];

/// protoc and grpc-java's plugin for this machine: (path, SHA-256) each.
const fn tools() -> [(&'static str, &'static str); 2] {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        [("com/google/protobuf/protoc/4.36.2/protoc-4.36.2-osx-aarch_64.exe", "765ccb18be73e565bfc3e2ad94e85039ca28f928b3e62b487e51b7b8fdf1dcfb"), ("io/grpc/protoc-gen-grpc-java/1.85.1/protoc-gen-grpc-java-1.85.1-osx-aarch_64.exe", "e5dc43dc4c2a31123b6edb62283e98d721edd94d216d946da564ae64e96fcd3b")]
    } else {
        [("com/google/protobuf/protoc/4.36.2/protoc-4.36.2-linux-x86_64.exe", "f6ee22d65361338459208da8bff801c9ca689741cba9187b34990dd45198f864"), ("io/grpc/protoc-gen-grpc-java/1.85.1/protoc-gen-grpc-java-1.85.1-linux-x86_64.exe", "e0701e7a47cc8e486c62a4abedecdecc2ae9a8cadd66a690f5a35f56b1d4a540")]
    }
}

/// One artefact in `target/maven/<folder>`, fetched once from Maven Central and refused unless its SHA-256 is the pinned one.
fn maven(root: &str, folder: &str, path: &str, sha: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(root).join("target/maven").join(folder);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(path.rsplit('/').next().unwrap());
    let digest = |f: &std::path::Path| String::from_utf8(std::process::Command::new("shasum").args(["-a", "256"]).arg(f).output().unwrap().stdout).unwrap().chars().take(64).collect::<String>();
    if !file.exists() || digest(&file) != sha {
        let got = std::process::Command::new("curl").args(["-sfL", "-o"]).arg(&file).arg(format!("https://repo1.maven.org/maven2/{path}")).status().unwrap();
        assert!(got.success(), "fetch {path}");
    }
    assert_eq!(digest(&file), sha, "{path} is not the pinned artefact");
    file
}

/// The gRPC drop-in on Java and Kotlin: grpc-java's stubs and base, then grpc-kotlin's coroutine stub and base, generated
/// by protoc from `grpc-java/drone.proto` and run unchanged over `MudChannel`. End to end: a JDK, curl and the network.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_java_and_kotlin_drop_in_runs_the_code_grpc_java_and_grpc_kotlin_generate() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let jars: Vec<_> = MAVEN.iter().map(|(f, p, s)| maven(&root, f, p, s)).collect();
    let [protoc, plugin] = tools().map(|(p, s)| maven(&root, "", p, s));
    for t in [&protoc, &plugin] { assert!(std::process::Command::new("chmod").arg("+x").arg(t).status().unwrap().success()); }
    let join = |v: Vec<&std::path::PathBuf>| v.iter().map(|p| p.to_str().unwrap()).collect::<Vec<_>>().join(":");
    let runtime = join(jars.iter().filter(|p| p.parent().unwrap().ends_with("maven")).collect());
    let compiler = join(jars.iter().filter(|p| p.parent().unwrap().ends_with("kotlin")).chain(jars.iter().filter(|p| p.to_str().unwrap().contains("kotlin-stdlib") || p.to_str().unwrap().contains("coroutines"))).collect());
    let ktplugin = jars.iter().find(|p| p.to_str().unwrap().contains("protoc-gen-grpc-kotlin")).unwrap();
    let out = std::env::temp_dir().join("mudrpc-grpc-java");
    let _ = std::fs::remove_dir_all(&out);
    let (src, classes) = (out.join("src"), out.join("classes"));
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&classes).unwrap();
    let wrapper = out.join("protoc-gen-grpckt");
    std::fs::write(&wrapper, format!("#!/bin/sh\nexec {} -jar {} \"$@\"\n", jdk("java"), ktplugin.display())).unwrap();
    assert!(std::process::Command::new("chmod").arg("+x").arg(&wrapper).status().unwrap().success());
    let generated = std::process::Command::new(&protoc).arg(format!("--plugin=protoc-gen-grpc-java={}", plugin.display())).arg(format!("--plugin=protoc-gen-grpckt={}", wrapper.display()))
        .arg(format!("--java_out={}", src.display())).arg(format!("--grpc-java_out=@generated=omit:{}", src.display())).arg(format!("--grpckt_out={}", src.display()))
        .args(["-I", &format!("{dir}/tests/grpc-java"), "drone.proto"]).output().unwrap();
    assert!(generated.status.success(), "{}", String::from_utf8_lossy(&generated.stderr));
    let sources = |ext: &str| { let mut v = Vec::new(); for e in std::fs::read_dir(src.join("test")).unwrap() { let p = e.unwrap().path(); if p.extension().is_some_and(|x| x == ext) { v.push(p); } } v };
    let built = std::process::Command::new(jdk("javac")).args(["-d", classes.to_str().unwrap(), "-cp", &runtime]).args(sources("java")).args([format!("{dir}/../java/Mud.java"), format!("{dir}/../java/MudChannel.java"), format!("{dir}/tests/grpc-java/Main.java")]).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let kotlin = std::process::Command::new(jdk("java")).args(["--enable-native-access=ALL-UNNAMED", "-cp", &compiler, "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler", "-no-stdlib", "-no-reflect", "-jvm-target", "21", "-cp", &format!("{}:{runtime}", classes.display()), "-d", classes.to_str().unwrap()])
        .args(sources("kt")).arg(format!("{dir}/tests/grpc-java/Main.kt")).output().unwrap();
    assert!(kotlin.status.success(), "{}", String::from_utf8_lossy(&kotlin.stderr));
    let run = |main: &str| std::process::Command::new(jdk("java")).args(["--enable-native-access=ALL-UNNAMED", "-cp", &format!("{}:{runtime}", classes.display()), main]).env("MUDRPC_LIB", shared_lib(&root)).output().unwrap();
    let java = run("Main");
    assert!(java.status.success(), "{}", String::from_utf8_lossy(&java.stderr));
    assert_eq!(String::from_utf8_lossy(&java.stdout), "\
arm: armed true id 7 by mudrpc:1
future: armed true
refused: FAILED_PRECONDITION battery low
handler throws: UNKNOWN Application error processing RPC
unknown method: UNIMPLEMENTED no method test.Drone/Fly
a method name that is not UTF-8: INVALID_ARGUMENT mudRPC: invalid argument, then armed true
telemetry reversed: 0 1 2 3 4
telemetry empty: 
telemetry fails after: 0 1
telemetry fails: ABORTED lost the camera
upload: UNIMPLEMENTED mudRPC carries no client stream
too large: RESOURCE_EXHAUSTED mudRPC: too large
deadline: DEADLINE_EXCEEDED deadline exceeded
deadline in time: true
cancelled: CANCELLED io.grpc.Context was cancelled without error
future cancelled: CANCELLED the future
after the cut: armed true
past the deadline: DEADLINE_EXCEEDED deadline exceeded
undecodable request: INTERNAL Failed to deserialize request message.
cancelled before sending, 16 times: pending 0 more, then armed true
cancel racing a reply: 30 of 30 finished
saved while calling: 50 calls answered, saves made
two full streams back to back: 63 63
a 64th item: UNKNOWN Application error processing RPC
an item past the wire mid-stream: 0 1, RESOURCE_EXHAUSTED the reply is larger than a mudRPC wire, its handler stopped: true
the peer closed first: UNAVAILABLE the channel is closed
unknown method, every free slot at once: all UNIMPLEMENTED
null description: INTERNAL 
a null wire: NullPointerException
a deadline eight days out: ttl 604800
five thousand junk wires: then armed true
a transport refusing a listen: armed true, faults heard yes
closed from its own send: returns
over UDP: armed true, telemetry 0 1 2 3 4
its socket closed: UNAVAILABLE the channel is closed
a public bind: MudException, a public peer: MudException, this library public: false
a reply past the wire: RESOURCE_EXHAUSTED the reply is larger than a mudRPC wire
request(-1): IllegalArgumentException
start after cancel: IllegalStateException
a listener that throws: CANCELLED Failed to read message.
a call after close: UNAVAILABLE the channel is closed
closed inside its own callback: returns
closed from a direct executor's callback: returns
cancelled with items unread: CANCELLED after 1
a slow reader past its deadline: DEADLINE_EXCEEDED deadline exceeded
the slow reader's deadline in time: true
an executor that refuses: OK, then armed true
a second halfClose: IllegalStateException
a transport refusing for 300 ms: armed true
a server that never answers: DEADLINE_EXCEEDED deadline exceeded
the server heard its ttl pass: true
three on UDP, one serving nothing: 10 of 10 armed
65 calls on 64 handlers: 64 running, then 65
native stream: 2 1 0
native refusal: FAILED_PRECONDITION native says no
a caller behind 48 owed a stream each: armed true
");
    let kt = run("MainKt");
    assert!(kt.status.success(), "{}", String::from_utf8_lossy(&kt.stderr));
    assert_eq!(String::from_utf8_lossy(&kt.stdout), "\
kotlin arm: armed true
kotlin telemetry: 0 1 2 3 4
kotlin refused: FAILED_PRECONDITION battery low
kotlin upload: UNIMPLEMENTED mudRPC carries no client stream
kotlin deadline: DEADLINE_EXCEEDED deadline exceeded
kotlin after the cut: armed true
");
}

const SCENARIOS: [&str; 16] = ["partition-heal", "jamming-window", "hostile-peer", "bandwidth-collapse", "rollback", "insider-flood", "rotation", "hostile-method", "noisy-radio", "stale-epoch", "torn-save", "fleet-of-40", "stranger", "hub-telemetry", "junk-at-bandwidth", "relay-holds-past-ttl"];
/// The readers' scenarios, each a round-loop line every harness carries.
const READER: [&str; 11] = ["noisy-relay-ring", "warp-off-the-round", "full-at-both-ends", "starved-command", "down-caller", "warp-on-the-call", "many-calls-one-round", "unsorted-host-events", "expired-across-the-warp", "warps-in-one-round", "warp-past-the-end"];
/// The campaign's runs every harness but Python's takes on a push, about 4 s each; Python's would take 9.
const CAMPAIGN: [&str; 2] = ["fleet-of-256", "reflash-within-the-week"];
/// Every scenario under each sealer the ABI builds, as the harness's flags: `Key`, `Signed<Key>`, `Gcm`, `Signed<Gcm>`.
fn modes() -> impl Iterator<Item = (&'static str, &'static [&'static str])> {
    [&[][..], &["--signed"], &["--gcm"], &["--gcm", "--signed"]].into_iter().flat_map(|f| SCENARIOS.into_iter().chain(READER).chain(CAMPAIGN).map(move |n| (n, f)))
}
/// The log a harness must match: the checked-in log for its sealer, or the one it falls back to as adapters' end-to-end
/// tests do (`.gcm.log`, then `.log`); signed with none checked in, the Rust harness's run under the same sealer.
fn expected(root: &str, name: &str, flags: &[&str]) -> String {
    let (gcm, signed, read) = (flags.contains(&"--gcm"), flags.contains(&"--signed"), |l: &str| std::fs::read_to_string(format!("{root}/scenarios/{name}.{l}")));
    let kept = if signed { ["gcm.signed.log", "signed.log"] } else { ["gcm.log", "log"] };
    if let Some(log) = kept[usize::from(!gcm)..].iter().find_map(|l| read(l).ok()) { return log; }
    assert!(signed, "{name}.log missing");
    let s = mudrpc_adapters::sim::Scenario::parse(&read("scenario").unwrap()).unwrap();
    (if gcm { mudrpc_adapters::harness::run_gcm_signed } else { mudrpc_adapters::harness::run_signed })(&s).join("\n") + "\n"
}

fn shared_lib(root: &str) -> String {
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    format!("{root}/target/debug/libmudrpc.{ext}")
}

/// The example in the .NET skill, compiled as a project with Mud.cs beside it, then a second program on the edges.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_dotnet_skill_example_runs() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    let skill = std::fs::read_to_string(format!("{dir}/../dotnet/SKILL.md")).unwrap();
    let start = skill.find("```csharp\n").expect("a ```csharp block") + 10;
    let end = start + skill[start..].find("```").unwrap();
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let extra = r#"
using System;
using System.Linq;
using MudRpc;

var key = Enumerable.Range(0, 32).Select(i => (byte)i).ToArray();
var a = new Peer("gcs", key, 1, 1); var b = new Peer("drone", key, 2, 1); b.Listen("nav");
if (!a.Outgoing(0)!.Listen) throw new Exception("first item is the inbox listen"); a.Sent();
var cid = a.Call(0, "nav", "Arm", Array.Empty<byte>(), 30); var o = a.Outgoing(0)!; a.Sent();
if (Peer.CipherDefault != Peer.CipherAes256Gcm || o.Wire[0] != 3) throw new Exception("the default is AES, version 3");
var saved = a.Save(); if (saved.Length <= 8192) throw new Exception("save too small");
var inc = b.Deliver(1, o.Topic, o.Wire);
if (inc.Kind != Kind.Call || inc.Method != "Arm" || inc.ReplyTo != "gcs") throw new Exception("call not delivered");
if (b.Deliver(1, o.Topic, o.Wire).Dropped != Dropped.Duplicate) throw new Exception("duplicate accepted");
if (b.Deliver(1, o.Topic, new byte[40]).Dropped != Dropped.Version) throw new Exception("version 0 accepted");
if (b.Deliver(1, o.Topic, Enumerable.Repeat((byte)3, 40).ToArray()).Dropped != Dropped.Open) throw new Exception("garbage opened");
b.Reply(1, inc, 3, System.Text.Encoding.UTF8.GetBytes("no"), detail: "not armed");
Outgoing r; while ((r = b.Outgoing(1)!).Listen) b.Sent(); b.Sent();
var got = a.Deliver(1, r.Topic, r.Wire);
if (got.Kind != Kind.Reply || got.Status != 3 || got.Detail != "not armed") throw new Exception("the detail lost");
var restored = Peer.Restore(saved, 2);
var ex = restored.Tick(2);
if (ex == null || ex.Kind != Kind.Expired || !ex.Id.SequenceEqual(cid)) throw new Exception("call in flight not expired after restore");
if (restored.Deliver(2, r.Topic, r.Wire).Dropped != Dropped.NoPending) throw new Exception("late reply matched after restore");
restored.Dispose();
try { restored.Tick(3); throw new Exception("used after close"); } catch (MudException e) when (e.Code == -1) { }
try { Peer.Restore(saved.Take(saved.Length - 1).ToArray(), 2); throw new Exception("short block accepted"); } catch (MudException e) when (e.Code == -4) { }
try { Peer.Restore(saved, 1); throw new Exception("same epoch accepted"); } catch (MudException e) when (e.Code == -6) { }
try { new Peer("x", key, 0, 1).Publish(0, "t", new byte[1], 1); throw new Exception("sender 0 sent"); } catch (MudException e) when (e.Code == -7) { }
a.Stage(Enumerable.Range(9, 32).Select(i => (byte)i).ToArray()); a.Rotate(); if (a.Opened != Slot.Current) throw new Exception("slot"); a.Retire();
try { a.Publish(5, "t", new byte[600], 10); throw new Exception("too large accepted"); } catch (MudException e) when (e.Code == -3) { }
int n = 0;
try { while (true) { a.Publish(5, "t", new byte[1], 10); n++; } } catch (MudException e) when (e.IsFull) { }
if (n != 16) throw new Exception("outbox holds " + n);
while (a.Outgoing(6) != null) a.Sent();
for (var j = 0; j < 32; j++) { a.Publish(6, "t", new byte[1], 10); a.Outgoing(6); a.Sent(); }
try { a.Publish(6, "t", new byte[1], 10); throw new Exception("33rd send accepted"); } catch (MudException e) when (e.Code == -9) { }
byte[] Counting(int s) => Enumerable.Range(s, 32).Select(i => (byte)i).ToArray();
var hub = new Peer("gcs", key, 1, 1, Counting(101), 1000, true); var leaf = new Peer("drone", key, 2, 1, Counting(102));
for (uint s = 1; s <= 16; s++) leaf.Roster(s, Peer.PublicKey(Counting(100 + (int)s)));
try { leaf.Roster(17, Peer.PublicKey(Counting(117))); throw new Exception("17th line accepted"); } catch (MudException e) when (e.IsFull) { }
for (uint s = 1; s <= 40; s++) hub.Roster(s, Peer.PublicKey(Counting(100 + (int)s)));
var hs = hub.Save(); if (hs.Length <= saved.Length) throw new Exception("a hub saves at its own size");
if (Peer.Restore(hs, 2).Counts.Managed != 40) throw new Exception("a hub restores at its own size");
void Expect(int code, Action f) { try { f(); } catch (MudException e) when (e.Code == code) { return; } throw new Exception("accepted, wanted " + code); }
Expect(-1, () => new Peer("x", new byte[32], 1, 1));
Expect(-1, () => new Peer("x", key, 1, 1, new byte[32]));
Expect(-1, () => new Peer("x", key, 1, 1, hub: true));
var retagged = (byte[])saved.Clone(); retagged[0] = 0; Expect(-4, () => Peer.Restore(retagged, 2));
Expect(-5, () => Peer.Restore(hs.Take(saved.Length).ToArray(), 2));
var c = new Peer("c", key, 3, 1);
Expect(-3, () => c.Publish(0, new string('t', 65), new byte[1], 10));
c.Publish(0, "t", new byte[1], 0); c.Publish(0, "t", new byte[1], uint.MaxValue);
var ttls = new System.Collections.Generic.List<uint>(); for (Outgoing? q; (q = c.Outgoing(0)) != null; c.Sent()) if (!q.Listen) ttls.Add(q.TtlSecs);
if (!ttls.SequenceEqual(new uint[] { 1, 604800 })) throw new Exception("ttl not clamped: " + string.Join(",", ttls));
for (var j = 0; j < 16; j++) { c.Call(1, "nav", "M", Array.Empty<byte>(), 10); c.Outgoing(1); c.Sent(); }
Expect(-2, () => c.Call(1, "nav", "M", Array.Empty<byte>(), 10));
Expect(-1, () => c.Publish(0, "", new byte[1], 10)); Expect(-1, () => new Peer("", key, 4, 1));
if (c.Pending != 16 || c.Queued != 0) throw new Exception($"pending {c.Pending} queued {c.Queued}");
Console.WriteLine("ok");
"#;
    for (name, program, want) in [("example", &skill[start..end], "drone: Arm\ngcs: status 0 armed\n"), ("extra", extra, "ok\n")] {
        let out = std::env::temp_dir().join(format!("mudrpc-skill-dotnet-{name}"));
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("Program.cs"), program).unwrap();
        std::fs::write(out.join(format!("{name}.csproj")), format!(r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net8.0</TargetFramework><Nullable>enable</Nullable><ImplicitUsings>disable</ImplicitUsings></PropertyGroup><ItemGroup><Compile Include="{dir}/../dotnet/Mud.cs" /></ItemGroup></Project>"#)).unwrap();
        let run = std::process::Command::new("dotnet").args(["run", "-c", "Release", "-nologo", "-v", "q", "--project", out.to_str().unwrap()]).env("MUDRPC_LIB", shared_lib(&root)).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{name}: {}", String::from_utf8_lossy(&run.stderr));
        assert!(run.status.success(), "{name}: {}", String::from_utf8_lossy(&run.stderr));
    }
}

/// The Java harness, compiled once with javac and run on every scenario. End to end: needs a JDK 22 or later and a build.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_java_harness_matches_the_checked_in_logs() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let out = std::env::temp_dir().join("mudrpc-harness-java");
    let built = std::process::Command::new(jdk("javac")).args(["-d", out.to_str().unwrap(), &format!("{dir}/../java/Mud.java"), &format!("{dir}/tests/java/Harness.java")]).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    for (name, flags) in modes() {
        let want = expected(&root, name, flags);
        let run = std::process::Command::new(jdk("java")).args(["--enable-native-access=ALL-UNNAMED", "-cp", out.to_str().unwrap(), "Harness", &format!("{root}/scenarios/{name}.scenario")]).args(flags).env("MUDRPC_LIB", shared_lib(&root)).output().unwrap();
        assert!(run.status.success(), "Java harness on {name} {flags:?}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "Java log for {name} {flags:?} differs from the Rust log");
    }
}

/// The JDK: `JAVA_HOME`, else Homebrew's keg on a Mac, else whatever is on the path.
fn jdk(tool: &str) -> String {
    let home = std::env::var("JAVA_HOME").unwrap_or_else(|_| "/opt/homebrew/opt/openjdk".into());
    if std::path::Path::new(&home).exists() { format!("{home}/bin/{tool}") } else { tool.into() }
}

/// The example in the Java skill, compiled with Mud.java beside it, then a second program on the edges.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_java_skill_example_runs() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    let skill = std::fs::read_to_string(format!("{dir}/../java/SKILL.md")).unwrap();
    let start = skill.find("```java\n").expect("a ```java block") + 8;
    let end = start + skill[start..].find("```").unwrap();
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let extra = r#"
import mudrpc.Mud.*;
import java.util.Arrays;

public class Extra {
    static void expect(int code, Runnable r) { try { r.run(); } catch (MudException e) { if (e.code == code) return; throw e; } throw new IllegalStateException("accepted, wanted " + code); }
    static void check(boolean ok, String what) { if (!ok) throw new IllegalStateException(what); }
    static byte[] filled(int n, int v) { byte[] b = new byte[n]; Arrays.fill(b, (byte) v); return b; }
    static byte[] counting(int start) { byte[] b = new byte[32]; for (int i = 0; i < 32; i++) b[i] = (byte) (start + i); return b; }

    public static void main(String[] args) {
        byte[] key = counting(0);
        Peer a = new Peer("gcs", key, 1, 1), b = new Peer("drone", key, 2, 1); b.listen("nav");
        check(a.outgoing(0).listen(), "first item is the inbox listen"); a.sent();
        byte[] cid = a.call(0, "nav", "Arm", new byte[0], 30); Outgoing o = a.outgoing(0); a.sent();
        check(Peer.CIPHER_DEFAULT == Peer.CIPHER_AES_256_GCM && o.wire()[0] == 3, "the default is AES, version 3");
        byte[] saved = a.save(); check(saved.length > 8192, "save too small");
        Incoming inc = b.deliver(1, o.topic(), o.wire());
        check(inc.kind() == Kind.CALL && inc.method().equals("Arm") && inc.replyTo().equals("gcs"), "call not delivered");
        check(b.deliver(1, o.topic(), o.wire()).dropped() == Dropped.DUPLICATE, "duplicate accepted");
        check(b.deliver(1, o.topic(), new byte[40]).dropped() == Dropped.VERSION, "version 0 accepted");
        check(b.deliver(1, o.topic(), filled(40, 3)).dropped() == Dropped.OPEN, "garbage opened");
        b.reply(1, inc, 3, "no".getBytes(), true, 0, "not armed");
        Outgoing r; while ((r = b.outgoing(1)).listen()) b.sent(); b.sent();
        Incoming got = a.deliver(1, r.topic(), r.wire());
        check(got.kind() == Kind.REPLY && got.status() == 3 && got.detail().equals("not armed"), "the detail lost");
        Peer restored = Peer.restore(saved, 2);
        Incoming ex = restored.tick(2);
        check(ex != null && ex.kind() == Kind.EXPIRED && Arrays.equals(ex.id(), cid), "call in flight not expired after restore");
        check(restored.deliver(2, r.topic(), r.wire()).dropped() == Dropped.NO_PENDING, "late reply matched after restore");
        restored.close();
        expect(-1, () -> restored.tick(3));
        expect(-1, restored::save);
        expect(-4, () -> Peer.restore(Arrays.copyOf(saved, saved.length - 1), 2));
        expect(-6, () -> Peer.restore(saved, 1));
        expect(-7, () -> new Peer("x", key, 0, 1).publish(0, "t", new byte[1], 1));
        a.stage(counting(9)); a.rotate(); check(a.opened() == Slot.CURRENT, "slot"); a.retire();
        expect(-3, () -> a.publish(5, "t", new byte[600], 10));
        int n = 0;
        try { while (true) { a.publish(5, "t", new byte[1], 10); n++; } } catch (MudException e) { if (!e.isFull()) throw e; }
        check(n == 16, "outbox holds " + n);
        while (a.outgoing(6) != null) a.sent();
        for (int j = 0; j < 32; j++) { a.publish(6, "t", new byte[1], 10); a.outgoing(6); a.sent(); }
        expect(-9, () -> a.publish(6, "t", new byte[1], 10));
        Peer hub = new Peer("gcs", key, 1, 1, counting(101), 1000, true), leaf = new Peer("drone", key, 2, 1, counting(102), 1000);
        for (int s = 1; s <= 16; s++) leaf.roster(s, Peer.publicKey(counting(100 + s)), null);
        expect(-2, () -> leaf.roster(17, Peer.publicKey(counting(117)), null));
        for (int s = 1; s <= 40; s++) hub.roster(s, Peer.publicKey(counting(100 + s)), null);
        byte[] hs = hub.save(); check(hs.length > saved.length, "a hub saves at its own size");
        check(Peer.restore(hs, 2).counts().managed() == 40, "a hub restores at its own size");
        expect(-1, () -> new Peer("x", new byte[32], 1, 1));
        expect(-1, () -> new Peer("x", key, 1, 1, new byte[32], 1000));
        expect(-1, () -> new Peer("x", key, 1, 1, null, 1000, true));
        byte[] retagged = saved.clone(); retagged[0] = 0; expect(-4, () -> Peer.restore(retagged, 2));
        expect(-5, () -> Peer.restore(Arrays.copyOf(hs, saved.length), 2));
        Peer c = new Peer("c", key, 3, 1);
        expect(-3, () -> c.publish(0, "t".repeat(65), new byte[1], 10));
        c.publish(0, "t", new byte[1], 0); c.publish(0, "t", new byte[1], -1);
        java.util.List<Integer> ttls = new java.util.ArrayList<>(); for (Outgoing q; (q = c.outgoing(0)) != null; c.sent()) if (!q.listen()) ttls.add(q.ttlSecs());
        check(ttls.equals(java.util.List.of(1, 604800)), "ttl not clamped: " + ttls);
        for (int j = 0; j < 16; j++) { c.call(1, "nav", "M", new byte[0], 10); c.outgoing(1); c.sent(); }
        expect(-2, () -> c.call(1, "nav", "M", new byte[0], 10));
        expect(-1, () -> c.publish(0, "", new byte[1], 10)); expect(-1, () -> new Peer("", key, 4, 1));
        check(c.pending() == 16 && c.queued() == 0, "pending " + c.pending() + " queued " + c.queued());
        System.out.println("ok");
    }
}
"#;
    for (name, program, want) in [("Example", &skill[start..end], "drone: Arm\ngcs: status 0 armed\n"), ("Extra", extra, "ok\n")] {
        let out = std::env::temp_dir().join(format!("mudrpc-skill-java-{name}"));
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join(format!("{name}.java")), program).unwrap();
        let built = std::process::Command::new(jdk("javac")).args(["-d", out.to_str().unwrap(), &format!("{dir}/../java/Mud.java"), out.join(format!("{name}.java")).to_str().unwrap()]).output().unwrap();
        assert!(built.status.success(), "{name}: {}", String::from_utf8_lossy(&built.stderr));
        let run = std::process::Command::new(jdk("java")).args(["--enable-native-access=ALL-UNNAMED", "-cp", out.to_str().unwrap(), name]).env("MUDRPC_LIB", shared_lib(&root)).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{name}: {}", String::from_utf8_lossy(&run.stderr));
        assert!(run.status.success(), "{name}: {}", String::from_utf8_lossy(&run.stderr));
    }
}

/// The JavaScript harness over the wasm build, run with Node on every scenario. End to end: needs Node 22 or later.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_js_harness_matches_the_checked_in_logs() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    let wasm = wasm(&root);
    for (name, flags) in modes() {
        let want = expected(&root, name, flags);
        let run = std::process::Command::new("node").args([&format!("{dir}/tests/js/harness.js"), &format!("{root}/scenarios/{name}.scenario")]).args(flags).env("MUDRPC_WASM", &wasm).output().unwrap();
        assert!(run.status.success(), "JavaScript harness on {name} {flags:?}: {}", String::from_utf8_lossy(&run.stderr));
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "JavaScript log for {name} {flags:?} differs from the Rust log");
    }
}

/// Node 22 or later on the path, then the C crate built for wasm32-unknown-unknown; the path of the binary.
fn wasm(root: &str) -> String {
    static BUILT: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); // once per process: two tests building at once raced on the binary
    let v = std::process::Command::new("node").arg("--version").output().expect("Node 22 or later is needed for the JavaScript tests: https://nodejs.org");
    let v = String::from_utf8_lossy(&v.stdout).trim().to_string();
    let major: u32 = v.trim_start_matches('v').split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
    assert!(major >= 22, "Node 22 or later is needed for the JavaScript tests, found {v}");
    assert!(BUILT.get_or_init(|| std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c", "--target", "wasm32-unknown-unknown"]).current_dir(root).status().unwrap().success()), "rustup target add wasm32-unknown-unknown");
    format!("{root}/target/wasm32-unknown-unknown/debug/mudrpc.wasm")
}

/// The example in the JavaScript skill, run with mud.js and the binary beside it, then a second program on the edges.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_js_skill_example_runs() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    let skill = std::fs::read_to_string(format!("{dir}/../js/SKILL.md")).unwrap();
    let start = skill.find("```js\n").expect("a ```js block") + 6;
    let end = start + skill[start..].find("```").unwrap();
    let wasm = wasm(&root);
    let extra = r"
import { Peer, MudError, CIPHER_DEFAULT, CIPHER_AES_256_GCM } from './mud.js';
const expect = (code, f) => { try { f(); } catch (e) { if (e instanceof MudError && e.code === code) return; throw e; } throw new Error('accepted, wanted ' + code); };
const check = (ok, what) => { if (!ok) throw new Error(what); };
const filled = (n, v) => new Uint8Array(n).fill(v), counting = (start) => Uint8Array.from({ length: 32 }, (_, i) => start + i);
const same = (x, y) => x.length === y.length && x.every((v, i) => v === y[i]);
const key = counting(0);
const a = new Peer('gcs', key, 1, 1), b = new Peer('drone', key, 2, 1); b.listen('nav');
check(a.outgoing(0).listen, 'first item is the inbox listen'); a.sent();
const cid = a.call(0, 'nav', 'Arm', new Uint8Array(0), 30); const o = a.outgoing(0); a.sent();
check(CIPHER_DEFAULT === CIPHER_AES_256_GCM && o.wire[0] === 3, 'the default is AES, version 3');
const saved = a.save(); check(saved.length > 8192, 'save too small');
const inc = b.deliver(1, o.topic, o.wire);
check(inc.kind === 'call' && inc.method === 'Arm' && inc.replyTo === 'gcs', 'call not delivered');
check(b.deliver(1, o.topic, o.wire).dropped === 'duplicate', 'duplicate accepted');
check(b.deliver(1, o.topic, new Uint8Array(40)).dropped === 'version', 'version 0 accepted');
check(b.deliver(1, o.topic, filled(40, 3)).dropped === 'open', 'garbage opened');
b.reply(1, inc, 3, new TextEncoder().encode('no'), true, 0, 'not armed');
let r; while ((r = b.outgoing(1)).listen) b.sent(); b.sent();
const got = a.deliver(1, r.topic, r.wire);
check(got.kind === 'reply' && got.status === 3 && got.detail === 'not armed', 'the detail lost');
const restored = Peer.restore(saved, 2);
const ex = restored.tick(2);
check(ex && ex.kind === 'expired' && same(ex.id, cid), 'call in flight not expired after restore');
check(restored.deliver(2, r.topic, r.wire).dropped === 'noPending', 'late reply matched after restore');
restored.close();
expect(-1, () => restored.tick(3));
expect(-4, () => Peer.restore(saved.slice(0, -1), 2));
expect(-6, () => Peer.restore(saved, 1));
expect(-7, () => new Peer('x', key, 0, 1).publish(0, 't', new Uint8Array(1), 1));
a.stage(counting(9)); a.rotate(); check(a.opened() === 'current', 'slot'); a.retire();
expect(-3, () => a.publish(5, 't', new Uint8Array(600), 10));
let n = 0;
try { for (;;) { a.publish(5, 't', new Uint8Array(1), 10); n++; } } catch (e) { if (!(e instanceof MudError && e.isFull)) throw e; }
check(n === 16, 'outbox holds ' + n);
while (a.outgoing(6)) a.sent();
for (let j = 0; j < 32; j++) { a.publish(6, 't', new Uint8Array(1), 10); a.outgoing(6); a.sent(); }
expect(-9, () => a.publish(6, 't', new Uint8Array(1), 10));
const hub = new Peer('gcs', key, 1, 1, counting(101), 1000, true), leaf = new Peer('drone', key, 2, 1, counting(102));
for (let s = 1; s <= 16; s++) leaf.roster(s, Peer.publicKey(counting(100 + s)));
expect(-2, () => leaf.roster(17, Peer.publicKey(counting(117))));
for (let s = 1; s <= 40; s++) hub.roster(s, Peer.publicKey(counting(100 + s)));
const hs = hub.save(); check(hs.length > saved.length, 'a hub saves at its own size');
check(Peer.restore(hs, 2).counts().managed === 40, 'a hub restores at its own size');
expect(-1, () => new Peer('x', new Uint8Array(32), 1, 1));
expect(-1, () => new Peer('x', key, 1, 1, new Uint8Array(32)));
expect(-1, () => new Peer('x', key, 1, 1, null, 1000, true));
const retagged = saved.slice(); retagged[0] = 0; expect(-4, () => Peer.restore(retagged, 2));
expect(-5, () => Peer.restore(hs.slice(0, saved.length), 2));
const c = new Peer('c', key, 3, 1);
expect(-3, () => c.publish(0, 't'.repeat(65), new Uint8Array(1), 10));
c.publish(0, 't', new Uint8Array(1), 0); c.publish(0, 't', new Uint8Array(1), 0xFFFFFFFF);
const ttls = []; for (let q; (q = c.outgoing(0)); c.sent()) if (!q.listen) ttls.push(q.ttlSecs);
check(same(ttls, [1, 604800]), 'ttl not clamped: ' + ttls);
for (let j = 0; j < 16; j++) { c.call(1, 'nav', 'M', new Uint8Array(0), 10); c.outgoing(1); c.sent(); }
expect(-2, () => c.call(1, 'nav', 'M', new Uint8Array(0), 10));
expect(-1, () => c.publish(0, '', new Uint8Array(1), 10)); expect(-1, () => new Peer('', key, 4, 1));
check(c.pending() === 16 && c.queued() === 0, `pending ${c.pending()} queued ${c.queued()}`);
const many = Array.from({ length: 3000 }, (_, i) => new Peer('p' + i, key, i + 1, 1, counting(i & 0xff), 100)); // the module's memory grows under the constructor
for (const p of many) p.close();
console.log('ok');
";
    for (name, program, want) in [("example", &skill[start..end], "drone: Arm\ngcs: status 0 armed\n"), ("extra", extra, "ok\n")] {
        let out = std::env::temp_dir().join(format!("mudrpc-skill-js-{name}"));
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join(format!("{name}.js")), program).unwrap();
        std::fs::copy(format!("{dir}/../js/mud.js"), out.join("mud.js")).unwrap();
        std::fs::copy(&wasm, out.join("mudrpc.wasm")).unwrap();
        let run = std::process::Command::new("node").arg(out.join(format!("{name}.js"))).env_remove("MUDRPC_WASM").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{name}: {}", String::from_utf8_lossy(&run.stderr));
        assert!(run.status.success(), "{name}: {}", String::from_utf8_lossy(&run.stderr));
    }
}

/// The standard harness in C and in Python, each run over the simulator through the ABI on every scenario (Python's
/// not on the campaign's), must produce exactly the checked-in logs. End to end.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_c_and_python_harnesses_match_the_checked_in_logs() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let out = std::env::temp_dir().join("mudrpc-harness");
    std::fs::create_dir_all(&out).unwrap();
    let mut cc = std::process::Command::new("cc");
    cc.args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I", dir, &format!("{dir}/tests/harness.c"), &format!("{root}/target/debug/libmudrpc.a"), "-o", out.join("harness").to_str().unwrap()]);
    if cfg!(target_os = "linux") { cc.args(["-lpthread", "-ldl", "-lm"]); }
    let built = cc.output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    for (name, flags) in modes() {
        let scenario = format!("{root}/scenarios/{name}.scenario");
        let want = expected(&root, name, flags);
        let c = std::process::Command::new(out.join("harness")).arg(&scenario).args(flags).output().unwrap();
        assert!(c.status.success(), "C harness on {name} {flags:?}: {}", String::from_utf8_lossy(&c.stderr));
        assert_eq!(String::from_utf8_lossy(&c.stdout), want, "C log for {name} {flags:?} differs from the Rust log");
        if CAMPAIGN.contains(&name) { continue; }
        let boot = format!("import sys, runpy; sys.path.insert(0, {:?}); sys.argv = ['harness.py', {scenario:?}] + {flags:?}; runpy.run_path({:?}, run_name='__main__')", format!("{dir}/../python"), format!("{dir}/tests/harness.py"));
        let py = std::process::Command::new("python3").args(["-I", "-c", &boot]).env("MUDRPC_LIB", format!("{root}/target/debug/libmudrpc.{ext}")).output().unwrap();
        assert!(py.status.success(), "Python harness on {name} {flags:?}: {}", String::from_utf8_lossy(&py.stderr));
        assert_eq!(String::from_utf8_lossy(&py.stdout), want, "Python log for {name} {flags:?} differs from the Rust log");
    }
}

#[test]
fn drop_zeroes_the_whole_block_and_the_keys_with_it() {
    let mut blk = peer("gcs", 2);
    let bytes = |b: &mut Block| unsafe { std::slice::from_raw_parts(b.mem() as *const u8, mud_size()).to_vec() };
    let has = |h: &[u8], n: &[u8]| h.windows(n.len()).any(|w| w == n);
    unsafe {
        assert_eq!(mud_stage(blk.mem(), blk.len(), b(&k(9))), MUD_OK);
        assert!(has(&bytes(&mut blk), &k(1)) && has(&bytes(&mut blk), &k(9)), "both keys sit in the block while the peer lives");
        assert_eq!(mud_drop(blk.mem(), blk.len()), MUD_OK);
        assert!(bytes(&mut blk).iter().all(|&b| b == 0), "after drop the caller's block is zeros: no key, no name, no wire");
        assert_eq!(mud_resume(blk.mem(), mud_size(), 2), MUD_ECORRUPT, "and cannot be mistaken for a peer");
        assert_eq!(mud_drop(ptr::null_mut(), mud_size()), MUD_EINVAL);
    }
}

#[test]
fn a_short_key_is_refused_unread_and_bytes_that_are_not_a_peer_are_corrupt_at_every_entry() {
    let key = k(1);
    let mut blk = Block::new();
    unsafe {
        let short = mud_config { name: b(b"gcs"), key: b(&key[..16]), sender: 1, epoch: 1, seed: b(&[]), budget: 0, hub: 0, cipher: 0 };
        assert_eq!(mud_new(blk.mem(), mud_size(), &raw const short), MUD_EINVAL, "16 bytes: refused, never read past");
        let mut live = peer("gcs", 2);
        assert_eq!(mud_stage(live.mem(), live.len(), b(&key[..31])), MUD_EINVAL);
        // a block never built or resumed: zeros, or a peer's bytes after mud_drop
        let verdicts = every_entry(blk.mem(), blk.len());
        assert!(verdicts.iter().all(|&v| v == MUD_ECORRUPT), "{verdicts:?}");
        assert_eq!((mud_pressure(blk.mem(), blk.len()), mud_count(blk.mem(), blk.len(), 0)), (0, 0));
        assert_eq!(mud_drop(live.mem(), live.len()), MUD_OK);
        assert_eq!(mud_call(live.mem(), live.len(), 0, b(b"nav"), b(b"M"), b(b""), 10, &mut mud_id { bytes: [0; 12] }), MUD_ECORRUPT, "after drop the block is zeros, and zeros are not a peer");
    }
}

#[test]
fn a_short_scratch_is_an_error_not_an_empty_verdict() {
    let mut gcs = peer("gcs", 2);
    unsafe {
        let mut id = mud_id { bytes: [0; 12] };
        assert_eq!(mud_call(gcs.mem(), gcs.len(), 0, b(b"nav"), b(b"M"), b(b""), 10, &raw mut id), MUD_OK);
        let (mut small, mut out, mut inc) = ([0u8; 100], mud_out { kind: 0, topic: b(&[]), wire: b(&[]), ttl_secs: 0 }, blank());
        assert_eq!(mud_outgoing(gcs.mem(), gcs.len(), 0, m(&mut small), &raw mut out), MUD_EINVAL, "not MUD_NONE: the item is still queued");
        assert_eq!(mud_deliver(gcs.mem(), gcs.len(), 0, b(b"nav"), b(&[1u8; 40]), m(&mut small), &raw mut inc), MUD_EINVAL);
        let mut big = [0u8; 64 + 512];
        assert_eq!(mud_outgoing(gcs.mem(), gcs.len(), 0, m(&mut big), &raw mut out), MUD_LISTEN);
    }
}

struct Rng(u64);
impl Rng {
    const fn next(&mut self) -> u64 { let mut x = self.0; x ^= x << 13; x ^= x >> 7; x ^= x << 17; self.0 = x; x }
    const fn below(&mut self, n: usize) -> usize { if n == 0 { 0 } else { (self.next() % n as u64) as usize } }
}
fn env(name: &str, default: u64) -> u64 { std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default) }

/// Every entry with random arguments against a live peer, a zeroed block and random bytes wearing a peer's magic: a code
/// every time, never a crash, and the live peer still answers afterwards. A random block may legitimately come back
/// `MUD_EPANIC`, which is the guard doing its job; the live peer never may.
#[test]
fn every_entry_answers_a_code_to_random_arguments_and_random_blocks() {
    let mut rng = Rng(env("MUDRPC_SEED", 1) ^ 7);
    let mut live = peer("gcs", 2);
    let mut arena = vec![0u8; 4096];
    for (i, x) in arena.iter_mut().enumerate() { *x = i as u8; }
    let mut blk = Block::new();
    for round in 0..env("MUDRPC_FUZZ", 3_000) {
        let which = rng.below(3);
        let m = unsafe {
            match which {
                0 => live.mem(),
                1 => { std::ptr::write_bytes(blk.mem().cast::<u8>(), 0, mud_size()); blk.mem() }
                _ => { let p = blk.mem().cast::<u8>(); for i in 0..mud_size() { *p.add(i) = rng.next() as u8; } std::ptr::copy_nonoverlapping(live.mem() as *const u8, p, 4); blk.mem() }
            }
        };
        let arg = |rng: &mut Rng| -> mud_buf { let s = rng.below(4000); let n = rng.below(96).min(4096 - s); if rng.below(10) == 0 { mud_buf { ptr: std::ptr::null(), len: n } } else { mud_buf { ptr: arena[s..].as_ptr(), len: n } } };
        let (mut id, mut inc, mut out) = (mud_id { bytes: [0; 12] }, blank(), mud_out { kind: 0, topic: b(&[]), wire: b(&[]), ttl_secs: 0 });
        inc.topic = arg(&mut rng); inc.method = arg(&mut rng); inc.reply_to = arg(&mut rng); inc.bytes = arg(&mut rng); inc.ttl_secs = rng.next() as u32; inc.seq = rng.next() as u32 % 70;
        let mut scratch = vec![0u8; rng.below(1200)];
        let now = rng.next() as u32 % 100_000;
        let len = if rng.below(4) == 0 { rng.below(blk.len() + 1) } else { blk.len() };
        let code = unsafe {
            match rng.below(13) {
                0 => mud_call(m, len, now, arg(&mut rng), arg(&mut rng), arg(&mut rng), rng.next() as u32, if rng.below(8) == 0 { std::ptr::null_mut() } else { &raw mut id }),
                1 => mud_publish(m, len, now, arg(&mut rng), arg(&mut rng), rng.next() as u32),
                2 => mud_listen(m, len, arg(&mut rng)),
                3 => mud_reply(m, len, now, if rng.below(8) == 0 { std::ptr::null() } else { &raw const inc }, rng.next() as u32 % 70, rng.next() as u32, b(b""), arg(&mut rng), rng.next() as i32 & 1),
                4 => mud_deliver(m, len, now, arg(&mut rng), arg(&mut rng), mud_mutbuf { ptr: scratch.as_mut_ptr(), len: scratch.len() }, &raw mut inc),
                5 => mud_tick(m, len, now, &raw mut inc),
                6 => { let c = mud_outgoing(m, len, now, mud_mutbuf { ptr: scratch.as_mut_ptr(), len: scratch.len() }, &raw mut out); if c > 0 { mud_sent(m, len); } c }
                7 => mud_sent(m, len),
                8 => mud_stage(m, len, arg(&mut rng)),
                9 => mud_rotate(m, len),
                10 => mud_retire(m, len),
                11 => mud_opened(m, len),
                _ => mud_renew(m, len, rng.next() as u32),
            }
        };
        assert!((-9..=7).contains(&code) || code == MUD_EPANIC, "round {round}: {code} is not a code"); // error codes, MUD_NONE, kinds up to MUD_ROLLBACK
        if which == 0 { assert_ne!(code, MUD_EPANIC, "round {round}: the live peer panicked under the guard"); }
    }
    unsafe {
        let mut id = mud_id { bytes: [0; 12] };
        let r = mud_call(live.mem(), live.len(), 1_000_000, b(b"nav"), b(b"M"), b(b""), 10, &raw mut id);
        assert!(r == MUD_OK || r == MUD_EFULL, "the live peer still answers: {r}");
    }
}

#[test]
fn sent_takes_only_what_outgoing_showed() {
    let mut gcs = peer("gcs", 2);
    unsafe {
        assert_eq!(mud_sent(gcs.mem(), gcs.len()), MUD_OK, "nothing shown: a no-op, not a blind pop");
        let mut scratch = [0u8; 64 + 512];
        let mut out = mud_out { kind: 0, topic: b(&[]), wire: b(&[]), ttl_secs: 0 };
        assert_eq!(mud_outgoing(gcs.mem(), gcs.len(), 0, m(&mut scratch), &raw mut out), MUD_LISTEN, "the listen item is still there");
        assert_eq!(mud_sent(gcs.mem(), gcs.len()), MUD_OK);
        assert_eq!(mud_sent(gcs.mem(), gcs.len()), MUD_OK);
        assert_eq!(mud_outgoing(gcs.mem(), gcs.len(), 0, m(&mut scratch), &raw mut out), MUD_NONE);
    }
}

/// `mud_save` is the one way to copy a block out, and the copy is disarmed until `mud_resume`.
#[test]
fn a_saved_copy_cannot_send_until_resumed_and_the_live_peer_still_can() {
    let (mut live, mut copy, mut again) = (peer("gcs", 1), Block::new(), Block::new());
    let mut id = mud_id { bytes: [0; 12] };
    unsafe {
        assert_eq!(mud_save(live.mem(), live.len(), copy.mem(), mud_size() - 1), MUD_EMEM, "too short");
        assert_eq!(mud_save(live.mem(), live.len(), live.mem(), mud_size()), MUD_EINVAL, "onto itself");
        assert_eq!(mud_save(live.mem(), live.len(), copy.mem(), mud_size()), MUD_OK);
        assert_eq!(mud_call(copy.mem(), copy.len(), 0, b(b"nav"), b(b"M"), b(b""), 10, &raw mut id), MUD_ECORRUPT, "restored without mud_resume: refused, not a nonce reused");
        assert_eq!(mud_publish(copy.mem(), copy.len(), 0, b(b"t"), b(b""), 10), MUD_ECORRUPT);
        assert_eq!(mud_call(live.mem(), live.len(), 0, b(b"nav"), b(b"M"), b(b""), 10, &raw mut id), MUD_OK, "the live peer is untouched");
        assert_eq!(mud_resume(copy.mem(), mud_size(), 1), MUD_EEPOCH);
        assert_eq!(mud_publish(copy.mem(), copy.len(), 0, b(b"t"), b(b""), 10), MUD_ECORRUPT, "a resume refused arms nothing");
        assert_eq!(mud_resume(copy.mem(), mud_size(), 2), MUD_OK);
        assert_eq!(mud_call(copy.mem(), copy.len(), 0, b(b"nav"), b(b"M"), b(b""), 10, &raw mut id), MUD_OK);
        assert_eq!(mud_save(copy.mem(), copy.len(), again.mem(), mud_size()), MUD_OK);
        let (a, c) = (std::slice::from_raw_parts(again.mem() as *const u8, mud_size()), std::slice::from_raw_parts(copy.mem() as *const u8, mud_size()));
        assert_eq!(a.iter().zip(c).filter(|(x, y)| x != y).count(), 1, "the copy differs from its source in one byte: the arm");
    }
}

/// The operator's counters through the ABI: a staged key is a management action, a junk wire a drop; an unknown index or a block that is not a peer's is 0.
#[test]
fn counts_through_the_abi() {
    let (mut live, mut zero) = (peer("gcs", 1), Block::new());
    let mut inc = blank();
    let mut scratch = [0u8; 512];
    let key = k(5);
    unsafe {
        assert_eq!((0..4).map(|w| mud_count(live.mem(), live.len(), w)).collect::<Vec<_>>(), [0, 0, 0, 0]);
        assert_eq!(mud_stage(live.mem(), live.len(), b(&key)), MUD_OK);
        assert_eq!(mud_deliver(live.mem(), live.len(), 0, b(b"t"), b(&[1u8; 40]), mud_mutbuf { ptr: scratch.as_mut_ptr(), len: 512 }, &raw mut inc), MUD_DROPPED, "junk: a drop");
        assert_eq!((0..4).map(|w| mud_count(live.mem(), live.len(), w)).collect::<Vec<_>>(), [1, 0, 0, 1]);
        assert_eq!((mud_count(live.mem(), live.len(), 4), mud_count(zero.mem(), zero.len(), 0)), (0, 0));
    }
}

/// One thread drives a peer, and the block says so when two do: a block an entry holds (its byte written 2 by hand here, as
/// a second thread mid-call, a crash mid-call or a torn write leaves it) is `MUD_EBUSY` at every entry that returns a code, nothing written, while
/// the four reads read; `mud_resume` clears it and a save's copy is not held. `mud_pending` and `mud_queued` through C.
#[test]
fn a_held_block_is_busy_at_every_entry_until_resumed_and_pending_and_queued_read_through_c() {
    let (mut blk, mut copy, key) = (under(MUD_CIPHER_CHACHA20_POLY1305, "gcs", 1, &k(7), 0), Block::new(), k(9));
    let (mut id, mut inc, mut scratch) = (mud_id { bytes: [0; 12] }, blank(), [0u8; 64 + 512]);
    let mut out = mud_out { kind: 0, topic: b(&[]), wire: b(&[]), ttl_secs: 0 };
    (inc.kind, inc.reply_to) = (MUD_CALL, b(b"gcs"));
    unsafe {
        let (mem, len) = (blk.mem(), blk.len());
        assert_eq!((mud_pending(mem, len), mud_queued(mem, len)), (0, 1), "the peer's own listen");
        assert_eq!(mud_call(mem, len, 0, b(b"nav"), b(b"M"), b(b""), 10, &raw mut id), MUD_OK);
        assert_eq!((mud_pending(mem, len), mud_queued(mem, len)), (1, 2));
        let busy = mem.cast::<u8>().add(mud_align() + size_of::<Auth>() - 2 * size_of::<usize>() - 4); // the peer's busy byte
        assert_eq!(*busy, 0);
        *busy = 2; // any byte but 0 is held, and only `mud_resume` takes it
        let before = std::slice::from_raw_parts(mem as *const u8, mud_size()).to_vec();
        let codes = [
            mud_save(mem, len, copy.mem(), copy.len()), mud_renew(mem, len, 3), mud_stage(mem, len, b(&key)), mud_rotate(mem, len), mud_retire(mem, len),
            mud_roster(mem, len, 2, b(&key), b(&[])), mud_revoke(mem, len, 2), mud_opened(mem, len), mud_call(mem, len, 0, b(b"nav"), b(b"M"), b(b""), 10, &raw mut id),
            mud_publish(mem, len, 0, b(b"t"), b(b""), 10), mud_listen(mem, len, b(b"t")), mud_reply(mem, len, 0, &raw const inc, 0, 0, b(b""), b(b""), 1),
            mud_deliver(mem, len, 0, b(b"t"), b(&[1u8; 40]), m(&mut scratch[..512]), &raw mut inc), mud_tick(mem, len, 20, &raw mut inc),
            mud_outgoing(mem, len, 0, m(&mut scratch), &raw mut out), mud_sent(mem, len), mud_drop(mem, len),
        ];
        assert!(codes.iter().all(|&c| c == MUD_EBUSY), "{codes:?}");
        assert_eq!(std::slice::from_raw_parts(mem as *const u8, mud_size()), before, "a busy entry wrote nothing");
        assert_eq!((mud_pending(mem, len), mud_queued(mem, len), mud_count(mem, len, 0), mud_pressure(mem, len)), (1, 2, 0, 0), "the reads read");
        assert_eq!(mud_resume(mem, len, 2), MUD_OK, "resume clears what a crash left");
        assert_eq!((mud_tick(mem, len, 0, &raw mut inc), mud_pending(mem, len)), (MUD_EXPIRED, 0), "and expires what was in flight");
        assert_eq!(mud_save(mem, len, copy.mem(), copy.len()), MUD_OK);
        assert_eq!(mud_publish(copy.mem(), copy.len(), 0, b(b"t"), b(b""), 10), MUD_ECORRUPT, "the copy is disarmed, not held");
        while mud_outgoing(mem, len, 0, m(&mut scratch), &raw mut out) > 0 { mud_sent(mem, len); }
        assert_eq!(mud_queued(mem, len), 0, "drained, the dead call wire dropped at the exit");
        assert_eq!(mud_call(mem, len, 1, b(b"nav"), b(b"M"), b(b""), 10, &raw mut id), MUD_OK);
        assert_eq!((mud_pending(mem, len), mud_queued(mem, len)), (1, 1));
    }
}

/// A secret of 32 equal bytes is what a zeroed config and a copied example carry; the ABI refuses it everywhere a secret enters.
#[test]
fn a_constant_key_or_seed_is_refused_where_it_enters() {
    let mut blk = Block::new();
    let cfg = |key: &[u8], seed: &[u8]| mud_config { name: b(b"gcs"), key: b(key), sender: 1, epoch: 1, seed: b(seed), budget: 1000, hub: 0, cipher: 0 };
    unsafe {
        assert_eq!(mud_new(blk.mem(), mud_size(), &cfg(&[0; 32], &[])), MUD_EINVAL, "a zeroed config");
        assert_eq!(mud_new(blk.mem(), mud_size(), &cfg(&[1; 32], &[])), MUD_EINVAL, "the copied example");
        assert_eq!(mud_new(blk.mem(), mud_size(), &cfg(&k(1), &[7; 32])), MUD_EINVAL, "a constant seed");
        assert_eq!(mud_new(blk.mem(), mud_size(), &cfg(&k(1), &k(7))), MUD_OK);
        assert_eq!(mud_stage(blk.mem(), blk.len(), b(&[2; 32])), MUD_EINVAL, "a constant next key");
        assert_eq!(mud_stage(blk.mem(), blk.len(), b(&k(2))), MUD_OK);
        assert_eq!(mud_drop(blk.mem(), blk.len()), MUD_OK);
    }
}

/// A leaf believes 16 senders and says `MUD_EFULL` at the 17th; a hub believes 256 in its own block; both deliver from the lines they hold.
#[test]
fn a_hub_believes_a_fleet_and_a_leaf_reports_full() {
    let (mut leaf, mut hub, mut copy, mut forty) = (Block::new(), Block::hub(), Block::hub(), Block::new());
    let (key, seeds): ([u8; 32], Vec<[u8; 32]>) = (k(1), (0..=255).map(k).collect());
    let cfg = |name: &'static [u8], sender: u32, hub: u32| mud_config { name: b(name), key: b(&key), sender, epoch: 1, seed: b(&seeds[sender as usize % 256]), budget: 1000, hub, cipher: 0 };
    let pk = |n: u32| { let mut o = [0u8; 32]; assert_eq!(unsafe { mud_public_key(b(&seeds[n as usize % 256]), o.as_mut_ptr()) }, MUD_OK); o };
    let line = |blk: &mut Block, n: u32| unsafe { mud_roster(blk.mem(), blk.len(), n, b(&pk(n)), b(&[])) };
    let (mut inc, mut out, mut id) = (blank(), mud_out { kind: 0, topic: b(&[]), wire: b(&[]), ttl_secs: 0 }, mud_id { bytes: [0; 12] });
    let (mut scratch, mut plain) = ([0u8; 600], [0u8; 512]);
    unsafe {
        assert_eq!(mud_new(leaf.mem(), mud_size(), &cfg(b"drone", 2, 0)), MUD_OK);
        assert!((1..=16).all(|n| line(&mut leaf, n) == MUD_OK));
        assert_eq!(line(&mut leaf, 17), MUD_EFULL, "the 17th line is an error");
        assert_eq!(line(&mut leaf, 16), MUD_OK, "a line it holds is replaced, not refused");
        assert_eq!(mud_new(hub.mem(), mud_size(), &cfg(b"gcs", 1, 1)), MUD_EMEM, "a hub needs its own size");
        assert_eq!(mud_new(hub.mem(), mud_hub_size(), &cfg(b"gcs", 1, 2)), MUD_EINVAL, "two shapes, no third");
        assert_eq!(mud_new(hub.mem(), mud_hub_size(), &mud_config { seed: b(&[]), budget: 0, ..cfg(b"gcs", 1, 1) }), MUD_EINVAL, "an unsigned peer has no roster to size");
        assert_eq!(mud_new(hub.mem(), mud_hub_size(), &cfg(b"gcs", 1, 1)), MUD_OK);
        assert!((1..=256).all(|n| line(&mut hub, n) == MUD_OK));
        assert_eq!(line(&mut hub, 257), MUD_EFULL, "the 257th line is an error");
        assert_eq!(mud_save(hub.mem(), hub.len(), copy.mem(), mud_size()), MUD_EMEM, "a hub saves at its own size");
        assert_eq!(mud_save(hub.mem(), hub.len(), copy.mem(), mud_hub_size()), MUD_OK);
        assert_eq!(mud_resume(copy.mem(), mud_hub_size(), 2), MUD_OK);
        assert_eq!(mud_count(copy.mem(), copy.len(), 0), 257, "every roster call took the sealer, the refused one too, and the count survived the save");
        // drone 40 calls nav: the hub holds its line and takes the call; the leaf does not and names the missing line
        assert_eq!(mud_new(forty.mem(), mud_size(), &cfg(b"drone-40", 40, 0)), MUD_OK);
        assert_eq!(mud_call(forty.mem(), forty.len(), 0, b(b"nav"), b(b"Ping"), b(b"1"), 10, &raw mut id), MUD_OK);
        while mud_outgoing(forty.mem(), forty.len(), 0, m(&mut scratch), &raw mut out) == MUD_LISTEN { mud_sent(forty.mem(), forty.len()); }
        let wire = std::slice::from_raw_parts(out.wire.ptr, out.wire.len).to_vec();
        assert_eq!(mud_deliver(copy.mem(), copy.len(), 0, b(b"nav"), b(&wire), m(&mut plain), &raw mut inc), MUD_CALL);
        assert_eq!(inc.sender, 40);
        assert_eq!(mud_deliver(leaf.mem(), leaf.len(), 0, b(b"nav"), b(&wire), m(&mut plain), &raw mut inc), MUD_DROPPED);
        assert_eq!((inc.dropped, inc.status), (2, 5), "open: no roster line");
        assert_eq!(mud_drop(copy.mem(), copy.len()), MUD_OK);
        assert!(std::slice::from_raw_parts(copy.mem() as *const u8, mud_hub_size()).iter().all(|&x| x == 0), "the whole hub block is zeroed");
        assert_eq!(mud_resume(copy.mem(), mud_hub_size(), 3), MUD_ECORRUPT);
        assert_eq!(mud_drop(hub.mem(), hub.len()), MUD_OK);
    }
    assert!(mud_hub_size() - mud_size() >= 240 * 69, "240 more lines of at least 69 bytes");
}

/// `mud_resume` takes the block's length: a tag whose shape does not fit it is `MUD_EMEM`, the tag the one thing read.
#[test]
fn resume_takes_the_length_and_a_hub_tag_in_a_leaf_buffer_is_refused_unread() {
    let (mut leaf, mut hub, mut copy, mut small) = (peer("gcs", 1), Block::hub(), Block::hub(), vec![0u64; mud_size() / 8]);
    let (key, seed) = (k(1), k(7));
    let cfg = mud_config { name: b(b"gcs"), key: b(&key), sender: 1, epoch: 1, seed: b(&seed), budget: 1000, hub: 1, cipher: 0 };
    unsafe {
        assert_eq!(mud_new(hub.mem(), mud_hub_size(), &raw const cfg), MUD_OK);
        assert_eq!(mud_save(hub.mem(), hub.len(), copy.mem(), mud_hub_size()), MUD_OK);
        let saved = std::slice::from_raw_parts(copy.mem() as *const u8, mud_hub_size()).to_vec();
        ptr::copy_nonoverlapping(saved.as_ptr(), small.as_mut_ptr().cast::<u8>(), mud_size());
        assert_eq!(mud_resume(small.as_mut_ptr().cast::<c_void>(), mud_size(), 2), MUD_EMEM, "a hub's first bytes in a leaf's buffer: refused, never read past");
        for len in [0, 3, mud_size(), mud_hub_size() - 1] { assert_eq!(mud_resume(copy.mem(), len, 2), MUD_EMEM, "{len}"); }
        assert_eq!(std::slice::from_raw_parts(copy.mem() as *const u8, mud_hub_size()), saved, "a refused resume wrote nothing");
        assert_eq!(mud_resume(copy.mem(), mud_hub_size(), 2), MUD_OK);
        for len in [0, 3, 4, mud_size() - 1] { assert_eq!(mud_resume(leaf.mem(), len, 2), MUD_EMEM, "{len}"); }
        assert_eq!(mud_resume(leaf.mem(), mud_size(), 2), MUD_OK);
        assert_eq!(mud_resume(ptr::null_mut(), mud_size(), 2), MUD_EINVAL);
    }
}

/// A saved block holds no byte the peer did not write: built over a stack of 0xCC, saved, and searched for a run of it.
#[test]
#[allow(clippy::large_stack_arrays)] // the frame is the dirt the peer is built over
fn a_saved_block_carries_nothing_from_the_stack() {
    #[inline(never)]
    fn dirty() { let mut z = [0xCCu8; 256 * 1024]; std::hint::black_box(&mut z); }
    let (key, seed) = (k(1), k(40));
    for (s, hub, cipher) in [0, 1].into_iter().flat_map(|c| [(&[][..], 0, c), (&seed[..], 0, c), (&seed[..], 1, c)]) {
        let n = if hub == 1 { mud_hub_size() } else { mud_size() };
        let (mut live, mut saved) = (Block::hub(), Block::hub());
        let cfg = mud_config { name: b(b"gcs"), key: b(&key), sender: 1, epoch: 1, seed: b(s), budget: 10, hub, cipher };
        dirty();
        unsafe {
            assert_eq!(mud_new(live.mem(), n, &raw const cfg), MUD_OK);
            assert_eq!(mud_save(live.mem(), live.len(), saved.mem(), n), MUD_OK);
            let run = std::slice::from_raw_parts(saved.mem() as *const u8, n).split(|&x| x != 0xCC).map(<[u8]>::len).max();
            assert!(run <= Some(3), "seed {} hub {hub} cipher {cipher}: a run of {run:?} stack bytes in the save", s.len());
        }
    }
}

/// A peer under `cipher` (0 ChaCha20-Poly1305, 1 AES-256-GCM), signed when `seed` is not empty, in the block its role needs.
fn under(cipher: u32, name: &str, sender: u32, seed: &[u8], hub: u32) -> Block {
    let (mut blk, n) = if hub == 1 { (Block::hub(), mud_hub_size()) } else { (Block::new(), mud_size()) };
    let key = k(1);
    let cfg = mud_config { name: b(name.as_bytes()), key: b(&key), sender, epoch: 1, seed: b(seed), budget: 1000, hub, cipher };
    assert_eq!(unsafe { mud_new(blk.mem(), n, &raw const cfg) }, MUD_OK);
    blk
}
/// The next wire `blk` hands its transport, past its listens.
fn next_wire(blk: &mut Block, now: u32) -> Vec<u8> {
    let (mut scratch, mut o) = ([0u8; 64 + 512], mud_out { kind: 0, topic: b(&[]), wire: b(&[]), ttl_secs: 0 });
    unsafe {
        while mud_outgoing(blk.mem(), blk.len(), now, m(&mut scratch), &raw mut o) == MUD_LISTEN { mud_sent(blk.mem(), blk.len()); }
        assert_eq!(o.kind, MUD_SEND);
        let w = std::slice::from_raw_parts(o.wire.ptr, o.wire.len).to_vec();
        mud_sent(blk.mem(), blk.len());
        w
    }
}
const fn tag(blk: &mut Block) -> u32 { unsafe { blk.mem().cast::<u32>().read() } }

/// A config naming `MUD_CIPHER_DEFAULT` builds an AES peer: tag 3, wire version 3.
#[test]
fn the_default_cipher_builds_a_version_3_peer() {
    let mut blk = under(MUD_CIPHER_DEFAULT, "gcs", 1, &[], 0);
    assert_eq!(tag(&mut blk), 3);
    assert_eq!(unsafe { mud_publish(blk.mem(), blk.len(), 0, b(b"nav"), b(b"x"), 10) }, MUD_OK);
    assert_eq!(next_wire(&mut blk, 0)[0], 3);
}

/// AES through `mud_new`: unsigned and signed pairs call and reply, each cipher drops the other's wire by its version byte,
/// and the fleet key's three slots rotate as `Key`'s do.
#[test]
fn aes_peers_through_the_abi_call_reply_rotate_and_drop_the_other_cipher_by_version() {
    let key = k(1);
    let third = mud_config { name: b(b"x"), key: b(&key), sender: 1, epoch: 1, seed: b(&[]), budget: 0, hub: 0, cipher: 2 };
    assert_eq!(unsafe { mud_new(Block::new().mem(), mud_size(), &raw const third) }, MUD_EINVAL, "two ciphers, no third");
    let (s1, s2) = (k(41), k(42));
    let (mut plain, mut inc, mut back, mut id) = ([0u8; 512], blank(), blank(), mud_id { bytes: [0; 12] });
    let line = |blk: &mut Block, n: u32, s: &[u8; 32]| unsafe { let mut pk = [0u8; 32]; assert_eq!(mud_public_key(b(s), pk.as_mut_ptr()), MUD_OK); assert_eq!(mud_roster(blk.mem(), blk.len(), n, b(&pk), b(&[])), MUD_OK); };
    for (seed1, seed2, aes, chacha) in [(&[][..], &[][..], 3, 1), (&s1[..], &s2[..], 4, 2)] {
        let (mut gcs, mut drone, mut old) = (under(1, "gcs", 1, seed1, 0), under(1, "drone", 2, seed2, 0), under(0, "drone", 2, seed2, 0));
        if aes == 4 { for blk in [&mut gcs, &mut drone, &mut old] { line(blk, 1, &s1); line(blk, 2, &s2); } }
        unsafe {
            assert_eq!((tag(&mut gcs), tag(&mut old)), (u32::from(aes), u32::from(chacha) - 1), "AES leaves are tags 3 and 4, after ChaCha's 0 and 1");
            assert_eq!((mud_listen(drone.mem(), drone.len(), b(b"nav")), mud_listen(old.mem(), old.len(), b(b"nav"))), (MUD_OK, MUD_OK));
            assert_eq!(mud_call(gcs.mem(), gcs.len(), 0, b(b"nav"), b(b"Arm"), b(b"1"), 30, &raw mut id), MUD_OK);
            let w = next_wire(&mut gcs, 0);
            assert_eq!(w[0], aes, "the wire's version byte");
            assert_eq!(mud_deliver(old.mem(), old.len(), 1, b(b"nav"), b(&w), m(&mut plain), &raw mut inc), MUD_DROPPED);
            assert_eq!((inc.dropped, inc.status), (1, u32::from(aes)), "a ChaCha peer drops an AES wire as Version({aes})");
            assert_eq!(mud_deliver(drone.mem(), drone.len(), 1, b(b"nav"), b(&w), m(&mut plain), &raw mut inc), MUD_CALL);
            assert_eq!((std::slice::from_raw_parts(inc.method.ptr, inc.method.len), inc.sender), (&b"Arm"[..], 1));
            assert_eq!(mud_reply(drone.mem(), drone.len(), 1, &raw const inc, 0, 7, b(b""), b(b"armed"), 1), MUD_OK);
            let r = next_wire(&mut drone, 1);
            assert_eq!(mud_deliver(gcs.mem(), gcs.len(), 2, b(b"gcs"), b(&r), m(&mut plain), &raw mut back), MUD_REPLY);
            assert_eq!((std::slice::from_raw_parts(back.bytes.ptr, back.bytes.len), back.status, back.id.bytes), (&b"armed"[..], 7, id.bytes));
            assert_eq!(mud_publish(old.mem(), old.len(), 2, b(b"nav"), b(b"x"), 10), MUD_OK);
            let c = next_wire(&mut old, 2);
            assert_eq!(c[0], chacha);
            assert_eq!(mud_deliver(drone.mem(), drone.len(), 2, b(b"nav"), b(&c), m(&mut plain), &raw mut inc), MUD_DROPPED);
            assert_eq!((inc.dropped, inc.status), (1, u32::from(chacha)), "an AES peer drops a ChaCha wire as Version({chacha})");
        }
    }
    let (mut a, mut z, mut late) = (under(1, "gcs", 1, &[], 0), under(1, "drone", 2, &[], 0), under(1, "late", 3, &[], 0));
    let mut hear = |from: &mut Block, z: &mut Block, now: u32| unsafe { assert_eq!(mud_publish(from.mem(), from.len(), now, b(b"nav"), b(b"x"), 10), MUD_OK); let w = next_wire(from, now); mud_deliver(z.mem(), z.len(), now, b(b"nav"), b(&w), m(&mut plain), &raw mut inc) };
    unsafe {
        assert_eq!(mud_listen(z.mem(), z.len(), b(b"nav")), MUD_OK);
        assert_eq!((mud_stage(a.mem(), a.len(), b(&k(9))), mud_stage(z.mem(), z.len(), b(&k(9))), mud_rotate(a.mem(), a.len())), (MUD_OK, MUD_OK, MUD_OK));
        assert_eq!((hear(&mut a, &mut z, 1), mud_opened(z.mem(), z.len())), (MUD_EVENT, 2), "the rotated sender's wire opens via Next");
        assert_eq!(mud_rotate(z.mem(), z.len()), MUD_OK);
        assert_eq!((hear(&mut late, &mut z, 2), mud_opened(z.mem(), z.len())), (MUD_EVENT, 1), "a late wire under the old key opens via Previous");
        assert_eq!((mud_retire(a.mem(), a.len()), mud_retire(z.mem(), z.len())), (MUD_OK, MUD_OK));
        assert_eq!((hear(&mut late, &mut z, 3), inc.dropped, inc.status), (MUD_DROPPED, 2, 2), "retired: the old key opens nothing");
        assert_eq!((0..4).map(|w| mud_count(a.mem(), a.len(), w)).collect::<Vec<_>>(), [3, 0, 0, 0], "stage, rotate, retire: three changes");
        assert_eq!((0..4).map(|w| mud_count(z.mem(), z.len(), w)).collect::<Vec<_>>(), [3, 0, 0, 1]);
    }
}

/// An AES block saves and resumes at its shape's length with its tag, and its bytes under the other cipher's tag are refused.
#[test]
fn an_aes_block_resumes_in_its_own_shape_and_retagged_to_the_other_cipher_is_corrupt() {
    let seed = k(7);
    let mut id = mud_id { bytes: [0; 12] };
    for (s, hub, want) in [(&[][..], 0, 3), (&seed[..], 0, 4), (&seed[..], 1, 5)] {
        let n = if hub == 1 { mud_hub_size() } else { mud_size() };
        let (mut live, mut chacha, mut copy) = (under(1, "gcs", 1, s, hub), under(0, "gcs", 1, s, hub), Block::hub());
        unsafe {
            assert_eq!(tag(&mut live), want);
            assert_eq!(mud_save(live.mem(), live.len(), copy.mem(), n - 1), MUD_EMEM, "tag {want}: saved at its shape's size");
            assert_eq!(mud_save(live.mem(), live.len(), copy.mem(), n), MUD_OK);
            assert_eq!(tag(&mut copy), want, "the save keeps the tag");
            if hub == 1 { assert_eq!(mud_resume(copy.mem(), mud_size(), 2), MUD_EMEM, "an AES hub's tag in a leaf's length: refused unread"); }
            assert_eq!(mud_resume(copy.mem(), n - 1, 2), MUD_EMEM);
            assert_eq!(mud_resume(copy.mem(), n, 2), MUD_OK);
            assert_eq!((tag(&mut copy), mud_call(copy.mem(), copy.len(), 0, b(b"nav"), b(b"M"), b(b""), 10, &raw mut id)), (want, MUD_OK), "resumed as itself, and sends");
            for (from, to) in [(&mut live, want - 3), (&mut chacha, want)] {
                assert_eq!(mud_save(from.mem(), from.len(), copy.mem(), n), MUD_OK);
                copy.mem().cast::<u32>().write(to);
                assert_eq!(mud_resume(copy.mem(), n, 3), MUD_ECORRUPT, "tag {want} as {to}: the key checks are the other cipher's");
            }
            assert_eq!((mud_drop(live.mem(), live.len()), mud_drop(chacha.mem(), chacha.len())), (MUD_OK, MUD_OK));
        }
    }
}

/// Every entry that takes a block, in the header's order, on `mem` of `len` bytes with arguments it would accept.
unsafe fn every_entry(mem: *mut c_void, len: usize) -> Vec<i32> {
    let (key, mut copy, mut scratch) = (k(1), Block::hub(), [0u8; 64 + 512]);
    let (mut id, mut inc, mut out) = (mud_id { bytes: [0; 12] }, blank(), mud_out { kind: 0, topic: b(&[]), wire: b(&[]), ttl_secs: 0 });
    vec![
        mud_save(mem, len, copy.mem(), copy.len()), mud_resume(mem, len, 2), mud_renew(mem, len, 3), mud_stage(mem, len, b(&key)), mud_rotate(mem, len), mud_retire(mem, len),
        mud_roster(mem, len, 1, b(&key), b(&[])), mud_revoke(mem, len, 1), mud_opened(mem, len), mud_call(mem, len, 0, b(b"nav"), b(b"M"), b(b""), 10, &raw mut id),
        mud_publish(mem, len, 0, b(b"t"), b(b""), 10), mud_listen(mem, len, b(b"t")), mud_reply(mem, len, 0, &raw const inc, 0, 0, b(b""), b(b""), 1),
        mud_deliver(mem, len, 0, b(b"t"), b(&[1u8; 40]), m(&mut scratch[..512]), &raw mut inc), mud_tick(mem, len, 0, &raw mut inc),
        mud_outgoing(mem, len, 0, m(&mut scratch), &raw mut out), mud_sent(mem, len), mud_drop(mem, len),
    ]
}

/// Every entry takes the block's length and checks it before the shape is read: a hub's tag in a leaf's length, or any
/// shape a byte short, is `MUD_EMEM` and the block is untouched; a save onto any word of its source is `MUD_EINVAL`.
#[test]
fn every_entry_takes_the_block_s_length_and_refuses_a_shape_that_does_not_fit() {
    let seed = k(7);
    for (cipher, s, hub) in [0, 1].into_iter().flat_map(|c| [(c, &[][..], 0), (c, &seed[..], 0), (c, &seed[..], 1)]) {
        let n = if hub == 1 { mud_hub_size() } else { mud_size() };
        let mut blk = under(cipher, "gcs", 1, s, hub);
        unsafe {
            assert_eq!(mud_stage(blk.mem(), n, b(&k(9))), MUD_OK);
            let before = std::slice::from_raw_parts(blk.mem() as *const u8, n).to_vec();
            let short = if hub == 1 { vec![0, 3, mud_size(), n - 1] } else { vec![0, 3, n - 1] };
            for len in short {
                let codes = every_entry(blk.mem(), len);
                assert!(codes.iter().all(|&c| c == MUD_EMEM), "cipher {cipher} hub {hub} len {len}: {codes:?}");
                assert_eq!((mud_count(blk.mem(), len, 0), mud_pressure(blk.mem(), len)), (0, 0));
            }
            assert_eq!(std::slice::from_raw_parts(blk.mem() as *const u8, n), before, "a refused entry wrote nothing");
            if hub == 1 {
                let mut small = vec![0u64; mud_size() / 8]; // a leaf's buffer holding a hub's first bytes
                ptr::copy_nonoverlapping(before.as_ptr(), small.as_mut_ptr().cast::<u8>(), mud_size());
                let codes = every_entry(small.as_mut_ptr().cast::<c_void>(), mud_size());
                assert!(codes.iter().all(|&c| c == MUD_EMEM), "cipher {cipher}: {codes:?}");
                assert_eq!((mud_count(small.as_mut_ptr().cast(), mud_size(), 0), mud_pressure(small.as_mut_ptr().cast(), mud_size())), (0, 0));
                assert_eq!(std::slice::from_raw_parts(small.as_ptr().cast::<u8>(), mud_size()), &before[..mud_size()], "a hub's tag in a leaf's length wrote nothing");
            }
            assert_eq!(mud_count(blk.mem(), n, 0), 1, "at its own length the block is the peer it was");
            assert_eq!(mud_drop(blk.mem(), n), MUD_OK);
        }
    }
    let (n, a, key) = (mud_size(), mud_align(), k(1));
    let mut arena = vec![0u64; 3 * n / 8];
    let base = arena.as_mut_ptr().cast::<u8>();
    unsafe {
        let src = base.add(n).cast::<c_void>();
        let cfg = mud_config { name: b(b"gcs"), key: b(&key), sender: 1, epoch: 1, seed: b(&[]), budget: 0, hub: 0, cipher: 0 };
        assert_eq!(mud_new(src, n, &raw const cfg), MUD_OK);
        let before = std::slice::from_raw_parts(src as *const u8, n).to_vec();
        for off in [a, n - a, n, n + a, 2 * n - a] { assert_eq!(mud_save(src, n, base.add(off).cast(), n), MUD_EINVAL, "a copy at {off} shares a word with its source at {n}"); }
        assert_eq!(std::slice::from_raw_parts(src as *const u8, n), before, "a refused save wrote nothing to its source");
        assert!(std::slice::from_raw_parts(base, 3 * n).iter().enumerate().all(|(i, &x)| (n..2 * n).contains(&i) || x == 0), "nor to its copy");
        let (mut inc, at) = (blank(), src.cast::<u8>().add(n - 512));
        assert_eq!(mud_deliver(src, n, 0, b(b"gcs"), b(&[1u8; 40]), mud_mutbuf { ptr: at, len: 512 }, &raw mut inc), MUD_EINVAL, "a scratch on the block's own bytes");
        assert_eq!(mud_tick(src, n, 0, src.cast()), MUD_EINVAL, "a verdict written over the block");
        let at = |x: &[u8]| mud_buf { ptr: src.cast::<u8>().add(before.windows(x.len()).position(|w| w == x).unwrap()), len: x.len() }; // the block's own copy of `x`
        let (name, own_key, mut id) = (at(b"gcs"), at(&key), mud_id { bytes: [0; 12] });
        assert_eq!(mud_publish(src, n, 0, name, b(b"x"), 60), MUD_EINVAL, "a topic on the block's bytes");
        assert_eq!(mud_publish(src, n, 0, b(b"nav"), name, 60), MUD_EINVAL, "a payload");
        assert_eq!(mud_listen(src, n, name), MUD_EINVAL, "a listen's topic");
        assert_eq!(mud_call(src, n, 0, name, b(b"M"), b(b""), 60, &raw mut id), MUD_EINVAL, "a call's service");
        assert_eq!(mud_stage(src, n, own_key), MUD_EINVAL, "a key");
        let mut call = blank();
        (call.kind, call.topic, call.method, call.reply_to) = (MUD_CALL, b(b"gcs"), b(b"M"), name);
        assert_eq!(mud_reply(src, n, 0, &raw const call, 0, 0, b(b""), b(b""), 1), MUD_EINVAL, "a call's reply_to");
        let cfg = mud_config { name, ..cfg };
        assert_eq!(mud_new(src, n, &raw const cfg), MUD_EINVAL, "a config's name");
        assert_eq!(std::slice::from_raw_parts(src as *const u8, n), before, "and none wrote to it");
        let empty = mud_buf { ptr: name.ptr, len: 0 };
        assert_eq!(mud_publish(src, n, 0, b(b"nav"), empty, 60), MUD_OK, "no bytes share none, wherever they point");
        call.reply_to = b(b"gcs");
        assert_eq!(mud_reply(src, n, 0, &raw const call, 0, 0, b(b""), empty, 1), MUD_OK, "the same call off the block is answered");
        let (mut signed, mut theirs) = (under(0, "gcs", 1, &k(7), 0), [0u8; 32]);
        assert_eq!(mud_public_key(b(&k(7)), theirs.as_mut_ptr()), MUD_OK);
        let mem = signed.mem();
        let line = mud_buf { ptr: mem.cast::<u8>().add(std::slice::from_raw_parts(mem as *const u8, mud_size()).windows(32).position(|w| w == theirs).unwrap()), len: 32 }; // its own public key, in the block
        assert_eq!(mud_roster(mem, mud_size(), 2, line, b(&[])), MUD_EINVAL, "a roster key on the block's bytes");
        assert_eq!(mud_roster(mem, mud_size(), 2, b(&theirs), line), MUD_EINVAL, "a roster's next key");
        assert_eq!(mud_roster(mem, mud_size(), 2, b(&theirs), b(&[])), MUD_OK, "and a copy of it is a line");
        for off in [0, 2 * n] { assert_eq!(mud_save(src, n, base.add(off).cast(), n), MUD_OK, "adjacent at {off}: no shared byte"); }
    }
}

/// ttl, uptime, sender, epoch, seq, budget and cipher each at 0, 1, `u32::MAX` − 1 and `u32::MAX`, all 4⁷ combinations
/// at once (a budget above 0 brings a seed, so a signed peer): `mud_new` and every loop entry answer a code, and at the
/// edge of uptime an expiry saturates rather than wrapping, so a call made a second before the end lives until it.
#[test]
fn every_u32_field_at_its_edge_at_once_answers_a_code_and_uptime_saturates() {
    const EDGE: [u32; 4] = [0, 1, u32::MAX - 1, u32::MAX];
    let (key, seed, mut blk, start) = (k(1), k(7), Block::new(), std::time::Instant::now());
    let (mut scratch, mut built) = ([0u8; 64 + 512], 0);
    for i in 0..4usize.pow(7) {
        let at: [u32; 7] = core::array::from_fn(|d| EDGE[i / 4usize.pow(d as u32) % 4]);
        let [ttl, now, sender, epoch, seq, budget, cipher] = at;
        let cfg = mud_config { name: b(b"gcs"), key: b(&key), sender, epoch, seed: b(if budget > 0 { &seed } else { &[] }), budget, hub: 0, cipher };
        let (mut id, mut inc, mut out) = (mud_id { bytes: [0; 12] }, blank(), mud_out { kind: 0, topic: b(&[]), wire: b(&[]), ttl_secs: 0 });
        let (mem, len) = (blk.mem(), blk.len());
        let mut codes = Vec::new();
        unsafe {
            std::ptr::write_bytes(mem.cast::<u8>(), 0, len);
            codes.push(mud_new(mem, len, &raw const cfg));
            built += usize::from(codes[0] == MUD_OK);
            codes.push(mud_call(mem, len, now, b(b"gcs"), b(b"M"), b(b""), ttl, &raw mut id));
            let alive = codes[1] == MUD_OK && now == u32::MAX - 1;
            if alive { assert_eq!(mud_tick(mem, len, now, &raw mut inc), MUD_NONE, "{at:?}: a ttl past the end of uptime saturates to it"); }
            (inc.reply_to, inc.ttl_secs, inc.seq) = (b(b"gcs"), ttl, seq);
            codes.push(mud_publish(mem, len, now, b(b"t"), b(b"x"), ttl));
            codes.push(mud_reply(mem, len, now, &raw const inc, seq, seq, b(b""), b(b"y"), 1));
            for _ in 0..4 {
                let c = mud_outgoing(mem, len, now, m(&mut scratch), &raw mut out);
                let wire = if c == MUD_SEND { std::slice::from_raw_parts(out.wire.ptr, out.wire.len).to_vec() } else { Vec::new() };
                codes.push(c);
                if c > 0 { mud_sent(mem, len); }
                if c == MUD_SEND { codes.push(mud_deliver(mem, len, now, b(b"gcs"), b(&wire), m(&mut scratch[..512]), &raw mut inc)); }
            }
            assert!(!alive || codes.contains(&MUD_SEND), "{at:?}: the call leaves, not dropped at the exit as expired");
            if alive { assert_eq!(mud_tick(mem, len, u32::MAX, &raw mut inc), MUD_EXPIRED, "{at:?}: and expires at the end"); }
            codes.push(mud_tick(mem, len, now, &raw mut inc));
            codes.push(mud_renew(mem, len, epoch.wrapping_add(1)));
        }
        assert!(codes.iter().all(|c| (-9..=7).contains(c)), "{at:?}: {codes:?}");
    }
    eprintln!("{} combinations, {built} peers built, in {:?}", 4usize.pow(7), start.elapsed());
}

/// The host mistakes the contract names, as `tests/wrong.c` commits them in C through the header alone, each answering what
/// every skill's Verify table gives: a refusal is a code, a ttl out of range is clamped, a reply to a call it does not
/// have is a verdict. These lines are the table. End to end.
#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn every_host_mistake_answers_the_code_the_skills_name() {
    const TABLE: [&str; 24] = [
        "mud_resume at the saved epoch: MUD_EEPOCH",
        "mud_renew below the current epoch: MUD_EEPOCH",
        "a send from sender 0: MUD_EUNPROVISIONED",
        "a key of 32 equal bytes: MUD_EINVAL",
        "a staged key of 32 equal bytes: MUD_EINVAL",
        "a seed of 32 equal bytes: MUD_EINVAL",
        "a block one byte short of mud_size(): MUD_EMEM",
        "a hub at a leaf's length: MUD_EMEM",
        "a block longer than its shape: silent, MUD_OK",
        "hub 1 without a seed: MUD_EINVAL",
        "a saved copy sent from before mud_resume: MUD_ECORRUPT",
        "a saved copy retagged to the other cipher, resumed: MUD_ECORRUPT",
        "ttl 0: clamped to 1",
        "ttl past a week: clamped to 604800",
        "a topic over MUD_MAX_TOPIC: MUD_ETOOLARGE",
        "a name over MUD_MAX_TOPIC: MUD_ETOOLARGE",
        "a payload over the ceiling: MUD_ETOOLARGE",
        "an empty name: MUD_EINVAL",
        "an empty service: MUD_EINVAL",
        "an empty topic: MUD_EINVAL",
        "a reply to a call this peer does not have: silent, MUD_DROPPED, dropped 5",
        "a 17th call pending: MUD_EFULL",
        "a 17th item queued undrained: MUD_EFULL",
        "the 33rd send in one second: MUD_ERATE",
    ];
    let dir = env!("CARGO_MANIFEST_DIR");
    let root = format!("{dir}/../..");
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(&root).status().unwrap().success());
    let out = std::env::temp_dir().join("mudrpc-wrong");
    std::fs::create_dir_all(&out).unwrap();
    let mut cc = std::process::Command::new("cc");
    cc.args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I", dir, &format!("{dir}/tests/wrong.c"), &format!("{root}/target/debug/libmudrpc.a"), "-o", out.join("wrong").to_str().unwrap()]);
    if cfg!(target_os = "linux") { cc.args(["-lpthread", "-ldl", "-lm"]); }
    let built = cc.output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let run = std::process::Command::new(out.join("wrong")).output().unwrap();
    assert!(run.status.success(), "wrong.c exited {:?}", run.status.code());
    assert_eq!(String::from_utf8_lossy(&run.stdout).lines().collect::<Vec<_>>(), TABLE);
}

/// The product's UDP through the ABI: a default build refuses a public address, and two sockets on loopback carry a wire.
#[test]
fn udp_through_the_abi_refuses_a_public_address_and_carries_a_wire() {
    unsafe {
        assert_eq!(mud_internet(), 0, "the default build, every release's, cannot go public");
        let mut code = 0;
        for (bind, peers, want) in [("0.0.0.0:0", "127.0.0.1:9", MUD_EINVAL), ("8.8.8.8:5000", "127.0.0.1:9", MUD_EINVAL), ("127.0.0.1:0", "1.1.1.1:5000", MUD_EINVAL), ("not an address", "127.0.0.1:9", MUD_EINVAL), ("127.0.0.1:0", "127.0.0.1:x", MUD_EINVAL), ("127.0.0.1:0", "", MUD_EINVAL)] {
            assert!(mud_udp_new(b(bind.as_bytes()), b(peers.as_bytes()), &raw mut code).is_null(), "{bind} {peers}");
            assert_eq!(code, want, "{bind} {peers}");
        }
        let probe = mud_udp_new(b(b"127.0.0.1:0"), b(b"127.0.0.1:9"), &raw mut code);
        let port = mud_udp_port(probe);
        assert!(mud_udp_new(b(format!("127.0.0.1:{port}").as_bytes()), b(b"127.0.0.1:9"), &raw mut code).is_null() && code == MUD_EFULL, "a port taken is the system's refusal");
        mud_udp_free(probe);
        let z = std::ptr::null_mut::<mud_udp>();
        let (a, to) = (0..50).find_map(|_| { // ports chosen free, chosen again while another test takes one between the choosing and the binding
            let (pa, pt) = { let (x, y) = (std::net::UdpSocket::bind("127.0.0.1:0").unwrap(), std::net::UdpSocket::bind("127.0.0.1:0").unwrap()); (x.local_addr().unwrap().port(), y.local_addr().unwrap().port()) };
            let to = mud_udp_new(b(format!("127.0.0.1:{pt}").as_bytes()), b(format!("127.0.0.1:{pa}").as_bytes()), &raw mut code);
            if to.is_null() { return None; }
            let a = mud_udp_new(b(format!("127.0.0.1:{pa}").as_bytes()), b(format!("127.0.0.1:{pt}, 127.0.0.1:9").as_bytes()), &raw mut code);
            if a.is_null() { mud_udp_free(to); return None; }
            Some((a, to))
        }).expect("two free loopback ports");
        assert_eq!((mud_udp_listen(to, b(b"nav")), mud_udp_listen(to, b(b"")), mud_udp_listen(z, b(b"nav"))), (MUD_OK, MUD_EINVAL, MUD_EINVAL));
        assert_eq!(mud_udp_send(a, b(b"other"), b(b"wire")), MUD_OK, "a topic `to` does not listen to");
        assert_eq!(mud_udp_send(a, b(b"nav"), b(b"wire")), MUD_OK, "one of two peers took it");
        assert_eq!((mud_udp_send(a, b(b""), b(b"w")), mud_udp_send(a, b(&[b't'; 65]), b(b"w")), mud_udp_send(z, b(b"nav"), b(b"w"))), (MUD_EINVAL, MUD_EINVAL, MUD_EINVAL));
        let mut scratch = vec![0u8; 64 + 512];
        let (mut t, mut w) = (blank_buf(), blank_buf());
        assert_eq!(mud_udp_poll(to, m(&mut [0u8; 8]), &raw mut t, &raw mut w), MUD_EINVAL, "scratch holds a topic and a wire");
        let mut got = MUD_NONE;
        for _ in 0..200 { got = mud_udp_poll(to, m(&mut scratch), &raw mut t, &raw mut w); if got == 1 { break; } std::thread::sleep(std::time::Duration::from_millis(5)); }
        assert_eq!(got, 1);
        assert_eq!((std::slice::from_raw_parts(t.ptr, t.len), std::slice::from_raw_parts(w.ptr, w.len)), (&b"nav"[..], &b"wire"[..]));
        assert_eq!((mud_udp_poll(to, m(&mut scratch), &raw mut t, &raw mut w), mud_udp_port(z)), (MUD_NONE, MUD_EINVAL));
        mud_udp_free(a);
        mud_udp_free(to);
        mud_udp_free(z);
    }
}

const fn blank_buf() -> mud_buf { mud_buf { ptr: std::ptr::null(), len: 0 } }
