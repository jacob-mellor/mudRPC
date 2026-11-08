//! The signed sealer of SPEC.md layer 1's authentication group: sign, then seal.
//!
//! `Signed<S, ROSTER>` wraps any sealer; the signature is the last 64 bytes of the plaintext, written into the room the
//! peer leaves after the envelope, so nothing on the wire changes shape and a listener sees what it saw before.
//! `open` is the inner tag check; `verify` runs after the peer's public verdicts: budget, roster line, signature.
//! The signed bytes are `nonce ‖ version ‖ len(topic) ‖ topic ‖ envelope`: every field at a fixed place or behind its
//! length, so no two inputs share a signature.
use crate::ed25519::{public_key, sign, small_order, verify};
use mudrpc_core::codec::le;
use mudrpc_core::rpc::tally;
use mudrpc_core::seal::{Error, Opened, Sealer};

const SIG: usize = 64;

/// One sender the receiver will believe: its number, its key, and the key it is moving to (zero: none).
#[repr(C)]
#[derive(Clone, Copy)]
struct Line { sender: u32, key: [u8; 32], next: [u8; 32], live: u8, _pad: [u8; 3] }
const _: () = assert!(size_of::<Line>() == 4 + 32 + 32 + 1 + 3);

/// `ROSTER` senders this receiver verifies.
#[repr(C)]
pub struct Signed<S: Sealer, const ROSTER: usize> { budget: u32, sec: u32, spent: u32, lines: [Line; ROSTER], inner: S, seed: [u8; 32], pk: [u8; 32], _pad: [u8; 4] }

impl<S: Sealer, const ROSTER: usize> Signed<S, ROSTER> {
    /// `seed` is this sender's Ed25519 seed, the one secret added to the fleet key; `budget` is verifications per second,
    /// measured by the host on its own silicon, 0 taken as 1, since a block holding 0 receives nothing and does not resume.
    /// The roster starts empty and refuses everyone.
    pub fn new(inner: S, seed: [u8; 32], budget: u32) -> Self {
        const { assert!(size_of::<Self>() == 12 + ROSTER * size_of::<Line>() + size_of::<S>() + 64 + 4) } // no padding, for an inner sealer of 8n bytes
        let line = Line { sender: 0, key: [0; 32], next: [0; 32], live: 0, _pad: [0; 3] };
        Self { budget: budget.max(1), sec: 0, spent: 0, lines: [line; ROSTER], inner, pk: public_key(&seed), seed, _pad: [0; 4] }
    }
    /// This sender's public key, for the other peers' rosters.
    #[must_use] pub const fn public_key(&self) -> [u8; 32] { self.pk }
    /// The wrapped sealer, for its key events.
    pub const fn inner(&mut self) -> &mut S { &mut self.inner }

    /// A roster line, replacing any for that sender. `Roster` for a key that would make the line forgeable by anyone (not
    /// a point, the identity, small order); `Full` when the roster is.
    pub fn roster(&mut self, sender: u32, key: [u8; 32], next: Option<[u8; 32]>) -> Result<(), Error> {
        let next = next.unwrap_or([0; 32]);
        if small_order(&key) || (next != [0; 32] && small_order(&next)) { return Err(Error::Roster); }
        let i = self.lines.iter().position(|l| l.live == 1 && l.sender == sender).or_else(|| self.lines.iter().position(|l| l.live == 0)).ok_or(Error::Full)?;
        if let Some(l) = self.lines.get_mut(i) { *l = Line { sender, key, next, live: 1, _pad: [0; 3] }; }
        Ok(())
    }
    /// Removes a sender's line: revocation, device by device.
    pub fn revoke(&mut self, sender: u32) { for l in self.lines.iter_mut().filter(|l| l.sender == sender) { l.live = 0; } }

    fn check(&mut self, now: u32, ad: &[u8], nonce: &[u8; 12], plain: &[u8]) -> Result<(), Error> {
        let Some(n) = plain.len().checked_sub(SIG) else { return Err(Error::Short) };
        let sender = le(&nonce[..4]) as u32;
        let (version, topic) = ad.split_at_checked(1).unwrap_or((ad, &[]));
        let Ok(tl) = u8::try_from(topic.len()) else { return Err(Error::Short) }; // the length prefix is one byte; the peer never passes more than MAX_TOPIC
        let line = *self.lines.iter().find(|l| l.live == 1 && l.sender == sender).ok_or(Error::Roster)?;
        let (msg, sig) = plain.split_at(n);
        let Ok(sig) = sig.try_into() else { return Err(Error::Sig) };
        let signed = [nonce, version, &[tl], topic, msg];
        for key in [line.key, line.next].into_iter().filter(|k| *k != [0; 32]) { // a unit per key tried
            let (sec, spent) = tally(now, self.sec, self.spent);
            if spent > self.budget { return Err(Error::Budget); }
            (self.sec, self.spent) = (sec, spent);
            if verify(&key, &signed, sig) { return Ok(()); }
        }
        Err(Error::Sig)
    }
}

impl<S: Sealer, const ROSTER: usize> Sealer for Signed<S, ROSTER> {
    const VERSION: u8 = S::VERSION + 1; // its inner's plus one: 2 over `Key`, 4 over `Gcm`, so a mixed fleet is two `Version(n)` counts
    const TRAILER: usize = SIG;
    const ROSTER: usize = ROSTER;

    fn seal(&mut self, nonce: &[u8; 12], ad: &[u8], plain: &mut [u8], n: usize, out: &mut [u8]) -> Result<usize, Error> {
        let (version, topic) = ad.split_at_checked(1).unwrap_or((ad, &[]));
        let Ok(tl) = u8::try_from(topic.len()) else { return Err(Error::Full) };
        let Some((msg, rest)) = plain.split_at_mut_checked(n) else { return Err(Error::Full) };
        let Some(sig) = rest.get_mut(..SIG) else { return Err(Error::Full) };
        sig.copy_from_slice(&sign(&self.seed, &self.pk, &[nonce, version, &[tl], topic, msg]));
        self.inner.seal(nonce, ad, plain, n + SIG, out)
    }

    fn open(&mut self, ad: &[u8], wire: &[u8], out: &mut [u8]) -> Result<Opened, Error> { self.inner.open(ad, wire, out) }

    fn verify(&mut self, now: u32, ad: &[u8], nonce: &[u8; 12], plain: &mut [u8]) -> Result<(), Error> {
        let r = self.check(now, ad, nonce, plain);
        if r.is_err() { plain.fill(0); }
        r
    }

    fn resume(&mut self) { (self.sec, self.spent) = (0, 0); self.inner.resume(); }

    /// The inner sealer, every flag, no line a block could smuggle in that `roster` would refuse, a budget that receives,
    /// and the public key still the seed's.
    fn intact(&self) -> bool {
        let sound = |l: &Line| l.live == 0 || !small_order(&l.key) && (l.next == [0; 32] || !small_order(&l.next));
        self.inner.intact() && self.lines.iter().all(|l| l.live <= 1 && sound(l)) && self.budget >= 1 && self.pk == public_key(&self.seed)
    }
}

impl<S: Sealer, const ROSTER: usize> Drop for Signed<S, ROSTER> {
    fn drop(&mut self) {
        self.seed = [0; 32];
        core::hint::black_box(&self.seed);
    }
}
