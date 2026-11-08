//! The instruction counts SPEC.md layer 1 states, one line `name instructions` each, after the checks: built with
//! `--features icount` and run with `-icount shift=0` on mps2-an386 alone, since RISC-V's `virt` has no `SysTick`. QEMU's
//! clock then moves 1 ns an instruction and the board's `SysTick` runs at 25 MHz, so a tick is 40 instructions; the NOP
//! loop prints first and is the scale gate: 1 002 000 under `-icount`, anything else exits 22. Exits 21 if `verify` takes more
//! than `VERIFY_AT_MOST`.
#[cfg(not(target_arch = "arm"))]
compile_error!("the counts are the Cortex-M4's: SysTick is a Cortex-M timer");
use super::{exit, next, pair, say, signed, two, Dropped, Gcm, Incoming, Key, Sealer, K};
use core::hint::black_box;
use mudrpc_auth::ed25519::{public_key, sign, verify};
use mudrpc_auth::sha512::sha512;
use mudrpc_core::seal::{poly1305, ChaCha20Poly1305, Cipher, Error, Slots};
use mudrpc_gcm::Aes256Gcm;

const VERIFY_AT_MOST: u32 = 5_500_000;

/// `SysTick`'s control, reload and current value registers.
const SYST: *mut u32 = 0xE000_E010 as *mut u32;

/// The instructions `f` takes, printed under `name`.
#[allow(clippy::cast_possible_truncation)] // a digit
fn count<R>(name: &str, f: impl FnOnce() -> R) -> (u32, R) {
    // SAFETY: SysTick's current value register, which every Cortex-M maps there
    let cvr = || unsafe { SYST.add(2).read_volatile() };
    let a = cvr();
    let r = black_box(f());
    let n = (a.wrapping_sub(cvr()) & 0x00FF_FFFF) * 40;
    let (mut d, mut i, mut v) = ([0u8; 10], 10, n);
    while i == 10 || v > 0 { i -= 1; d[i] = b'0' + (v % 10) as u8; v /= 10; }
    say(&[name.as_bytes(), b" ", d.get(i..).unwrap_or(&[])]);
    (n, r)
}

/// A signed unary call's 84 bytes of plaintext sealed and opened: the 113-byte wire with its version byte.
fn seal_open<S: Sealer>(names: [&str; 2], mut s: S) {
    let (ad, mut plain, mut wire, mut out) = ([S::VERSION, b'n', b'a', b'v'], [5u8; 84], [0u8; 112], [0u8; 112]);
    if count(names[0], || s.seal(&[1; 12], &ad, &mut plain, 84, &mut wire)).1 != Ok(112) || count(names[1], || s.open(&ad, &wire, &mut out)).1.is_err() { exit(1); }
}

/// Slots holding the fleet key alone, or a key rotated out, the current one and one staged.
fn slots<C: Cipher>(n: usize) -> Slots<C> {
    let mut s = Slots::new(K);
    if n == 3 { s.stage([8; 32]); s.rotate(); s.stage([9; 32]); }
    s
}

/// A junk wire of a signed unary call's length, its version byte the receiver's, through `deliver`: a tag check a slot.
fn junk<C: Cipher + 'static>(name: &str, n: usize) {
    let Some((_, b)) = pair((slots::<C>(1), slots::<C>(n))) else { exit(1) };
    let (mut w, mut buf) = ([0x5a; 113], [0u8; 512]);
    w[0] = C::VERSION;
    if count(name, || b.deliver(5, "nav", &w, &mut buf)).1 != Incoming::Dropped(Dropped::Open(Error::Tag)) { exit(1); }
}

/// A signed unary call delivered: one tag, one signature.
fn deliver_signed<S: Sealer + 'static>(name: &str, new: fn([u8; 32]) -> S) {
    let Some((a, b)) = signed(two(new)).and_then(pair) else { exit(1) };
    let (mut w, mut buf) = ([0u8; 512], [0u8; 512]);
    let n = if a.call(5, "nav", "Ping", b"ping", 30).is_ok() { next(a, &mut w) } else { 0 };
    if !matches!(count(name, || b.deliver(5, "nav", &w[..n], &mut buf)).1, Incoming::Call(_)) { exit(1); }
}

pub fn run() {
    // SAFETY: SysTick's reload, current value and control registers: the largest reload, cleared, on the processor clock
    unsafe { SYST.add(1).write_volatile(0x00FF_FFFF); SYST.add(2).write_volatile(0); SYST.write_volatile(5) };
    let (n, ()) = count("nop_1000000", || for _ in 0..1000 {
        // SAFETY: a thousand NOPs, nothing touched
        unsafe { core::arch::asm!(".rept 1000", "nop", ".endr", options(nomem, nostack, preserves_flags)) }
    });
    if n != 1_002_000 { say(&[b"FAIL scale: not instructions; run under -icount shift=0"]); exit(22); }
    let (seed, msg) = ([1u8; 32], [5u8; 113]);
    let pk = count("public_key", || public_key(&seed)).1;
    let sig = count("sign", || sign(&seed, &pk, &[&msg])).1;
    let (n, ok) = count("verify", || verify(&pk, &[&msg], &sig));
    if !ok || n > VERIFY_AT_MOST { say(&[b"FAIL verify above the bound"]); exit(21); }
    count("sha512_113", || sha512(&[&msg]));
    count("poly1305_113", || poly1305(&K, &msg));
    count("ghash_113", || mudrpc_gcm::ghash::ghash(&[9; 16], &[], &msg));
    seal_open(["key_seal", "key_open"], Key::new(K));
    seal_open(["gcm_seal", "gcm_open"], Gcm::new(K));
    junk::<ChaCha20Poly1305>("key_junk_1", 1);
    junk::<ChaCha20Poly1305>("key_junk_3", 3);
    junk::<Aes256Gcm>("gcm_junk_1", 1);
    junk::<Aes256Gcm>("gcm_junk_3", 3);
    deliver_signed("deliver_signed", Key::new);
    deliver_signed("gcm_deliver_signed", Gcm::new);
}
