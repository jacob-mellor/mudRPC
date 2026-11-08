//! What the core suites share: the peer they test and its sends, the seeded generator, hex.
#![allow(dead_code, clippy::cast_possible_truncation)] // each suite takes what it needs; the generator narrows on purpose
use mudrpc_core::rpc::{Outgoing, Peer};
use mudrpc_core::seal::Key;

pub type N = Peer<Key, 256, 4, 8, 16, 4>;
pub const KEY: [u8; 32] = [7; 32];
/// A provisioned peer: roster number `sender`, first boot.
pub fn peer(name: &str, sender: u32) -> N { N::new(name, Key::new(KEY), sender, 1).unwrap() }
pub const fn from_block(b: &[u8]) -> N { unsafe { core::ptr::read_unaligned(b.as_ptr().cast::<N>()) } }

/// Everything the peer wants sent, taken.
pub fn sends(n: &mut N, now: u32) -> Vec<(String, Vec<u8>)> {
    let mut v = Vec::new();
    while let Some(o) = n.outgoing(now) {
        if let Outgoing::Send { topic, wire, .. } = o { v.push((topic.into(), wire.into())); }
        n.sent();
    }
    v
}

/// xorshift64: one seed, one sequence, on every run.
pub struct Rng(pub u64);
impl Rng {
    pub const fn next(&mut self) -> u64 { let mut x = self.0; x ^= x << 13; x ^= x >> 7; x ^= x << 17; self.0 = x; x }
    pub const fn below(&mut self, n: usize) -> usize { if n == 0 { 0 } else { (self.next() % n as u64) as usize } }
    pub fn fill(&mut self, b: &mut [u8]) { for c in b.chunks_mut(8) { c.copy_from_slice(&self.next().to_le_bytes()[..c.len()]); } }
}
pub fn env(name: &str, default: u64) -> u64 { std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default) }

/// Hex in any spacing or separator; "-" is empty, as Wycheproof writes it.
pub fn hex(s: &str) -> Vec<u8> {
    s.split(|c: char| !c.is_ascii_hexdigit())
        .filter(|t| !t.is_empty())
        .flat_map(|t| (0..t.len()).step_by(2).map(move |i| u8::from_str_radix(&t[i..i + 2], 16).unwrap()))
        .collect()
}
