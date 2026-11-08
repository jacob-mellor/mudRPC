//! The target in CI (SPEC.md, the bar): core, auth and gcm built as the tripwire builds them and run under QEMU on the
//! target's instruction set, the in-process checks one line each, exit 0 or the failed check's number: on Arm through
//! semihosting, on RISC-V through `virt`'s UART and test device. Bare: a hand-written head and reset handler, no crates.
//! Built from `qemu/`, whose `.cargo/config.toml` names each board's linker script, with `cargo build --profile tripwire
//! --target thumbv7em-none-eabihf` and run with `qemu-system-arm -M mps2-an386 -cpu cortex-m4 -nographic
//! -semihosting-config enable=on,target=native -kernel target/thumbv7em-none-eabihf/tripwire/qemu`; RISC-V is
//! `riscv32imc-unknown-none-elf` on `qemu-system-riscv32 -M virt -bios none -nographic`. With `--features icount`, run
//! under `-icount shift=0` on the Cortex-M4, the checks are followed by the instruction counts (`count.rs`). The last three
//! are the host's scrub probes on the board: the stack below a check's frame searched after the operation.
#![no_std]
#![no_main]
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(clippy::many_single_char_names)] // as the peer tests name them: `a` and `b` the peers, `w` a wire, `c` a call
use core::hint::black_box;
use core::mem::MaybeUninit;
use mudrpc_auth::ed25519::{public_key, sign, verify};
use mudrpc_auth::sha512::sha512;
use mudrpc_auth::signed::Signed;
use mudrpc_core::rpc::{Dropped, Error, Incoming, Outgoing, Peer};
use mudrpc_core::seal::{self, block, Key, Sealer};
use mudrpc_gcm::{aes, Gcm};
#[cfg(feature = "icount")]
mod count;

type P<S> = Peer<S, 512, 16, 64, 16, 16>;
const K: [u8; 32] = [7; 32];

/// Arm's semihosting (`bkpt 0xAB` on M-profile): the operation in r0, its argument or parameter block in r1.
#[cfg(target_arch = "arm")]
fn semi(op: u32, arg: usize) {
    // SAFETY: QEMU services the breakpoint and reads only the block at `arg`, which the caller keeps alive across the call
    unsafe { core::arch::asm!("bkpt #0xAB", inout("r0") op => _, in("r1") arg, options(nostack)) };
}

/// The parts and a newline as one NUL-terminated line, at most 62 bytes before the newline: Arm's `SYS_WRITE0`, or
/// byte by byte to the 16550 at `0x1000_0000`, each once the line status register (offset 5) shows the holding register empty.
fn say(parts: &[&[u8]]) {
    let mut line = [0u8; 64];
    for (d, s) in line.iter_mut().take(63).zip(parts.iter().flat_map(|p| p.iter()).chain(b"\n")) { *d = *s; }
    #[cfg(target_arch = "arm")]
    semi(0x04, line.as_ptr() as usize);
    #[cfg(target_arch = "riscv32")]
    for b in line.into_iter().take_while(|&b| b != 0) {
        // SAFETY: the UART's data and line status registers, which `virt` maps at these addresses
        unsafe { while (0x1000_0005 as *const u8).read_volatile() & 0x20 == 0 {} (0x1000_0000 as *mut u8).write_volatile(b) }
    }
}

/// Arm's `SYS_EXIT_EXTENDED`, `ADP_Stopped_ApplicationExit` and the code, since on the 32-bit interface only it carries
/// one; or `virt`'s test device at `0x10_0000`, `0x5555` to pass and `code << 16 | 0x3333` to fail with the code.
fn exit(code: u32) -> ! {
    #[cfg(target_arch = "arm")]
    semi(0x20, [0x2_0026, code].as_ptr() as usize);
    // SAFETY: the test device's one register, which `virt` maps there
    #[cfg(target_arch = "riscv32")]
    unsafe { (0x10_0000 as *mut u32).write_volatile(if code == 0 { 0x5555 } else { code << 16 | 0x3333 }) };
    loop { core::hint::spin_loop() }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { say(&[b"FAIL panic"]); exit(255) }

#[no_mangle]
extern "C" fn fault() -> ! { say(&[b"FAIL fault"]); exit(254) }

// The head, first in the image: Arm's initial stack pointer before the vector table; RISC-V's first instructions, where
// `virt` jumps, which set the stack and point every trap at `fault` through an entry four-byte aligned, as `mtvec` needs.
#[cfg(target_arch = "arm")]
core::arch::global_asm!(".section .head, \"a\"", ".word _stack_top");
#[cfg(target_arch = "riscv32")]
core::arch::global_asm!(".section .head, \"ax\"", "la sp, _stack_top", "la t0, 1f", "csrw mtvec, t0", "j reset", ".balign 4", "1: j fault");

/// Reset and the fourteen exceptions after it, after the head.
#[cfg(target_arch = "arm")]
#[link_section = ".vectors"]
#[used]
static VECTORS: [unsafe extern "C" fn() -> !; 15] = {
    let mut v = [fault as unsafe extern "C" fn() -> !; 15];
    v[0] = reset;
    v
};

#[no_mangle]
unsafe extern "C" fn reset() -> ! {
    extern "C" { static mut _sbss: u32; static mut _ebss: u32; static mut _sdata: u32; static mut _edata: u32; static _sidata: u32; }
    // SAFETY: nothing else runs yet; the linker script bounds and word-aligns both ranges; CPACR (0xE000_ED88) is every FPU-bearing Cortex-M's
    unsafe {
        let mut b = &raw mut _sbss;
        while b < &raw mut _ebss { b.write_volatile(0); b = b.add(1); }
        let (mut d, mut s) = (&raw mut _sdata, &raw const _sidata);
        while d < &raw mut _edata { d.write_volatile(s.read()); (d, s) = (d.add(1), s.add(1)); }
        #[cfg(target_arch = "arm")]
        (0xE000_ED88 as *mut u32).write_volatile(0xF << 20); // the FPU on, for a hard-float build that touches it
    }
    main()
}

/// Two peers' cells in `.bss`: a peer is 12 to 14 KB, kept off the stack. Each check takes both afresh.
const HALF: usize = 16 * 1024;
#[repr(C, align(8))]
struct Arena([MaybeUninit<u8>; 2 * HALF]);
static mut ARENA: Arena = Arena([MaybeUninit::uninit(); 2 * HALF]);

/// Two sealers from one fleet key.
fn two<S>(new: fn([u8; 32]) -> S) -> (S, S) { (new(K), new(K)) }

/// "gcs", sender 1, and "nav", sender 2, under the two sealers.
fn pair<S: Sealer + 'static>((a, b): (S, S)) -> Option<(&'static mut P<S>, &'static mut P<S>)> {
    const { assert!(size_of::<P<S>>() <= HALF && align_of::<P<S>>() <= 8) }
    let (a, b) = (P::new("gcs", a, 1, 1).ok()?, P::new("nav", b, 2, 1).ok()?);
    // SAFETY: the arena's two halves, each aligned and large enough (asserted above); one check runs at a time
    let (ca, cb) = unsafe { let c = (&raw mut ARENA).cast::<MaybeUninit<P<S>>>(); (&mut *c, &mut *c.byte_add(HALF)) };
    Some((ca.write(a), cb.write(b)))
}

/// Each sealer signed, each sender on the other's roster.
fn signed<S: Sealer>((a, b): (S, S)) -> Option<(Signed<S, 16>, Signed<S, 16>)> {
    let (mut a, mut b) = (Signed::new(a, [1; 32], 100), Signed::new(b, [2; 32], 100));
    let (ka, kb) = (a.public_key(), b.public_key());
    (a.roster(2, kb, None).is_ok() && b.roster(1, ka, None).is_ok()).then_some((a, b))
}

/// The next sealed wire `p` queues, past its subscriptions, into `w`; its length.
fn next<S: Sealer>(p: &mut P<S>, w: &mut [u8; 512]) -> usize {
    while let Some(o) = p.outgoing(5) {
        let n = if let Outgoing::Send { wire, .. } = o { w.iter_mut().zip(wire).for_each(|(d, s)| *d = *s); wire.len() } else { 0 };
        p.sent();
        if n > 0 { return n; }
    }
    0
}

/// One peer calls, the other delivers and replies, the first delivers the reply and sees its id.
fn round<S: Sealer + 'static>(sealers: (S, S)) -> bool {
    let Some((a, b)) = pair(sealers) else { return false };
    let (mut w, mut buf) = ([0u8; 512], [0u8; 512]);
    let Ok(id) = a.call(5, "nav", "Ping", b"ping", 30) else { return false };
    let n = next(a, &mut w);
    let Incoming::Call(c) = b.deliver(5, "nav", &w[..n], &mut buf) else { return false };
    if b.reply(5, &c, 0, 0, "", b"pong", true).is_err() { return false; }
    let n = next(b, &mut w);
    matches!(a.deliver(5, "gcs", &w[..n], &mut buf), Incoming::Reply { id: got, bytes: b"pong", .. } if got == id)
}

/// The same wire twice: the window refuses the second.
fn replay() -> bool {
    let Some((a, b)) = pair(two(Key::new)) else { return false };
    let (mut w, mut buf) = ([0u8; 512], [0u8; 512]);
    let n = if a.call(5, "nav", "Ping", b"", 30).is_ok() { next(a, &mut w) } else { 0 };
    matches!(b.deliver(5, "nav", &w[..n], &mut buf), Incoming::Call(_)) && b.deliver(5, "nav", &w[..n], &mut buf) == Incoming::Dropped(Dropped::Duplicate)
}

/// A block saved as `mud_save` saves one, its bytes copied and the copy disarmed: no send until it is resumed.
fn resume() -> bool {
    let Some((a, b)) = pair(two(Key::new)) else { return false };
    // SAFETY: a peer's bytes are its save file (`repr(C)`, no pointers); `b` is another cell of the same type
    unsafe { core::ptr::copy_nonoverlapping(&raw const *a, &raw mut *b, 1) };
    b.disarm();
    b.call(5, "nav", "Ping", b"", 30) == Err(Error::Corrupt) && b.resume(2).is_ok() && b.call(5, "nav", "Ping", b"", 30).is_ok()
}

const fn hex<const N: usize>(s: &str) -> [u8; N] {
    const fn nib(c: u8) -> u8 { if c > b'9' { c - b'a' + 10 } else { c - b'0' } }
    let (s, mut b, mut i) = (s.as_bytes(), [0; N], 0);
    while i < N { b[i] = nib(s[2 * i]) << 4 | nib(s[2 * i + 1]); i += 1; }
    b
}

/// A Wycheproof case: key, IV, AD, message, and the wire, IV ‖ ciphertext ‖ tag.
struct Case<const M: usize, const W: usize> { key: [u8; 32], iv: [u8; 12], ad: [u8; 8], msg: [u8; M], wire: [u8; W] }
type Seal = fn(&[u8; 32], &[u8; 12], &[u8], &[u8], &mut [u8]) -> Result<usize, seal::Error>;
type Open = fn(&[u8; 32], &[u8], &[u8], &mut [u8]) -> Result<usize, seal::Error>;

/// Sealed byte for byte as the case says, opened back, and one ciphertext bit flipped is the tag's refusal.
fn aead<const M: usize, const W: usize>(c: &Case<M, W>, seal: Seal, open: Open) -> bool {
    let (mut w, mut p) = ([0u8; W], [0u8; M]);
    let ok = seal(&c.key, &c.iv, &c.ad, &c.msg, &mut w) == Ok(W) && w == c.wire && open(&c.key, &c.ad, &w, &mut p) == Ok(M) && p == c.msg;
    w[12] ^= 1;
    ok && open(&c.key, &c.ad, &w, &mut p) == Err(seal::Error::Tag)
}

/// ChaCha20-Poly1305 tcId 5, AES-256-GCM tcId 91.
const CHACHA: Case<1, 29> = Case {
    key: hex("46f0254965f769d52bdb4a70b443199f8ef207520d1220c55e4b70f0fda620ee"), iv: hex("ab0dca716ee051d2782f4403"), ad: hex("91ca6c592cbcca53"),
    msg: hex("51"), wire: hex("ab0dca716ee051d2782f4403c4168310ca45b1f7c66cad4e99e43f72b9"),
};
const GCM: Case<10, 38> = Case {
    key: hex("92ace3e348cd821092cd921aa3546374299ab46209691bc28b8752d17f123c20"), iv: hex("00112233445566778899aabb"), ad: hex("00000000ffffffff"),
    msg: hex("00010203040506070809"), wire: hex("00112233445566778899aabbe27abdd2d2a53d2f136b9a4a2579529301bcfb71c78d4060f52c"),
};

/// RFC 8032 §7.1, TEST 1: the empty message.
const SEED: [u8; 32] = hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
const PK: [u8; 32] = hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
const SIG: [u8; 64] = hex("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b");

/// The stack a scrub check searches below its frame: every operation's frames fit in it with room.
const SCAN: usize = 32 * 1024;

/// Zeroes more than `SCAN` of the stack below the caller's frame, as the host probes' `scrub` does.
#[inline(never)]
#[allow(clippy::large_stack_arrays, clippy::missing_const_for_fn)] // the frame is the zeroing, at run time
fn clear() { black_box(&mut [0u8; SCAN + SCAN / 2]); }

/// A frame that leaves `n` on the stack: the control that proves the search sees.
#[inline(never)]
fn plant(n: &[u8]) { let mut spot = [0u8; 4096]; spot[2048..].iter_mut().zip(n).for_each(|(d, s)| *d = *s); black_box(&mut spot); }

/// The first of the needles in the `SCAN` bytes below `top`; inlined, so its frame is its caller's and nothing below `top`
/// is written between the operation and the search.
#[inline(always)]
#[allow(clippy::inline_always)] // as above
fn find(top: usize, needles: &[(&'static str, &[u8])]) -> Option<&'static str> {
    // SAFETY: the free stack below a live frame, which the linker script keeps in RAM, read a byte at a time
    let at = |i: usize| unsafe { ((top - SCAN + i) as *const u8).read_volatile() };
    needles.iter().find(|(_, n)| (0..=SCAN - n.len()).any(|i| n.iter().enumerate().all(|(j, b)| at(i + j) == *b))).map(|(name, _)| *name)
}

/// A host scrub probe (core/, auth/ and gcm/tests/scrub.rs) on the board: the stack below this frame cleared and found
/// clean, the first needle planted and found, cleared again, `op` run, and no needle left; one found is named.
#[inline(never)]
fn scrub(needles: &[(&'static str, &[u8])], op: &mut dyn FnMut() -> bool) -> bool {
    let top: usize;
    // SAFETY: copies the stack pointer; nothing is read or written
    #[cfg(target_arch = "arm")]
    unsafe { core::arch::asm!("mov {}, sp", out(reg) top, options(nomem, nostack, preserves_flags)) };
    // SAFETY: as above
    #[cfg(target_arch = "riscv32")]
    unsafe { core::arch::asm!("mv {}, sp", out(reg) top, options(nomem, nostack, preserves_flags)) };
    clear();
    let clean = find(top, needles).is_none();
    plant(needles[0].1);
    let seen = find(top, needles) == Some(needles[0].0);
    clear();
    let ok = black_box(op)();
    find(top, needles).map_or(clean && seen && ok, |n| { say(&[b"found ", n.as_bytes()]); false })
}

/// The secret scalar, clamped, and the prefix that `seed` hashes to.
fn halves(seed: &[u8; 32]) -> [[u8; 32]; 2] {
    let h = sha512(&[seed]);
    let mut a: [u8; 32] = core::array::from_fn(|i| h[i]);
    (a[0], a[31]) = (a[0] & 248, (a[31] & 63) | 64);
    [a, core::array::from_fn(|i| h[32 + i])]
}

/// The scrub check's call payload as its envelope field encodes it: tag, length, bytes.
const FIELD: &[u8; 19] = b"\x1a\x11MISSION-WAYPOINTS";

/// A signed call: the signer's scalar and prefix, the plaintext, and the keystream block and one-time key of the nonce
/// a twin's call takes, which is the next call's.
fn scrub_call() -> bool {
    let Some((a, b)) = signed(two(Key::new)).and_then(pair) else { return false };
    // SAFETY: a peer's bytes are its save file (`repr(C)`, no pointers); `b` is another cell of the same type
    unsafe { core::ptr::copy_nonoverlapping(&raw const *a, &raw mut *b, 1) };
    let Ok(id) = b.call(5, "nav", "Ping", &FIELD[2..], 30) else { return false };
    let ([s, p], ks, otk) = (halves(&[1; 32]), block(&K, 1, &id.0), block(&K, 0, &id.0));
    scrub(&[("keystream", &ks), ("one-time key", &otk[..32]), ("scalar", &s), ("prefix", &p), ("plaintext", FIELD)], &mut || a.call(5, "nav", "Ping", &FIELD[2..], 30) == Ok(id))
}

/// A seal and an open: `s`'s plaintext through it and back.
fn both<S: Sealer>(s: &mut S) -> bool {
    let (mut plain, mut wire, mut out) = (*b"waypoint", [0u8; 64], [0u8; 64]);
    s.seal(&[1; 12], b"nav", &mut plain, 8, &mut wire).is_ok_and(|n| s.open(b"nav", &wire[..n], &mut out).is_ok_and(|o| o.len == 8))
}

/// A seal and an open under each cipher, one key: the key, `Key`'s keystream block and one-time key, `Gcm`'s round keys 1
/// and 14 as `encrypt` reads them, H, E(K, J₀) and the keystream of the first two blocks.
#[allow(clippy::cast_possible_truncation)] // the key's bytes are a small formula
fn scrub_seal() -> bool {
    let key: [u8; 32] = core::array::from_fn(|i| (i as u8).wrapping_mul(0x9d) ^ 0x5a);
    let (ks, otk, sk) = (block(&key, 1, &[1; 12]), block(&key, 0, &[1; 12]), aes::schedule(&key));
    let words = |q: &[u32]| -> [u8; 32] { core::array::from_fn(|i| q[i / 4].to_ne_bytes()[i % 4]) };
    let (r1, r14) = (words(&sk.0[8..16]), words(&sk.0[112..]));
    let ctr = |i: u8| -> [u8; 16] { core::array::from_fn(|j| if j < 12 { 1 } else if j == 15 { i } else { 0 }) };
    let (mut hj, mut ctrs) = ([[0; 16], ctr(1)], [ctr(2), ctr(3)]);
    aes::encrypt(&sk, &mut hj);
    aes::encrypt(&sk, &mut ctrs);
    let (mut k, mut g) = (Key::new(key), Gcm::new(key));
    let needles: [(&str, &[u8]); 8] = [("round key 1", &r1), ("round key 14", &r14), ("key", &key), ("chacha keystream", &ks), ("one-time key", &otk[..32]), ("H", &hj[0]), ("E(K, J0)", &hj[1]), ("aes keystream", ctrs.as_flattened())];
    scrub(&needles, &mut || both(&mut k)) && scrub(&needles, &mut || both(&mut g)) // apart, so GCM's frames cover none of ChaCha's
}

type Check = (&'static str, fn() -> bool);

fn main() -> ! {
    let checks: [Check; 13] = [
        ("seal key", || aead(&CHACHA, seal::seal, seal::open)),
        ("seal gcm", || aead(&GCM, mudrpc_gcm::seal, mudrpc_gcm::open)),
        ("sign", || public_key(&SEED) == PK && sign(&SEED, &PK, &[]) == SIG),
        ("verify", || { let mut s = SIG; s[0] ^= 1; verify(&PK, &[], &SIG) && !verify(&PK, &[], &s) }),
        ("call key", || round(two(Key::new))),
        ("call signed key", || signed(two(Key::new)).is_some_and(round)),
        ("call gcm", || round(two(Gcm::new))),
        ("call signed gcm", || signed(two(Gcm::new)).is_some_and(round)),
        ("replay", replay),
        ("resume", resume),
        ("scrub sign", || { let [a, p] = halves(&SEED); scrub(&[("prefix", &p), ("scalar", &a), ("seed", &SEED)], &mut || sign(&SEED, &PK, &[]) == SIG) }),
        ("scrub call", scrub_call),
        ("scrub seal", scrub_seal),
    ];
    for (n, (name, check)) in (1u8..).zip(checks) {
        let d = [b'0' + n / 10, b'0' + n % 10];
        let d = if n < 10 { &d[1..] } else { &d[..] };
        if !check() { say(&[b"FAIL ", d, b" ", name.as_bytes()]); exit(n.into()); }
        say(&[b"ok ", d, b" ", name.as_bytes()]);
    }
    #[cfg(feature = "icount")]
    count::run();
    exit(0)
}
