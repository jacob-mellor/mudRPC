//! The transport contract every adapter implements, and the pump that moves bytes between a Peer and a transport.
use mudrpc_core::rpc::{Incoming, Outgoing, Peer};
use mudrpc_core::seal::Sealer;
use std::net::IpAddr;

/// A loopback, private or link-local address, v4 or v6: the only kind `lan` binds, and the only kind `udp` binds or
/// sends to in a build without the feature `internet`.
#[must_use]
pub const fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// What `poll` yields: a wire that arrived on a topic this radio listens to, or a change in whether any path exists.
#[derive(Debug, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub enum Rx<'a> { Message { topic: &'a str, wire: &'a [u8] }, OnMesh, OffMesh }

/// What every adapter implements; `pump` is its one caller.
pub trait Transport {
    /// Why a send or listen failed; the item stays queued for the next round.
    type Error;
    /// Returns at once; delivery is the mesh's business.
    fn send(&mut self, topic: &str, wire: &[u8], ttl_secs: u32) -> Result<(), Self::Error>;
    /// Announce and subscribe, one act.
    fn listen(&mut self, topic: &str) -> Result<(), Self::Error>;
    /// Never blocks. Drain until None.
    fn poll(&mut self) -> Option<Rx<'_>>;
    /// Behave as if no path exists; keep receiving.
    fn silent(&mut self, on: bool);
}

/// One round: outgoing to the transport, inbound to the peer, time to the peer. `on` sees every Incoming with the peer,
/// so a call is answered where it arrives.
///
/// A transport error ends the drain, not the round: the failed item stays queued for the next, and the error is returned
/// after the inbound and the clock have reached the peer.
pub fn pump<T: Transport, S: Sealer, const MSG: usize, const PENDING: usize, const SEEN: usize, const SENDERS: usize, const OUT: usize>(
    peer: &mut Peer<S, MSG, PENDING, SEEN, SENDERS, OUT>, t: &mut T, now: u32, mut on: impl FnMut(&mut Peer<S, MSG, PENDING, SEEN, SENDERS, OUT>, Incoming),
) -> Result<(), T::Error> {
    let mut sent = Ok(());
    while let Some(o) = peer.outgoing(now) {
        sent = match o { Outgoing::Send { topic, wire, ttl_secs } => t.send(topic, wire, ttl_secs), Outgoing::Listen { topic } => t.listen(topic) };
        if sent.is_err() { break; }
        peer.sent();
    }
    let mut buf = [0u8; MSG];
    while let Some(rx) = t.poll() {
        if let Rx::Message { topic, wire } = rx {
            let i = peer.deliver(now, topic, wire, &mut buf);
            on(peer, i);
        }
    }
    while let Some(i) = peer.tick(now) { on(peer, i); }
    sent
}
