//! A product's UDP: wires between listed peers, on private addresses, or on public ones in a build with the cargo
//! feature `internet`.
//!
//! A send goes to every listed peer, and each keeps what its peer listens to, a wire on any other topic dropped unread; it is done when one peer took it, so one
//! unreachable peer stalls nothing. A datagram from an address not listed, or one no wire can be, is dropped unread; a
//! `poll` reads at most 1024, so a flood cannot spin, and a stranger flooding faster than that a round outruns the
//! honest wires behind it, which is the firewall's to stop. No routing and no store-and-forward: those are a mesh's.
//! Without `internet` the check that admits a public address is not compiled, so no setting can open one; `build.rs`
//! refuses the feature without the builder's own `MUDRPC_INTERNET`, and a socket on a public address says so on
//! standard error, where no build hides it.
//! Datagram: `topic_len(1) ‖ topic ‖ wire`.
use crate::transport::{Rx, Transport};
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};

/// The longest datagram: the length byte, a 64-byte topic and the 512-byte wire.
pub const MAX_DATAGRAM: usize = 1 + 64 + 512;

/// Whether this build may bind or send to a public address.
pub const INTERNET: bool = cfg!(feature = "internet");

/// Datagrams one `poll` reads before it answers none.
const BOUND: usize = 1024;

#[cfg(not(feature = "internet"))]
const fn admitted(ip: IpAddr) -> bool { crate::transport::private(ip) }
#[cfg(feature = "internet")]
fn admitted(ip: IpAddr) -> bool {
    if !crate::transport::private(ip) { eprintln!("mudrpc: {ip} is a public address: this socket listens on, or sends to, the internet"); }
    true
}

/// One socket and the peers it sends to and hears from.
pub struct Udp {
    sock: UdpSocket,
    peers: Vec<(IpAddr, u16)>,
    to: Vec<SocketAddr>,
    listens: Vec<String>,
    buf: [u8; MAX_DATAGRAM + 1],
    silent: bool,
}

impl Udp {
    /// Binds `addr` (port 0 allowed) and sends to `peers`, at least one. Refuses a public address, either, in a build
    /// without `internet`; `0.0.0.0` and `::` are public, since they bind every interface.
    pub fn new(addr: SocketAddr, peers: &[SocketAddr]) -> io::Result<Self> {
        if peers.is_empty() { return Err(io::Error::new(io::ErrorKind::InvalidInput, "no peer listed: a socket that hears no one")); }
        if let Some(a) = std::iter::once(&addr).chain(peers).find(|a| !admitted(a.ip())) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{a} is public, and this build binds and sends to private addresses alone; the cargo feature `internet` builds one that may")));
        }
        let sock = UdpSocket::bind(addr)?;
        sock.set_nonblocking(true)?;
        let v6 = addr.is_ipv6();
        let to = peers.iter().map(|p| match (v6, p.ip()) { (true, IpAddr::V4(v4)) => SocketAddr::new(IpAddr::V6(v4.to_ipv6_mapped()), p.port()), _ => *p }).collect();
        Ok(Self { sock, peers: peers.iter().map(|p| (p.ip().to_canonical(), p.port())).collect(), to, listens: Vec::new(), buf: [0; MAX_DATAGRAM + 1], silent: false })
    }

    /// The address bound, its port the one chosen for port 0.
    pub fn local_addr(&self) -> io::Result<SocketAddr> { self.sock.local_addr() }
}

impl Transport for Udp {
    type Error = io::Error;

    fn send(&mut self, topic: &str, wire: &[u8], _ttl_secs: u32) -> Result<(), io::Error> {
        if self.silent { return Err(io::Error::new(io::ErrorKind::NotConnected, "silent")); }
        let tl = u8::try_from(topic.len()).unwrap_or(0);
        if tl == 0 || tl > 64 || wire.is_empty() || wire.len() > 512 { return Err(io::Error::new(io::ErrorKind::InvalidInput, "a topic is 1 to 64 bytes and a wire 1 to 512")); }
        let mut d = [0u8; MAX_DATAGRAM];
        d[0] = tl;
        d[1..=topic.len()].copy_from_slice(topic.as_bytes());
        d[1 + topic.len()..1 + topic.len() + wire.len()].copy_from_slice(wire);
        let n = 1 + topic.len() + wire.len();
        let (mut sent, mut why) = (false, None);
        for p in &self.to {
            match self.sock.send_to(&d[..n], p) { Ok(_) => sent = true, Err(e) => why = Some(e) }
        }
        if sent { Ok(()) } else { Err(why.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no peer took it"))) }
    }

    fn listen(&mut self, topic: &str) -> Result<(), io::Error> {
        if !self.listens.iter().any(|t| t == topic) { self.listens.push(topic.to_string()); }
        Ok(())
    }

    fn poll(&mut self) -> Option<Rx<'_>> {
        let mut found = None;
        for _ in 0..BOUND {
            let (n, src) = match self.sock.recv_from(&mut self.buf) {
                Ok(r) => r,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => continue, // an earlier send's ICMP error, as Windows reports it here: this datagram, not the socket
            };
            if !self.peers.contains(&(src.ip().to_canonical(), src.port())) { continue; }
            let tl = usize::from(self.buf[0]);
            if n > MAX_DATAGRAM || tl == 0 || tl > 64 || n < 2 + tl || n - 1 - tl > 512 { continue; }
            let Ok(topic) = std::str::from_utf8(&self.buf[1..=tl]) else { continue };
            if !self.listens.iter().any(|t| t == topic) { continue; } // the transport's half of the contract: a wire on a topic this radio listens to
            found = Some((tl, n));
            break;
        }
        let (tl, n) = found?;
        let topic = std::str::from_utf8(&self.buf[1..=tl]).ok()?;
        Some(Rx::Message { topic, wire: &self.buf[1 + tl..n] })
    }

    fn silent(&mut self, on: bool) { self.silent = on; }
}
