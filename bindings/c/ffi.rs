//! The one native interop surface. Every language wrapper sits on these functions.
//! Rules: the caller owns all memory (`mud_size` then `mud_new`); nothing allocates; no pointer is kept past the call that
//! passed it; every buffer travels with a length, the block too; every function returns a code; no callbacks;
//! `mud_deliver` and `mud_outgoing` write into the caller's scratch and point into it. Every entry from `mud_new` to
//! `mud_sent` runs under `guard`, so a panic cannot cross: it becomes `MUD_EPANIC`, and each that takes a block holds its
//! busy byte while it runs (`held`), so a second caller is `MUD_EBUSY`. `mud_count`, `mud_pressure`, `mud_pending`,
//! `mud_queued`, the sizes and wasm's allocator have no panic path and write nothing; the simulator and generator below
//! are development tools, unguarded.
//! Built without the default `std` feature, for a firmware, the crate is `no_std`: the peer entries alone, a panic calling C's `abort`.
#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs, clippy::pedantic, clippy::nursery, clippy::undocumented_unsafe_blocks)]
#![allow(
    non_camel_case_types, // the ABI's names are C's
    clippy::missing_safety_doc, // the contract is the module's, above; each unsafe body carries its SAFETY line
    clippy::many_single_char_names, // the locals are the C arguments' initials: `s` the simulator, `t` the topic, `w` the wire, `o` the out struct
)]
use core::ffi::c_void;
use core::{mem, ptr, slice, str};
use mudrpc_core::{codec, envelope};
use mudrpc_core::rpc::{self, Call, Dropped, Incoming, Outgoing, Peer, MAX_TOPIC};
use mudrpc_auth::signed::Signed;
use mudrpc_core::seal::{self, Cipher, Key, Sealer, Slot, Slots};
use mudrpc_gcm::Gcm;

/** The ABI's message size, and the scratch `mud_deliver` needs. */ pub const MUD_MSG: usize = 512;
/** The longest topic or name, core's `MAX_TOPIC`. */ pub const MUD_MAX_TOPIC: usize = 64;
/** Senders a leaf believes; the line past it is `MUD_EFULL`. */ pub const MUD_ROSTER: usize = 16;
/** Senders a hub believes. */ pub const MUD_HUB_ROSTER: usize = 256;
const _: () = assert!(MUD_MAX_TOPIC == MAX_TOPIC);

/// The ABI's peers: 512-byte messages, 16 calls in flight, 16 queued sends; a leaf remembers 64 messages and keeps 16
/// senders' windows, a hub 1024 and 256.
///
/// The leaf block holds either an unsigned peer under the fleet key alone or a signed one with a seed, a roster of 16 and a
/// budget (SPEC.md layer 1, authentication), tagged 0 or 1. The hub block, tagged 2, is the signed peer with a 256-line roster, for the host
/// that believes a fleet. Tags 3, 4 and 5 are the same three under AES-256-GCM (SPEC.md layer 5).
pub type Plain = Peer<Key, MUD_MSG, 16, 64, 16, 16>;
/// The signed leaf: a roster of 16 lines.
pub type Auth = Peer<Signed<Key, MUD_ROSTER>, MUD_MSG, 16, 64, 16, 16>;
/// The signed hub: a roster of 256 lines, 256 windows, 1024 remembered messages.
pub type Hub = Peer<Signed<Key, MUD_HUB_ROSTER>, MUD_MSG, 16, 1024, 256, 16>;
/// The unsigned leaf under AES-256-GCM.
pub type AesPlain = Peer<Gcm, MUD_MSG, 16, 64, 16, 16>;
/// The signed leaf under AES-256-GCM.
pub type AesAuth = Peer<Signed<Gcm, MUD_ROSTER>, MUD_MSG, 16, 64, 16, 16>;
/// The signed hub under AES-256-GCM.
pub type AesHub = Peer<Signed<Gcm, MUD_HUB_ROSTER>, MUD_MSG, 16, 1024, 256, 16>;
/// The leaf block, `mud_size()` bytes: tag 0 an unsigned peer, tag 1 a signed one, 3 and 4 the same under AES.
#[repr(C, u32)]
#[allow(missing_docs, clippy::large_enum_variant)] // the variants are named above; the block is sized for the larger kind: one size for every leaf, whichever it provisions
pub enum P { Plain(Plain), Signed(Auth), AesPlain(AesPlain) = 3, AesSigned(AesAuth) }
/// The hub block, `mud_hub_size()` bytes: tag 2, after the leaf's two, and 5 under AES.
#[repr(C, u32)]
#[allow(missing_docs)] // named above
pub enum H { Hub(Hub) = 2, AesHub(AesHub) = 5 }
const _: () = assert!(mem::align_of::<H>() == mem::align_of::<P>());
/// Where a variant starts: `repr(C, u32)` is the tag, then the variants' union at its alignment.
const BODY: usize = mem::align_of::<P>();
const _: () = assert!(mem::size_of::<P>() == BODY + mem::size_of::<Auth>() && mem::size_of::<H>() == BODY + mem::size_of::<Hub>());

/// Builds the block in place: zeros, the tag, the peer. An enum written whole would carry the bytes it never set, the word
/// after its tag and the tail past a smaller variant, from the stack into every save.
const unsafe fn place<T>(mem: *mut c_void, size: usize, tag: u32, peer: T) { // SAFETY: `mem` is `size` writable bytes aligned for `P`, as `mud_new` checked; `peer` has no padding
    ptr::write_bytes(mem.cast::<u8>(), 0, size);
    mem.cast::<u32>().write(tag);
    mem.cast::<u8>().add(BODY).cast::<T>().write(peer);
}

/// The fleet key's three slots, wherever the sealer keeps them.
trait Keys { type K; fn keys(&mut self) -> &mut Self::K; }
impl<C: Cipher> Keys for Slots<C> { type K = Self; fn keys(&mut self) -> &mut Self { self } }
impl<S: Sealer, const R: usize> Keys for Signed<S, R> { type K = S; fn keys(&mut self) -> &mut S { self.inner() } }

/// The caller's block as the peer it holds, by its tag.
enum Any<'a> { Plain(&'a mut Plain), Signed(&'a mut Auth), Hub(&'a mut Hub), AesPlain(&'a mut AesPlain), AesSigned(&'a mut AesAuth), AesHub(&'a mut AesHub) }
const fn size(a: &Any) -> usize { if matches!(a, Any::Hub(_) | Any::AesHub(_)) { mem::size_of::<H>() } else { mem::size_of::<P>() } }

/// Runs `body` on whichever peer `any` holds.
macro_rules! each { ($any:expr, |$p:ident| $body:expr) => { match $any { Any::Plain($p) => $body, Any::Signed($p) => $body, Any::Hub($p) => $body, Any::AesPlain($p) => $body, Any::AesSigned($p) => $body, Any::AesHub($p) => $body } }; }
/// `body` on peer `p` with its busy byte held, or `MUD_EBUSY` when another entry holds it: a detector of two threads on one
/// block, the host's fault, not a lock.
macro_rules! held { ($p:ident, $body:expr) => { if $p.hold(true) { MUD_EBUSY } else { let r = $body; $p.hold(false); r } }; }
/// Runs `body` on whichever peer the block holds, held, or returns the code `peer` gave.
macro_rules! on { ($mem:expr, $len:expr, |$p:ident| $body:expr) => { match peer($mem, $len) { Err(e) => e, Ok(a) => each!(a, |$p| held!($p, $body)) } }; }
/// Signed peers only: `body` on a leaf's or a hub's, held, `MUD_EINVAL` for an unsigned one.
macro_rules! signed { ($mem:expr, $len:expr, |$p:ident| $body:expr) => { match peer($mem, $len) { Err(e) => e, Ok(Any::Plain(_) | Any::AesPlain(_)) => MUD_EINVAL, Ok(Any::Signed($p)) => held!($p, $body), Ok(Any::Hub($p)) => held!($p, $body), Ok(Any::AesSigned($p)) => held!($p, $body), Ok(Any::AesHub($p)) => held!($p, $body) } }; }

/** Done. */ pub const MUD_OK: i32 = 0;
/** An argument refused: a null or misaligned block, a struct misaligned, a buffer null, too short or on the block's bytes, text not UTF-8, a name or topic empty, a value out of range. */ pub const MUD_EINVAL: i32 = -1;
/** No pending or outbox slot, a full roster, or a simulated radio down. */ pub const MUD_EFULL: i32 = -2;
/** A topic over `MUD_MAX_TOPIC`, a message over `MUD_MSG`, a seq at 64, or `mud_gen`'s `out` too small. */ pub const MUD_ETOOLARGE: i32 = -3;
/** Bytes that are not a live peer's, a block `mud_resume` refuses, or one restored and never resumed. */ pub const MUD_ECORRUPT: i32 = -4;
/** A block null, misaligned or smaller than its shape. */ pub const MUD_EMEM: i32 = -5;
/** An epoch not above the current one. */ pub const MUD_EEPOCH: i32 = -6;
/** A send from sender 0. */ pub const MUD_EUNPROVISIONED: i32 = -7;
/** The counter under this epoch is spent: `mud_renew`. */ pub const MUD_EEXHAUSTED: i32 = -8;
/** This second's 32 sends are spent: the next second sends. */ pub const MUD_ERATE: i32 = -9;
/** Another entry holds the block: two threads on one peer, the host's fault; nothing was done. `mud_resume` clears it. */ pub const MUD_EBUSY: i32 = -10;
/** A panic inside the library, while it unwinds; built without std there is no panic to catch: the tripwire proves none is reachable. */ pub const MUD_EPANIC: i32 = -99;

/** Nothing: no verdict, or nothing queued. */ pub const MUD_NONE: i32 = 0;
/** A call for this peer to answer with `mud_reply`. */ pub const MUD_CALL: i32 = 1;
/** A reply to a call of ours; `end` closes the stream. */ pub const MUD_REPLY: i32 = 2;
/** A publish on a topic this peer listens to. */ pub const MUD_EVENT: i32 = 3;
/** A call of ours whose ttl passed with the stream open. */ pub const MUD_EXPIRED: i32 = 4;
/** A wire refused; `dropped` and `status` say why. */ pub const MUD_DROPPED: i32 = 5;
/** A sender over 64 a second, over its share of a second the 256 cap spent, or the one whose wire came after 64 from senders with no window landed that second (the fleet past its windows, not a flooder: size the fleet or sign); reported once a second. */ pub const MUD_FLOOD: i32 = 6;
/** A sender whose epoch went back: a cloned or rolled-back device. */ pub const MUD_ROLLBACK: i32 = 7;
/** `wire` to send on `topic`, kept for `ttl_secs`. */ pub const MUD_SEND: i32 = 1;
/** `topic` to announce and subscribe to. */ pub const MUD_LISTEN: i32 = 2;
/** `mud_config.cipher`: ChaCha20-Poly1305, wire versions 1 and 2 signed, smaller and faster without an AES instruction. */ pub const MUD_CIPHER_CHACHA20_POLY1305: u32 = 0;
/** AES-256-GCM, wire versions 3 and 4 signed. */ pub const MUD_CIPHER_AES_256_GCM: u32 = 1;
/** What every wrapper passes when its caller names none, and what a C host names: AES, the NIST algorithm buyers ask for; a zeroed config is ChaCha20-Poly1305. */ pub const MUD_CIPHER_DEFAULT: u32 = MUD_CIPHER_AES_256_GCM;

/// Bytes or UTF-8 text, not NUL-terminated: `ptr` readable for `len` bytes, null only when `len` is 0.
#[repr(C)] #[derive(Clone, Copy)] #[allow(missing_docs)] pub struct mud_buf { pub ptr: *const u8, pub len: usize }
/// The caller's writable bytes: `ptr` writable for `len` bytes, never null.
#[repr(C)] #[derive(Clone, Copy)] #[allow(missing_docs)] pub struct mud_mutbuf { pub ptr: *mut u8, pub len: usize }
/// The call's nonce: sender ‖ epoch ‖ counter.
#[repr(C)] #[derive(Clone, Copy)] #[allow(missing_docs)] pub struct mud_id { pub bytes: [u8; 12] }
/// What `mud_new` builds a peer from; anything else is `MUD_EINVAL`.
///
/// `name`: the peer's inbox topic; `key`: exactly 32 bytes and not 32 equal ones (a zeroed config, an example's constant), as for
/// `seed` and `mud_stage`; `sender`: roster number, 0 cannot send; `epoch`: persisted beside the key, higher every boot; `seed`:
/// empty for an unsigned peer, this sender's 32-byte Ed25519 seed for a signed one (wire version 2, or 4 under AES); `budget`: verifications per
/// second a signed receiver affords, at least 1; `hub`: 0 a leaf (`MUD_ROSTER` lines in `mud_size()` bytes), 1 a hub
/// (`MUD_HUB_ROSTER` lines in `mud_hub_size()` bytes, a seed required); `cipher`: a `MUD_CIPHER_` constant.
#[repr(C)] #[allow(missing_docs)] pub struct mud_config { pub name: mud_buf, pub key: mud_buf, pub sender: u32, pub epoch: u32, pub seed: mud_buf, pub budget: u32, pub hub: u32, pub cipher: u32 }

/// A verdict from `mud_deliver` or `mud_tick`; fields a kind does not use are zero or empty.
///
/// `kind` one of `MUD_NONE` .. `MUD_ROLLBACK`; `id` the call's on `MUD_CALL`, `MUD_REPLY`, `MUD_EXPIRED`; `topic`,
/// `method`, `reply_to`, `detail`, `bytes` point into your scratch and topic buffers; `ttl_secs` a call's; `seq` and `end` (1 on
/// the last) a call's or a reply's stream; `status` a reply's, or the detail of a drop.
#[repr(C)]
#[allow(missing_docs)] // named above
pub struct mud_incoming {
    pub kind: i32,
    pub id: mud_id,
    pub topic: mud_buf,
    pub method: mud_buf,
    pub reply_to: mud_buf,
    pub detail: mud_buf,
    pub bytes: mud_buf,
    pub ttl_secs: u32,
    pub seq: u32,
    pub status: u32,
    pub end: i32,
    /// For `MUD_DROPPED`: 1 version, 2 open, 3 decode, 4 duplicate, 5 no pending call, 6 topic, or a call's `reply_to`, empty or too long, 7 seq beyond the
    /// stream bound or its lowest end, or not 0 on a call, 8 id field disagrees with the header, 9 replay, behind the sender's window or floor, 10 rate. `status` carries
    /// the detail: the version byte; open 1 short 2 tag 3 scratch too small 4 signature 5 no roster line 6 budget; decode 1..7.
    pub dropped: i32,
    /// The sender's roster number on `MUD_CALL`, `MUD_REPLY`, `MUD_EVENT`, `MUD_FLOOD` and `MUD_ROLLBACK`: what the sender wrote,
    /// or proven under a signed fleet.
    pub sender: u32,
}

/// An item for the transport: `kind` `MUD_SEND` with `topic`, `wire` and `ttl_secs`, or `MUD_LISTEN` with `topic`; both point into your scratch.
#[repr(C)] #[allow(missing_docs)]
pub struct mud_out { pub kind: i32, pub topic: mud_buf, pub wire: mud_buf, pub ttl_secs: u32 }

const EMPTY: mud_buf = mud_buf { ptr: ptr::null(), len: 0 };

const fn b(s: &[u8]) -> mud_buf { mud_buf { ptr: s.as_ptr(), len: s.len() } }

const unsafe fn bytes<'a>(x: mud_buf) -> Option<&'a [u8]> { // SAFETY: `ptr` is readable for `len` bytes for the call, as the contract says; null with a length is refused
    if x.len == 0 { Some(&[]) } else if x.ptr.is_null() { None } else { Some(slice::from_raw_parts(x.ptr, x.len)) }
}

unsafe fn text<'a>(x: mud_buf) -> Option<&'a str> { str::from_utf8(bytes(x)?).ok() } // SAFETY: as `bytes`

const unsafe fn mutable<'a>(x: mud_mutbuf) -> Option<&'a mut [u8]> { // SAFETY: `ptr` is writable for `len` bytes for the call and aliases no other argument; null is refused
    if x.ptr.is_null() { None } else { Some(slice::from_raw_parts_mut(x.ptr, x.len)) }
}

/// Whether `len` bytes at `p` share none of the block's `n` at `mem`; no bytes share none, wherever they point.
fn apart(mem: *mut c_void, n: usize, p: *const u8, len: usize) -> bool { let (m, p) = (mem as usize, p as usize); len == 0 || p.saturating_add(len) <= m || m.saturating_add(n) <= p }

/// Whether none of `bufs` shares a byte with the block's `n` at `mem`.
fn beside(mem: *mut c_void, n: usize, bufs: &[mud_buf]) -> bool { bufs.iter().all(|x| apart(mem, n, x.ptr, x.len)) }

/// The caller's struct to read: null or misaligned is `None`.
unsafe fn read<'a, T>(x: *const T) -> Option<&'a T> { if x.is_aligned() { x.as_ref() } else { None } } // SAFETY: a non-null, aligned `x` is the caller's `T` for the call

/// The caller's struct to write beside the block's `n` bytes at `mem`: null, misaligned or sharing a byte with them is `None`.
unsafe fn own<'a, T>(x: *mut T, mem: *mut c_void, n: usize) -> Option<&'a mut T> { if x.is_aligned() && apart(mem, n, x.cast(), mem::size_of::<T>()) { x.as_mut() } else { None } } // SAFETY: as `read`, and no byte of it is the block's

/// The caller's block as a peer, at every entry: null or misaligned is `MUD_EINVAL`; `len` under 4 bytes or under the shape
/// the tag names is `MUD_EMEM`, the tag the one thing read; bytes without a shape's tag and this layout's magic are
/// `MUD_ECORRUPT`, so a block freed and reused is refused rather than read; a block `mud_save` copied out is disarmed until
/// `mud_resume`, so one restored without resuming refuses to send.
unsafe fn peer<'a>(m: *mut c_void, len: usize) -> Result<Any<'a>, i32> { // SAFETY: a non-null, aligned `m` is `len` bytes of the caller's; the tag is read only within them, the shape only once they cover it
    if m.is_null() || !(m as usize).is_multiple_of(mem::align_of::<P>()) { return Err(MUD_EINVAL); }
    if len < 4 || len < if matches!(*m.cast::<u32>(), 2 | 5) { mem::size_of::<H>() } else { mem::size_of::<P>() } { return Err(MUD_EMEM); }
    let a = match *m.cast::<u32>() { // the tag, before the block is read as one shape
        0 | 1 | 3 | 4 => match &mut *m.cast::<P>() { P::Plain(x) => Any::Plain(x), P::Signed(x) => Any::Signed(x), P::AesPlain(x) => Any::AesPlain(x), P::AesSigned(x) => Any::AesSigned(x) },
        2 | 5 => match &mut *m.cast::<H>() { H::Hub(x) => Any::Hub(x), H::AesHub(x) => Any::AesHub(x) },
        _ => return Err(MUD_ECORRUPT),
    };
    if each!(&a, |x| x.live()) { Ok(a) } else { Err(MUD_ECORRUPT) }
}

/// A key is exactly 32 bytes; a shorter buffer is refused, never read past.
/// A secret (the fleet key, a staged key, a seed) is also never 32 equal bytes: a zeroed config and every example's `[1; 32]` are refused.
unsafe fn secret32(b: mud_buf) -> Option<[u8; 32]> { key32(b).filter(|k| k.windows(2).any(|w| w[0] != w[1])) } // SAFETY: as `key32`
unsafe fn key32(b: mud_buf) -> Option<[u8; 32]> { (!b.ptr.is_null() && b.len == 32).then(|| slice::from_raw_parts(b.ptr, 32).try_into().unwrap()) } // SAFETY: read only when `len` is 32, within it

const fn code(r: Result<(), rpc::Error>) -> i32 {
    match r {
        Ok(()) => MUD_OK, Err(rpc::Error::Full) => MUD_EFULL, Err(rpc::Error::TooLarge) => MUD_ETOOLARGE, Err(rpc::Error::Corrupt) => MUD_ECORRUPT,
        Err(rpc::Error::Epoch) => MUD_EEPOCH, Err(rpc::Error::Unprovisioned) => MUD_EUNPROVISIONED, Err(rpc::Error::Exhausted) => MUD_EEXHAUSTED, Err(rpc::Error::Rate) => MUD_ERATE, Err(rpc::Error::Empty) => MUD_EINVAL,
    }
}

#[cfg(feature = "std")]
fn guard(f: impl FnOnce() -> i32) -> i32 { std::panic::catch_unwind(core::panic::AssertUnwindSafe(f)).unwrap_or(MUD_EPANIC) }
#[cfg(not(feature = "std"))]
fn guard(f: impl FnOnce() -> i32) -> i32 { f() } // no unwind to catch, and the tripwire proves no panic is reachable

#[cfg(not(feature = "std"))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    extern "C" { fn abort() -> !; }
    // SAFETY: C's `abort` takes nothing and never returns; the tripwire defines none, so a reachable panic fails its link
    unsafe { abort() }
}

#[allow(clippy::needless_pass_by_value)] // `fill` is the verdict's last reader: a move says so
fn fill(out: &mut mud_incoming, i: Incoming) -> i32 {
    *out = mud_incoming { kind: MUD_NONE, id: mud_id { bytes: [0; 12] }, topic: EMPTY, method: EMPTY, reply_to: EMPTY, detail: EMPTY, bytes: EMPTY, ttl_secs: 0, seq: 0, status: 0, end: 0, dropped: 0, sender: 0 };
    match i {
        Incoming::Call(c) => { out.kind = MUD_CALL; out.id.bytes = c.id.0; out.sender = c.sender; out.topic = b(c.topic.as_bytes()); out.method = b(c.method.as_bytes()); out.reply_to = b(c.reply_to.as_bytes()); out.ttl_secs = c.ttl_secs; out.seq = c.seq; out.end = i32::from(c.end); out.bytes = b(c.bytes); }
        Incoming::Reply { id, sender, seq, end, status, detail, bytes } => { out.kind = MUD_REPLY; out.id.bytes = id.0; out.sender = sender; out.seq = seq; out.end = i32::from(end); out.status = status; out.detail = b(detail.as_bytes()); out.bytes = b(bytes); }
        Incoming::Event { topic, sender, bytes } => { out.kind = MUD_EVENT; out.topic = b(topic.as_bytes()); out.sender = sender; out.bytes = b(bytes); }
        Incoming::Expired { id } => { out.kind = MUD_EXPIRED; out.id.bytes = id.0; }
        Incoming::Flood { sender } => { out.kind = MUD_FLOOD; out.sender = sender; }
        Incoming::Rollback { sender } => { out.kind = MUD_ROLLBACK; out.sender = sender; }
        Incoming::Dropped(why) => {
            out.kind = MUD_DROPPED;
            out.dropped = match why {
                Dropped::Version(v) => { out.status = u32::from(v); 1 }
                Dropped::Open(e) => { out.status = match e { seal::Error::Short => 1, seal::Error::Tag => 2, seal::Error::Full => 3, seal::Error::Sig => 4, seal::Error::Roster => 5, seal::Error::Budget => 6 }; 2 }
                Dropped::Decode(e) => { out.status = match e { envelope::Error::Codec(c) => match c { codec::Error::Truncated => 1, codec::Error::Overflow => 2, codec::Error::Tag => 3, codec::Error::Full => 4, codec::Error::Utf8 => 5 }, envelope::Error::Missing => 6, envelope::Error::Id => 7 }; 3 }
                Dropped::Duplicate => 4,
                Dropped::NoPending => 5,
                Dropped::Topic => 6,
                Dropped::Seq => 7,
                Dropped::Id => 8,
                Dropped::Replay => 9,
                Dropped::Rate => 10,
            };
        }
    }
    out.kind
}

/// Bytes a leaf needs. Allocate at least this, aligned to `mud_align()`.
#[no_mangle] pub const extern "C" fn mud_size() -> usize { mem::size_of::<P>() }
/// Bytes a hub needs: the signed peer with a 256-line roster, 256 windows and 1024 remembered messages.
#[no_mangle] pub const extern "C" fn mud_hub_size() -> usize { mem::size_of::<H>() }
/// The alignment every block and every saved copy needs.
#[no_mangle] pub const extern "C" fn mud_align() -> usize { mem::align_of::<P>() }

/// Builds a peer in `mem`. `name` is its inbox topic; `key` is 32 bytes; `sender` is the roster number (0 cannot send);
/// `epoch` is the host's persisted counter, written to storage before this is called.
#[no_mangle]
pub unsafe extern "C" fn mud_new(mem: *mut c_void, len: usize, config: *const mud_config) -> i32 { // SAFETY: `mem` is checked for size and alignment before the write; `config` is read through `read`, null and misaligned refused
    guard(|| {
        if mem.is_null() || len < mem::size_of::<P>() || !(mem as usize).is_multiple_of(mem::align_of::<P>()) { return MUD_EMEM; }
        let Some(c) = read(config).filter(|c| apart(mem, len, config.cast(), mem::size_of::<mud_config>()) && beside(mem, len, &[c.name, c.key, c.seed])) else { return MUD_EINVAL };
        let (Some(name), Some(key)) = (text(c.name), secret32(c.key)) else { return MUD_EINVAL };
        if c.hub == 1 && len < mem::size_of::<H>() { return MUD_EMEM; }
        match c.cipher { MUD_CIPHER_CHACHA20_POLY1305 => shape(mem, c, name, Key::new(key), 0), MUD_CIPHER_AES_256_GCM => shape(mem, c, name, Gcm::new(key), 3), _ => MUD_EINVAL }
    })
}

/// The shape `c` names under the fleet sealer `k`, tagged from `tag`: the unsigned leaf, the signed leaf, the hub.
unsafe fn shape<S: Sealer>(mem: *mut c_void, c: &mud_config, name: &str, k: S, tag: u32) -> i32 { // SAFETY: as `place`, which `mud_new` checked for
    match (c.seed.len, secret32(c.seed), c.hub) {
        (0, _, 0) => code(Peer::<S, MUD_MSG, 16, 64, 16, 16>::new(name, k, c.sender, c.epoch).map(|p| place(mem, mem::size_of::<P>(), tag, p))),
        (_, Some(seed), 0) if c.budget > 0 => code(Peer::<Signed<S, MUD_ROSTER>, MUD_MSG, 16, 64, 16, 16>::new(name, Signed::new(k, seed, c.budget), c.sender, c.epoch).map(|p| place(mem, mem::size_of::<P>(), tag + 1, p))), // a budget of 0 would receive nothing, silently
        (_, Some(seed), 1) if c.budget > 0 => code(Peer::<Signed<S, MUD_HUB_ROSTER>, MUD_MSG, 16, 1024, 256, 16>::new(name, Signed::new(k, seed, c.budget), c.sender, c.epoch).map(|p| place(mem, mem::size_of::<H>(), tag + 2, p))), // a hub is a roster; an unsigned peer has none to size
        _ => MUD_EINVAL,
    }
}

/// The peer's bytes for storage, into `out` of its shape's size (`mud_size()` or `mud_hub_size()`) aligned to `mud_align()`,
/// sharing no byte with `mem`.
///
/// The copy cannot send until `mud_resume` arms it: a block restored without resuming is `MUD_ECORRUPT`, not a nonce reused.
#[no_mangle]
pub unsafe extern "C" fn mud_save(mem: *mut c_void, len: usize, out: *mut c_void, out_len: usize) -> i32 { // SAFETY: `peer` checks `mem`, so its bytes are a peer's; `out` is checked for size and alignment and shares no byte with it
    guard(|| {
        if out.is_null() || !(out as usize).is_multiple_of(mem::align_of::<P>()) { return MUD_EMEM; }
        let n = match peer(mem, len) { Err(e) => return e, Ok(a) => size(&a) };
        if out_len < n { return MUD_EMEM; }
        if !apart(mem, n, out.cast(), n) { return MUD_EINVAL; } // the copy shares a byte with its source
        if let Ok(a) = peer(mem, len) { if each!(a, |p| p.hold(true)) { return MUD_EBUSY; } }
        ptr::copy_nonoverlapping(mem.cast::<u8>(), out.cast::<u8>(), n); // the copy carries the hold, released in both
        if let Ok(a) = peer(mem, len) { each!(a, |p| p.hold(false)); }
        match peer(out, n) { Err(e) => e, Ok(a) => each!(a, |p| { p.hold(false); p.disarm(); MUD_OK }) }
    })
}

/// After copying a saved peer back into `mem`, `len` bytes: validate it and take the host's new epoch, which must be higher.
///
/// The one entry that takes a held block: it clears, when it succeeds, the busy byte an entry a crash or a panic ended left in it.
#[no_mangle]
pub unsafe extern "C" fn mud_resume(mem: *mut c_void, len: usize, epoch: u32) -> i32 { guard(|| match peer(mem, len) { Err(e) => e, Ok(a) => each!(a, |p| code(p.resume(epoch))) }) } // SAFETY: `peer` checks the block

/// A higher epoch for a live peer; clears `MUD_EEXHAUSTED`. Written to storage first.
#[no_mangle]
pub unsafe extern "C" fn mud_renew(mem: *mut c_void, len: usize, epoch: u32) -> i32 { guard(|| on!(mem, len, |p| code(p.renew(epoch)))) } // SAFETY: `peer` checks the block

/// Ends a peer: runs its destructor and zeroes the whole block, keys included. `mem` is then the caller's to free.
#[no_mangle]
pub unsafe extern "C" fn mud_drop(mem: *mut c_void, len: usize) -> i32 { // SAFETY: `peer` checks the block; after `drop_in_place` nothing reads it before the zeroing clears the magic
    guard(|| match peer(mem, len) { Err(e) => e, Ok(a) => { let n = size(&a); each!(a, |p| if p.hold(true) { return MUD_EBUSY } else { ptr::drop_in_place(p) }); ptr::write_bytes(mem.cast::<u8>(), 0, n); MUD_OK } })
}

/// The next fleet key, 32 bytes and not 32 equal ones: the first of a rotation's three steps.
#[no_mangle]
pub unsafe extern "C" fn mud_stage(mem: *mut c_void, len: usize, key: mud_buf) -> i32 { guard(|| { let n = match peer(mem, len) { Err(e) => return e, Ok(a) => size(&a) }; let Some(k) = secret32(key).filter(|_| beside(mem, n, &[key])) else { return MUD_EINVAL }; on!(mem, len, |p| { p.sealer_mut().keys().stage(k); MUD_OK }) }) } // SAFETY: `peer` checks the block; `key` is read only if 32 bytes long
/// Previous ← current ← next: the peer seals under the staged key and still opens the old one.
#[no_mangle]
pub unsafe extern "C" fn mud_rotate(mem: *mut c_void, len: usize) -> i32 { guard(|| on!(mem, len, |p| { p.sealer_mut().keys().rotate(); MUD_OK })) } // SAFETY: `peer` checks the block
/// Drops the previous fleet key, once every peer has rotated.
#[no_mangle]
pub unsafe extern "C" fn mud_retire(mem: *mut c_void, len: usize) -> i32 { guard(|| on!(mem, len, |p| { p.sealer_mut().keys().retire(); MUD_OK })) } // SAFETY: `peer` checks the block

/// Signed peers only.
///
/// A roster line for `sender`: its public key and, during a signing-key rotation, the next one (`next` empty otherwise).
/// Refused for a key anyone could sign for, a full roster (16 lines on a leaf, 256 on a hub), or an unsigned peer.
#[no_mangle]
pub unsafe extern "C" fn mud_roster(mem: *mut c_void, len: usize, sender: u32, key: mud_buf, next: mud_buf) -> i32 { // SAFETY: `peer` checks the block; `key` and `next` are read only if 32 bytes long
    guard(|| {
        let m = match peer(mem, len) { Err(e) => return e, Ok(a) => size(&a) };
        let (Some(k), n, true) = (key32(key), if next.len == 0 { None } else { Some(key32(next)) }, beside(mem, m, &[key, next])) else { return MUD_EINVAL };
        let next = match n { None => None, Some(Some(n)) => Some(n), Some(None) => return MUD_EINVAL };
        signed!(mem, len, |p| match p.sealer_mut().roster(sender, k, next) { Ok(()) => MUD_OK, Err(seal::Error::Full) => MUD_EFULL, Err(_) => MUD_EINVAL })
    })
}
/// Signed peers only: removes `sender`'s roster line.
#[no_mangle]
pub unsafe extern "C" fn mud_revoke(mem: *mut c_void, len: usize, sender: u32) -> i32 { // SAFETY: `peer` checks the block
    guard(|| signed!(mem, len, |p| { p.sealer_mut().revoke(sender); MUD_OK }))
}
/// The 32-byte public key of a seed, for building rosters.
#[no_mangle]
pub unsafe extern "C" fn mud_public_key(seed: mud_buf, out: *mut u8) -> i32 { // SAFETY: `seed` is read only if 32 bytes long; `out` is the caller's 32 writable bytes, null refused
    guard(|| { let (Some(s), false) = (key32(seed), out.is_null()) else { return MUD_EINVAL }; ptr::copy_nonoverlapping(mudrpc_auth::ed25519::public_key(&s).as_ptr(), out, 32); MUD_OK })
}
/// Which key slot opened the last wire that reached product code: 0 current, 1 previous, 2 next.
#[no_mangle]
pub unsafe extern "C" fn mud_opened(mem: *mut c_void, len: usize) -> i32 { guard(|| on!(mem, len, |p| match p.opened() { Slot::Current => 0, Slot::Previous => 1, Slot::Next => 2 })) } // SAFETY: `peer` checks the block

/// Calls `method` on `service`, writing the id its replies will carry to `out`; the call expires after `ttl_secs`.
#[no_mangle]
pub unsafe extern "C" fn mud_call(mem: *mut c_void, len: usize, now: u32, service: mud_buf, method: mud_buf, req: mud_buf, ttl_secs: u32, out: *mut mud_id) -> i32 { // SAFETY: `peer` checks the block; the buffers are read within their lengths; `out` is written through `own`
    guard(|| {
        let n = match peer(mem, len) { Err(e) => return e, Ok(a) => size(&a) }; // the block first, so a corrupt block is named before an argument is
        let (Some(s), Some(m), Some(r), Some(o), true) = (text(service), text(method), bytes(req), own(out, mem, n), beside(mem, n, &[service, method, req])) else { return MUD_EINVAL };
        on!(mem, len, |p| match p.call(now, s, m, r, ttl_secs) { Ok(id) => { o.bytes = id.0; MUD_OK } Err(e) => code(Err(e)) })
    })
}

/// Publishes `payload` on `topic` for every listener, no reply; kept by the mesh for `ttl_secs`.
#[no_mangle]
pub unsafe extern "C" fn mud_publish(mem: *mut c_void, len: usize, now: u32, topic: mud_buf, payload: mud_buf, ttl_secs: u32) -> i32 { // SAFETY: `peer` checks the block; the buffers are read within their lengths
    guard(|| {
        let n = match peer(mem, len) { Err(e) => return e, Ok(a) => size(&a) };
        let (Some(t), Some(d), true) = (text(topic), bytes(payload), beside(mem, n, &[topic, payload])) else { return MUD_EINVAL };
        on!(mem, len, |p| code(p.publish(now, t, d, ttl_secs)))
    })
}
/// Listens on `topic`: its events arrive as `MUD_EVENT`, after `mud_outgoing` hands the transport its `MUD_LISTEN`.
#[no_mangle]
pub unsafe extern "C" fn mud_listen(mem: *mut c_void, len: usize, topic: mud_buf) -> i32 { // SAFETY: `peer` checks the block; `topic` is read within its length
    guard(|| {
        let n = match peer(mem, len) { Err(e) => return e, Ok(a) => size(&a) };
        let (Some(t), true) = (text(topic), beside(mem, n, &[topic])) else { return MUD_EINVAL };
        on!(mem, len, |p| code(p.listen(t)))
    })
}
/// Answers a `MUD_CALL` exactly as `mud_deliver` filled it, with a status, its UTF-8 detail (empty for none) and the payload. Stream: seq 0..n, end on the last.
#[no_mangle]
pub unsafe extern "C" fn mud_reply(mem: *mut c_void, len: usize, now: u32, call: *const mud_incoming, seq: u32, status: u32, detail: mud_buf, payload: mud_buf, end: i32) -> i32 { // SAFETY: `peer` checks the block; `call` is read through `read`, null and misaligned refused; the buffers are read within their lengths
    guard(|| {
        let n = match peer(mem, len) { Err(e) => return e, Ok(a) => size(&a) }; // the block first, so a corrupt block is named before an argument is
        let (Some(c), Some(d), Some(why)) = (read(call), bytes(payload), bytes(detail).and_then(|b| core::str::from_utf8(b).ok())) else { return MUD_EINVAL };
        if c.kind != MUD_CALL || !apart(mem, n, call.cast(), mem::size_of::<mud_incoming>()) || !beside(mem, n, &[c.topic, c.method, c.reply_to, c.bytes, detail, payload]) { return MUD_EINVAL; }
        let (Some(topic), Some(method), Some(reply_to), Some(cb)) = (text(c.topic), text(c.method), text(c.reply_to), bytes(c.bytes)) else { return MUD_EINVAL };
        let call = Call { id: mudrpc_core::envelope::Id(c.id.bytes), sender: c.sender, topic, method, reply_to, ttl_secs: c.ttl_secs, seq: c.seq, end: c.end != 0, bytes: cb };
        on!(mem, len, |p| code(p.reply(now, &call, seq, status, why, d, end != 0)))
    })
}

/// A wire from the transport. `scratch` must hold `MUD_MSG` bytes. Returns the kind filled into `out` (pointers into `scratch` and `topic`), or an error.
#[no_mangle]
pub unsafe extern "C" fn mud_deliver(mem: *mut c_void, len: usize, now: u32, topic: mud_buf, wire: mud_buf, scratch: mud_mutbuf, out: *mut mud_incoming) -> i32 { // SAFETY: `peer` checks the block; the buffers are read within their lengths and checked apart from it; `scratch` is the caller's writable bytes, its size checked; `out` is written through `own`
    guard(|| {
        let n = match peer(mem, len) { Err(e) => return e, Ok(a) => size(&a) }; // the block first, so a corrupt block is named before an argument is
        let (Some(t), Some(w), Some(s), Some(o)) = (text(topic), bytes(wire), mutable(scratch), own(out, mem, n)) else { return MUD_EINVAL };
        if s.len() < MUD_MSG || ![(topic.ptr, topic.len), (wire.ptr, wire.len), (scratch.ptr.cast_const(), scratch.len)].into_iter().all(|(p, l)| apart(mem, n, p, l)) { return MUD_EINVAL; } // the scratch must hold a message, a short one an error, not an empty verdict; none is the block's bytes
        on!(mem, len, |p| fill(o, p.deliver(now, t, w, s)))
    })
}

/// Time passes. Returns `MUD_EXPIRED` with the id, or `MUD_NONE`. Call until `MUD_NONE`.
#[no_mangle]
pub unsafe extern "C" fn mud_tick(mem: *mut c_void, len: usize, now: u32, out: *mut mud_incoming) -> i32 { // SAFETY: `peer` checks the block; `out` is written through `own`
    guard(|| {
        let n = match peer(mem, len) { Err(e) => return e, Ok(a) => size(&a) }; // the block first, so a corrupt block is named before an argument is
        let Some(o) = own(out, mem, n) else { return MUD_EINVAL };
        on!(mem, len, |p| p.tick(now).map_or(MUD_NONE, |i| fill(o, i)))
    })
}

/// Next item for the transport, copied into `scratch`, which must hold `MAX_TOPIC + 512` bytes, and left in place until `mud_sent`.
#[no_mangle]
pub unsafe extern "C" fn mud_outgoing(mem: *mut c_void, len: usize, now: u32, scratch: mud_mutbuf, out: *mut mud_out) -> i32 { // SAFETY: `peer` checks the block; `scratch` is the caller's writable bytes, its size checked and apart from the block; `out` is written through `own`
    guard(|| {
        let n = match peer(mem, len) { Err(e) => return e, Ok(a) => size(&a) }; // the block first, so a corrupt block is named before an argument is
        let (Some(s), Some(o)) = (mutable(scratch), own(out, mem, n)) else { return MUD_EINVAL };
        let Some((t, w)) = s.split_at_mut_checked(MAX_TOPIC).filter(|(_, w)| w.len() >= MUD_MSG && apart(mem, n, scratch.ptr, scratch.len)) else { return MUD_EINVAL }; // a short scratch would look like an empty outbox; none is the block's bytes
        *o = mud_out { kind: MUD_NONE, topic: EMPTY, wire: EMPTY, ttl_secs: 0 };
        on!(mem, len, |p| {
            match p.outgoing(now) {
                Some(Outgoing::Send { topic, wire, ttl_secs }) => *o = mud_out { kind: MUD_SEND, topic: copy(t, topic.as_bytes()), wire: copy(w, wire), ttl_secs },
                Some(Outgoing::Listen { topic }) => *o = mud_out { kind: MUD_LISTEN, topic: copy(t, topic.as_bytes()), wire: EMPTY, ttl_secs: 0 },
                None => {}
            }
            o.kind
        })
    })
}

/// `from` into the head of `to`, so what the caller holds points into its own bytes, never the block.
fn copy(to: &mut [u8], from: &[u8]) -> mud_buf {
    for (d, s) in to.iter_mut().zip(from) { *d = *s; }
    b(to.get(..from.len()).unwrap_or(&[]))
}

/// The transport took the item `mud_outgoing` last showed; with nothing shown, nothing happens.
#[no_mangle]
pub unsafe extern "C" fn mud_sent(mem: *mut c_void, len: usize) -> i32 { guard(|| on!(mem, len, |p| { p.sent(); MUD_OK })) } // SAFETY: `peer` checks the block

/// The operator's counters: 0 managed (the sealer taken for a change), 1 rollbacks, 2 floods, 3 dropped but duplicates; anything else, or a block that is not a peer's, is 0.
#[no_mangle]
pub unsafe extern "C" fn mud_count(mem: *mut c_void, len: usize, which: i32) -> u32 { // SAFETY: `peer` checks the block
    let c = match peer(mem, len) { Ok(a) => each!(a, |p| p.counts()), Err(_) => return 0 };
    match which { 0 => c.managed, 1 => c.rollbacks, 2 => c.floods, 3 => c.dropped, _ => 0 }
}
/// Live seen or sender entries evicted, or senders refused a window, for want of room; rising under load means replay protection is degraded. 0 for a block that is not a peer's.
#[no_mangle]
pub unsafe extern "C" fn mud_pressure(mem: *mut c_void, len: usize) -> u32 { peer(mem, len).map_or(0, |a| each!(a, |p| p.pressure())) } // SAFETY: `peer` checks the block
/// Calls in flight, of 16: at 16 `mud_call` is `MUD_EFULL`. 0 for a block that is not a peer's.
#[no_mangle]
pub unsafe extern "C" fn mud_pending(mem: *mut c_void, len: usize) -> u32 { peer(mem, len).map_or(0, |a| each!(a, |p| p.pending())) } // SAFETY: `peer` checks the block
/// Items queued for the transport, of 16: at 16 every producer is `MUD_EFULL`. 0 for a block that is not a peer's.
#[no_mangle]
pub unsafe extern "C" fn mud_queued(mem: *mut c_void, len: usize) -> u32 { peer(mem, len).map_or(0, |a| each!(a, |p| p.queued())) } // SAFETY: `peer` checks the block

/// wasm only: the module's own allocator, lent to the JavaScript wrapper, which has no malloc to reach. Free with the same length.
#[cfg(all(feature = "std", target_arch = "wasm32"))]
#[no_mangle] pub unsafe extern "C" fn mud_alloc(len: usize) -> *mut u8 { std::alloc::alloc(std::alloc::Layout::from_size_align_unchecked(len.max(1), 8)) } // SAFETY: a non-zero size with alignment 8 is a valid layout
/// wasm only: frees what `mud_alloc` gave, with the same length.
#[cfg(all(feature = "std", target_arch = "wasm32"))]
#[no_mangle] pub unsafe extern "C" fn mud_free(ptr: *mut u8, len: usize) { std::alloc::dealloc(ptr, std::alloc::Layout::from_size_align_unchecked(len.max(1), 8)) } // SAFETY: the wrapper frees what `mud_alloc` gave, with the same length

// ───────────────────────── the simulator and the generator: development tools, not core ─────────────────────────
// These allocate (a world, generated source) and are the only functions here that do. They exist so every binding can
// run the standard scenarios and load a .proto at runtime without a second implementation of either.
#[cfg(feature = "std")]
pub use dev::*;
#[cfg(feature = "std")]
mod dev {
    use super::{b, bytes, guard, mutable, ptr, text, mud_buf, mud_mutbuf, EMPTY, MUD_EFULL, MUD_EINVAL, MUD_ETOOLARGE, MUD_NONE, MUD_OK};
    use mudrpc_adapters::udp::{self, Udp};
    use mudrpc_adapters::sim::{self, Kind, Scenario, Sim, World};
    use mudrpc_adapters::transport::{Rx, Transport};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A simulated mesh for one scenario, from `mud_sim_new`, one radio per peer; free it with `mud_sim_free`.
    pub struct mud_sim { world: Rc<RefCell<World>>, radios: Vec<Sim>, scenario: Scenario }

    /// One scenario event.
    ///
    /// `at_ms` when; `kind` 1 partition 2 heal 3 jam 4 unjam (`heal a`) 5 malformed 6 replay 7 flood 8 bandwidth 9 `peer_down` 10 `peer_up`
    /// 18 noise 23 hold, which the world applies, or a host event the harness performs: 11 clone 12 spam 13 stage 14 rotate 15 retire 16 warp
    /// 17 hostile 19 `stale_reboot` 20 `torn_save` 21 revoke 22 telemetry; `a` the first radio or warp's milliseconds; `b` the second radio, a rate, bits per
    /// second, noise or hold's milliseconds; `text` the topic (malformed, flood) or the method (hostile).
    #[repr(C)] #[allow(missing_docs)] pub struct mud_event { pub at_ms: u64, pub kind: i32, pub a: u64, pub b: u64, pub text: mud_buf }

    const unsafe fn simulator<'a>(s: *mut mud_sim) -> Option<&'a mut mud_sim> { s.as_mut() } // SAFETY: a non-null `s` came from `mud_sim_new` and is not yet freed

    /// A product's UDP socket and the peers it sends to, from `mud_udp_new`; free it with `mud_udp_free`.
    pub struct mud_udp(Udp);

    /// 1 when this library was built with the cargo feature `internet`, so `mud_udp_new` may bind or send to a public address.
    #[no_mangle] pub extern "C" fn mud_internet() -> i32 { i32::from(udp::INTERNET) }

    /// gRPC's name for a status number as a C string that lives as long as the library, `""` past 16.
    #[no_mangle] pub extern "C" fn mud_status_name(status: u32) -> *const core::ffi::c_char { mudrpc_core::rpc::STATUS_NAMES.get(status as usize).map_or(c"", |n| n).as_ptr() }

    /// Binds `bind`, `"ip:port"`, and sends to `peers`, `"ip:port"` items apart by spaces or commas.
    ///
    /// Null with `*code` `MUD_EINVAL` for text that is not that, or a public address in a build without `internet`, and
    /// `MUD_EFULL` when the system refuses the bind.
    #[no_mangle]
    pub unsafe extern "C" fn mud_udp_new(bind: mud_buf, peers: mud_buf, code: *mut i32) -> *mut mud_udp { // SAFETY: the buffers are read within their lengths; `code` is written through `as_mut`, null ignored
        let mut out = ptr::null_mut();
        let c = guard(|| {
            let (Some(bind), Some(peers)) = (text(bind), text(peers)) else { return MUD_EINVAL };
            let (Ok(bind), Ok(peers)) = (bind.parse(), peers.split([' ', ',']).filter(|p| !p.is_empty()).map(str::parse).collect::<Result<Vec<_>, _>>()) else { return MUD_EINVAL };
            match Udp::new(bind, &peers) {
                Ok(u) => { out = Box::into_raw(Box::new(mud_udp(u))); MUD_OK }
                Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => MUD_EINVAL,
                Err(_) => MUD_EFULL,
            }
        });
        if let Some(r) = code.as_mut() { *r = c; }
        out
    }

    /// Frees a socket from `mud_udp_new`; null is ignored.
    #[no_mangle] pub unsafe extern "C" fn mud_udp_free(u: *mut mud_udp) { if !u.is_null() { drop(Box::from_raw(u)); } } // SAFETY: `u` came from `Box::into_raw` in `mud_udp_new` and is freed once

    /// The bound port, the one chosen for port 0.
    #[no_mangle] pub unsafe extern "C" fn mud_udp_port(u: *mut mud_udp) -> i32 { u.as_ref().and_then(|u| u.0.local_addr().ok()).map_or(MUD_EINVAL, |a| i32::from(a.port())) } // SAFETY: a non-null `u` came from `mud_udp_new` and is not yet freed

    /// Sends `wire` on `topic` to every listed peer: `MUD_OK` when one took it, `MUD_EFULL` when none did (send it again later), `MUD_EINVAL` for a bad argument.
    #[no_mangle]
    pub unsafe extern "C" fn mud_udp_send(u: *mut mud_udp, topic: mud_buf, wire: mud_buf) -> i32 { // SAFETY: a non-null `u` came from `mud_udp_new`; the buffers are read within their lengths
        guard(|| match (u.as_mut(), text(topic), bytes(wire)) {
            (Some(u), Some(t), Some(w)) if !t.is_empty() && t.len() <= 64 && !w.is_empty() && w.len() <= 512 => if u.0.send(t, w, 0).is_ok() { MUD_OK } else { MUD_EFULL },
            _ => MUD_EINVAL,
        })
    }

    /// Listens on `topic`, as a peer's listen item asks: a wire on any other topic is dropped unread.
    #[no_mangle]
    pub unsafe extern "C" fn mud_udp_listen(u: *mut mud_udp, topic: mud_buf) -> i32 { // SAFETY: a non-null `u` came from `mud_udp_new`; `topic` is read within its length
        guard(|| match (u.as_mut(), text(topic)) { (Some(u), Some(t)) if !t.is_empty() && t.len() <= 64 => { let _ = u.0.listen(t); MUD_OK } _ => MUD_EINVAL })
    }

    /// The next wire from a listed peer: 1 (topic and wire point into `scratch`, which holds `MUD_MAX_TOPIC` + `MUD_MSG` bytes), `MUD_NONE` when none waits.
    #[no_mangle]
    pub unsafe extern "C" fn mud_udp_poll(u: *mut mud_udp, scratch: mud_mutbuf, topic: *mut mud_buf, wire: *mut mud_buf) -> i32 { // SAFETY: a non-null `u` came from `mud_udp_new`; `scratch` is written within its length; `topic` and `wire` are written through `as_mut`
        guard(|| {
            let (Some(u), Some(buf), Some(t), Some(w)) = (u.as_mut(), mutable(scratch), topic.as_mut(), wire.as_mut()) else { return MUD_EINVAL };
            if buf.len() < 64 + 512 { return MUD_EINVAL; }
            let Some(Rx::Message { topic, wire }) = u.0.poll() else { return MUD_NONE };
            let (tl, wl) = (topic.len(), wire.len());
            buf[..tl].copy_from_slice(topic.as_bytes());
            buf[tl..tl + wl].copy_from_slice(wire);
            *t = b(&buf[..tl]);
            *w = b(&buf[tl..tl + wl]);
            1
        })
    }

    /// A world from scenario text, one radio per peer, or null with the failing line in `*line` (0 for a missing header).
    #[no_mangle]
    pub unsafe extern "C" fn mud_sim_new(scenario: mud_buf, line: *mut i32) -> *mut mud_sim { // SAFETY: `scenario` is read within its length; `line` is written through `as_mut`, null refused
        let Some(text) = text(scenario) else { if let Some(l) = line.as_mut() { *l = 0; } return ptr::null_mut() };
        match Scenario::parse(text) {
            Ok(s) => { let (world, radios) = sim::world(&s); Box::into_raw(Box::new(mud_sim { world, radios, scenario: s })) }
            Err((l, _)) => { if let Some(out) = line.as_mut() { *out = i32::try_from(l).unwrap_or(i32::MAX); } ptr::null_mut() }
        }
    }

    /// Frees a simulator from `mud_sim_new`; null is ignored.
    #[no_mangle] pub unsafe extern "C" fn mud_sim_free(s: *mut mud_sim) { if !s.is_null() { drop(Box::from_raw(s)); } } // SAFETY: `s` came from `Box::into_raw` in `mud_sim_new` and is freed once
    /// The scenario's `peers` line: the radio count.
    #[no_mangle] pub unsafe extern "C" fn mud_sim_peers(s: *mut mud_sim) -> i32 { simulator(s).map_or(MUD_EINVAL, |s| i32::try_from(s.radios.len()).unwrap_or(i32::MAX)) } // SAFETY: `simulator` refuses null
    /// The latest event's `at_ms`, from which a harness runs on for its tail.
    #[no_mangle] pub unsafe extern "C" fn mud_sim_last_event_ms(s: *mut mud_sim) -> u64 { simulator(s).map_or(0, |s| s.scenario.events.iter().map(|e| e.at_ms).max().unwrap_or(0)) } // SAFETY: `simulator` refuses null
    /// The scenario's event lines, for `mud_sim_event`.
    #[no_mangle] pub unsafe extern "C" fn mud_sim_events(s: *mut mud_sim) -> i32 { simulator(s).map_or(MUD_EINVAL, |s| i32::try_from(s.scenario.events.len()).unwrap_or(i32::MAX)) } // SAFETY: `simulator` refuses null

    /// Event `i` in file order; `text` points into the simulator and lives as long as it does.
    #[no_mangle]
    pub unsafe extern "C" fn mud_sim_event(s: *mut mud_sim, i: i32, out: *mut mud_event) -> i32 { // SAFETY: `simulator` refuses null; `out` is written through `as_mut`, null refused
        let (Some(s), Some(o)) = (simulator(s), out.as_mut()) else { return MUD_EINVAL };
        let Some(e) = usize::try_from(i).ok().and_then(|i| s.scenario.events.get(i)) else { return MUD_EINVAL };
        let (kind, a, b, text) = match &e.kind {
            Kind::Partition(a, b) => (1, *a as u64, *b as u64, EMPTY), Kind::Heal(a, b) => (2, *a as u64, *b as u64, EMPTY), Kind::Jam(a) => (3, *a as u64, 0, EMPTY), Kind::Unjam(a) => (4, *a as u64, 0, EMPTY),
            Kind::Malformed(a, t) => (5, *a as u64, 0, self::b(t.as_bytes())), Kind::Replay(a) => (6, *a as u64, 0, EMPTY), Kind::Flood(a, t, n) => (7, *a as u64, u64::from(*n), self::b(t.as_bytes())),
            Kind::Bandwidth(a, n) => (8, *a as u64, u64::from(*n), EMPTY), Kind::PeerDown(a) => (9, *a as u64, 0, EMPTY), Kind::PeerUp(a) => (10, *a as u64, 0, EMPTY),
            Kind::Clone(a, b) => (11, *a as u64, *b as u64, EMPTY), Kind::Spam(a, n) => (12, *a as u64, u64::from(*n), EMPTY), Kind::Stage => (13, 0, 0, EMPTY),
            Kind::Rotate(a) => (14, *a as u64, 0, EMPTY), Kind::Retire => (15, 0, 0, EMPTY), Kind::Warp(ms) => (16, *ms, 0, EMPTY),
            Kind::Hostile(a) => (17, *a as u64, 0, self::b(sim::HOSTILE.as_bytes())), Kind::Noise(a, n) => (18, *a as u64, u64::from(*n), EMPTY),
            Kind::StaleReboot(a) => (19, *a as u64, 0, EMPTY), Kind::TornSave(a) => (20, *a as u64, 0, EMPTY), Kind::Revoke(a, b) => (21, *a as u64, *b as u64, EMPTY), Kind::Telemetry => (22, 0, 0, EMPTY), Kind::Hold(a, ms) => (23, *a as u64, *ms, EMPTY),
        };
        *o = mud_event { at_ms: e.at_ms, kind, a, b, text };
        MUD_OK
    }
    /// Scenario time, in milliseconds.
    #[no_mangle] pub unsafe extern "C" fn mud_sim_now_ms(s: *mut mud_sim) -> u64 { simulator(s).map_or(0, |s| s.world.borrow().now_ms()) } // SAFETY: `simulator` refuses null
    /// One round of the world: the radio events now due, messages moved and expired, link changes reported, then the clock on `dt_ms`.
    #[no_mangle] pub unsafe extern "C" fn mud_sim_step(s: *mut mud_sim, dt_ms: u64) { if let Some(s) = simulator(s) { s.world.borrow_mut().step(dt_ms); } } // SAFETY: `simulator` refuses null
    /// A host line in the log at the current time: a harness's peer events and host events.
    #[no_mangle] pub unsafe extern "C" fn mud_sim_note(s: *mut mud_sim, note: mud_buf) { if let (Some(s), Some(n)) = (simulator(s), text(note)) { s.world.borrow_mut().note(n); } } // SAFETY: `simulator` refuses null; `note` is read within its length

    unsafe fn radio<'a>(s: *mut mud_sim, r: i32) -> Option<&'a mut Sim> { simulator(s)?.radios.get_mut(usize::try_from(r).ok()?) } // SAFETY: as `simulator`

    /// Radio `r` sends `wire` on `topic`, kept for `ttl_secs`; `MUD_EINVAL` for a bad argument, `MUD_EFULL` when the radio is down.
    #[no_mangle]
    pub unsafe extern "C" fn mud_sim_send(s: *mut mud_sim, r: i32, topic: mud_buf, wire: mud_buf, ttl_secs: u32) -> i32 { // SAFETY: `radio` refuses a null simulator; the buffers are read within their lengths
        match (radio(s, r), text(topic), bytes(wire)) { (Some(x), Some(t), Some(w)) => if x.send(t, w, ttl_secs).is_ok() { MUD_OK } else { MUD_EFULL }, _ => MUD_EINVAL }
    }

    /// Radio `r` listens on `topic`; `MUD_EFULL` when it is down.
    #[no_mangle]
    pub unsafe extern "C" fn mud_sim_listen(s: *mut mud_sim, r: i32, topic: mud_buf) -> i32 { // SAFETY: `radio` refuses a null simulator; `topic` is read within its length
        match (radio(s, r), text(topic)) { (Some(x), Some(t)) => if x.listen(t).is_ok() { MUD_OK } else { MUD_EFULL }, _ => MUD_EINVAL }
    }

    /// Radio `r` sends nothing while `on` is nonzero, and still receives.
    #[no_mangle] pub unsafe extern "C" fn mud_sim_silent(s: *mut mud_sim, r: i32, on: i32) { if let Some(x) = radio(s, r) { x.silent(on != 0); } } // SAFETY: `radio` refuses a null simulator

    /// Next item for radio `r`: 1 message (topic and wire point into `scratch`), 2 on-mesh, 3 off-mesh, `MUD_NONE` when idle.
    #[no_mangle]
    pub unsafe extern "C" fn mud_sim_poll(s: *mut mud_sim, r: i32, scratch: mud_mutbuf, topic: *mut mud_buf, wire: *mut mud_buf) -> i32 { // SAFETY: `radio` refuses a null simulator; `scratch` is the caller's writable bytes, written within its length; `topic` and `wire` are written through `as_mut`
        let (Some(x), Some(buf), Some(t), Some(w)) = (radio(s, r), mutable(scratch), topic.as_mut(), wire.as_mut()) else { return MUD_EINVAL };
        match x.poll() {
            None => MUD_NONE,
            Some(Rx::OnMesh) => 2,
            Some(Rx::OffMesh) => 3,
            Some(Rx::Message { topic, wire }) => {
                let (tl, wl) = (topic.len(), wire.len());
                if buf.len() < tl + wl { return MUD_EINVAL; }
                buf[..tl].copy_from_slice(topic.as_bytes());
                buf[tl..tl + wl].copy_from_slice(wire);
                *t = b(&buf[..tl]);
                *w = b(&buf[tl..tl + wl]);
                1
            }
        }
    }

    /// The world's log as text, one line per entry. Returns the bytes needed; fills `out` when it is large enough.
    #[no_mangle]
    pub unsafe extern "C" fn mud_sim_log(s: *mut mud_sim, out: mud_mutbuf) -> i32 { // SAFETY: `simulator` refuses null; `out` is written within its length
        let Some(s) = simulator(s) else { return MUD_EINVAL };
        let text = s.world.borrow().log().join("\n") + "\n";
        if let Some(o) = mutable(out) { if o.len() >= text.len() { o[..text.len()].copy_from_slice(text.as_bytes()); } }
        i32::try_from(text.len()).unwrap_or(MUD_ETOOLARGE)
    }

    /// Stubs from `.proto` text: `lang` is "rust", "python" or "c".
    ///
    /// Returns bytes written, `MUD_ETOOLARGE` if `out` is too small (nothing written), `MUD_EINVAL` for a parse error with the
    /// line in `*line`, or an unknown language.
    #[no_mangle]
    pub unsafe extern "C" fn mud_gen(lang: mud_buf, proto: mud_buf, out: mud_mutbuf, line: *mut i32) -> i32 { // SAFETY: the buffers are read within their lengths; `out` is written within its; `line` through `as_mut`, null refused
        let (Some(lang), Some(proto), Some(o)) = (text(lang), text(proto), mutable(out)) else { return MUD_EINVAL };
        let schema = match mudrpc_gen::parse::parse(proto) { Ok(s) => s, Err((l, _)) => { if let Some(x) = line.as_mut() { *x = i32::try_from(l).unwrap_or(i32::MAX); } return MUD_EINVAL } };
        let code = match lang {
            "rust" => mudrpc_gen::rust::emit(&schema),
            "python" => mudrpc_gen::python::emit(&schema),
            "c" => match mudrpc_gen::c::emit(&schema) { Ok(c) => c, Err(_) => return MUD_EINVAL },
            _ => return MUD_EINVAL,
        };
        if o.len() < code.len() { return MUD_ETOOLARGE; }
        o[..code.len()].copy_from_slice(code.as_bytes());
        i32::try_from(code.len()).unwrap_or(MUD_ETOOLARGE)
    }
}
