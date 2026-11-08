//! The no-panic tripwire (SPEC.md, the bar): core, auth, gcm and the ABI without std linked into a bare binary whose panic
//! handler, the ABI's, calls C's `abort`, which nothing here defines, so if any panic site survives optimisation the link
//! fails. Every entry point is called with inputs the optimiser cannot see through, so nothing is dropped as unreachable.
//! Built per embedded target with `cargo build --manifest-path tripwire/Cargo.toml --profile tripwire --target <t>`; never
//! run. Overflow checks are off in that profile, as in a product's release build: the arithmetic that must not depend on
//! the profile is pinned by the tests, not here.
#![no_std]
#![no_main]
#![warn(clippy::pedantic, clippy::nursery)]
use core::hint::black_box;
use mudrpc_auth::signed::Signed;
use mudrpc_core::rpc::{Incoming, Outgoing, Peer};
use mudrpc_core::seal::Key;
use mudrpc_gcm::Gcm;

type Plain = Peer<Key, 512, 16, 64, 16, 16>;
type Auth = Peer<Signed<Key, 16>, 512, 16, 64, 16, 16>;
type Aes = Peer<Gcm, 512, 16, 64, 16, 16>;
type AesAuth = Peer<Signed<Gcm, 16>, 512, 16, 64, 16, 16>;

fn exercise<S: mudrpc_core::seal::Sealer>(p: &mut Peer<S, 512, 16, 64, 16, 16>) -> u32 {
    let now = black_box(5u32);
    let mut acc = 0u32;
    if p.call(now, black_box("nav"), black_box("M"), black_box(&[1u8; 10]), black_box(30)).is_ok() { acc += 1; }
    let _ = p.publish(now, black_box("nav"), black_box(&[2u8; 20]), black_box(30));
    let _ = p.listen(black_box("t"));
    while let Some(o) = p.outgoing(now) {
        if let Outgoing::Send { wire, .. } = o { acc = acc.wrapping_add(u32::try_from(wire.len()).unwrap_or(u32::MAX)); }
        p.sent();
    }
    let wire = black_box([7u8; 100]);
    let mut plain = [0u8; 512];
    if let Incoming::Call(c) = p.deliver(now, black_box("nav"), black_box(&wire[..]), black_box(&mut plain[..])) {
        let _ = p.reply(now, &c, black_box(0), black_box(0), "", black_box(&[1u8; 3]), black_box(true));
    }
    if p.tick(now).is_some() { acc += 1; }
    let _ = p.resume(black_box(2));
    let _ = p.renew(black_box(3));
    acc.wrapping_add(p.pressure()).wrapping_add(p.opened() as u32)
}

/// The ABI's peer entries as a firmware calls them, on blocks and lengths the optimiser cannot see.
fn abi() -> i32 {
    use mudrpc::{mud_buf, mud_config, mud_id, mud_mutbuf};
    const fn b(s: &[u8]) -> mud_buf { mud_buf { ptr: s.as_ptr(), len: s.len() } }
    let (mut block, mut copy) = ([0u64; mudrpc::mud_size() / 8 + 1], [0u64; mudrpc::mud_size() / 8 + 1]);
    let (mem, out, len) = (black_box(block.as_mut_ptr().cast()), black_box(copy.as_mut_ptr().cast()), black_box(mudrpc::mud_size()));
    let (key, seed, wire, mut scratch) = (black_box([3u8; 32]), black_box([4u8; 32]), black_box([7u8; 100]), [0u8; 64 + 512]);
    let s = mud_mutbuf { ptr: scratch.as_mut_ptr(), len: black_box(scratch.len()) };
    let config = mud_config { name: b(b"gcs"), key: b(&key), sender: black_box(1), epoch: black_box(1), seed: b(&seed), budget: black_box(100), hub: black_box(0), cipher: black_box(1) };
    let mut id = mud_id { bytes: [0; 12] };
    unsafe { // SAFETY: every pointer is this frame's, at its length, as the header asks; the verdicts are plain integers and pointers, so zeroed is valid
        let (mut o, mut i) = (core::mem::zeroed(), core::mem::zeroed());
        [
            mudrpc::mud_new(mem, len, &raw const config),
            mudrpc::mud_call(mem, len, black_box(5), b(b"nav"), b(b"M"), b(&[1; 10]), black_box(30), &raw mut id),
            mudrpc::mud_outgoing(mem, len, black_box(5), s, &raw mut o),
            mudrpc::mud_deliver(mem, len, black_box(5), b(b"nav"), b(&wire), s, &raw mut i),
            mudrpc::mud_save(mem, len, out, len),
            mudrpc::mud_resume(out, len, black_box(2)),
        ].iter().fold(0, |a, c| a ^ c)
    }
}

#[no_mangle]
pub extern "C" fn _start() -> u32 {
    let mut acc = abi().cast_unsigned();
    if let Ok(mut p) = Plain::new(black_box("gcs"), Key::new(black_box([1; 32])), black_box(1), black_box(1)) {
        acc = acc.wrapping_add(exercise(&mut p));
        p.sealer_mut().stage(black_box([3; 32]));
        p.sealer_mut().rotate();
        p.sealer_mut().retire();
    }
    if let Ok(mut p) = Auth::new(black_box("gcs"), Signed::new(Key::new(black_box([1; 32])), black_box([2; 32]), black_box(100)), black_box(1), black_box(1)) {
        let _ = p.sealer_mut().roster(black_box(2), black_box([4; 32]), black_box(None));
        acc = acc.wrapping_add(exercise(&mut p));
    }
    if let Ok(mut p) = Aes::new(black_box("gcs"), Gcm::new(black_box([1; 32])), black_box(1), black_box(1)) {
        acc = acc.wrapping_add(exercise(&mut p));
        p.sealer_mut().stage(black_box([3; 32]));
        p.sealer_mut().rotate();
        p.sealer_mut().retire();
    }
    if let Ok(mut p) = AesAuth::new(black_box("gcs"), Signed::new(Gcm::new(black_box([1; 32])), black_box([2; 32]), black_box(100)), black_box(1), black_box(1)) {
        let _ = p.sealer_mut().roster(black_box(2), black_box([4; 32]), black_box(None));
        acc = acc.wrapping_add(exercise(&mut p));
    }
    acc
}
