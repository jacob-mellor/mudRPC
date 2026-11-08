//! Plain UDP on a private network, with the same fault model as `sim` deciding what may pass.
//!
//! Every radio runs the same scenario against the same clock and applies its link kinds; the kinds only the simulator's
//! `World` applies (the traffic it injects, a radio's noise, holding and bandwidth) are refused at `new`. The sender alone
//! decides whether a link is up, and the receiver only checks that it is itself alive, so a clock boundary cannot lose a
//! message. A message waits at its sender until a link to each peer exists or its ttl ends. Each `work()` reads at most
//! 64 datagrams and the inbox holds at most 1024, so a flood cannot spin or grow. Refuses to bind anywhere but a private
//! or loopback address, and drops a datagram whose source socket is not the listed peer it names or which is longer than
//! a wire can be.
//! Datagram: `from(1) ‖ topic_len(1) ‖ topic ‖ wire`.
use crate::sim::{Event, Kind, Links, Scenario};
use crate::transport::{private, Rx, Transport};
use std::collections::VecDeque;

/// The longest datagram a peer sends: the two-byte frame, a topic, and the ABI's 512-byte wire, version byte included.
pub const MAX_DATAGRAM: usize = 2 + 64 + 512;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::Instant;

struct Queued { topic: String, wire: Vec<u8>, expires_ms: u64, sent: Vec<bool> }
enum Owned { Message(String, Vec<u8>), OnMesh, OffMesh }

/// One radio as a UDP socket, with the scenario's links and faults decided at the sender.
pub struct Lan {
    sock: UdpSocket,
    id: usize,
    peers: Vec<Option<SocketAddr>>,
    links: Links,
    events: Vec<Event>,
    start: Instant,
    speed: u64,
    listens: Vec<String>,
    queue: VecDeque<Queued>,
    inbox: VecDeque<Owned>,
    silent: bool,
    on_mesh: bool,
    cur: Option<Owned>,
}

impl Lan {
    /// Binds `addr` (port 0 allowed) as radio `id` of the scenario. `speed` scales scenario time: 100 means 1 s of
    /// scenario per 10 ms of wall clock. `start` is the shared instant of t = 0.
    pub fn new(s: &Scenario, id: usize, addr: SocketAddr, speed: u64, start: Instant) -> io::Result<Self> {
        if !private(addr.ip()) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "lan binds only private or loopback addresses")); }
        if s.peers > 256 { return Err(io::Error::new(io::ErrorKind::InvalidInput, "lan carries the radio index in one byte: at most 256 peers")); }
        if let Some(e) = s.events.iter().find(|e| matches!(e.kind, Kind::Malformed(..) | Kind::Replay(_) | Kind::Flood(..) | Kind::Noise(..) | Kind::Hold(..) | Kind::Bandwidth(..))) {
            let kind = format!("{:?}", e.kind).split('(').next().unwrap_or_default().to_lowercase();
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("line {}: `{kind}` is the simulator's to apply, and over loopback the run would be a clean one", e.line)));
        }
        let sock = UdpSocket::bind(addr)?;
        sock.set_nonblocking(true)?;
        let mut events = s.events.clone();
        events.sort_by_key(|e| e.at_ms);
        events.reverse();
        Ok(Self { sock, id, peers: vec![None; s.peers], links: Links::new(s.peers), events, start, speed: speed.max(1), listens: vec![], queue: VecDeque::new(), inbox: VecDeque::new(), silent: false, on_mesh: false, cur: None })
    }

    /// The bound address, port included, for the other radios' `peer`.
    #[must_use] pub fn local_addr(&self) -> SocketAddr { self.sock.local_addr().unwrap() }

    /// Where radio `peer` can be reached.
    pub fn peer(&mut self, peer: usize, addr: SocketAddr) -> io::Result<()> {
        if !private(addr.ip()) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "lan sends only to private or loopback addresses")); }
        *self.peers.get_mut(peer).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no such radio in the scenario"))? = Some(addr);
        Ok(())
    }

    /// Scenario time: wall time since `start`, scaled by `speed`.
    #[must_use] pub fn now_ms(&self) -> u64 { u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX) * self.speed }

    /// Apply due events, flush what can leave, read what arrived.
    #[allow(clippy::cast_possible_truncation)] // `send` refused a topic over 255 bytes
    fn work(&mut self) {
        let now = self.now_ms();
        while self.events.last().is_some_and(|e| e.at_ms <= now) {
            let e = self.events.pop().unwrap();
            self.links.apply(&e.kind);
        }
        let on = self.links.on_mesh(self.id);
        if on != self.on_mesh { self.on_mesh = on; self.inbox.push_back(if on { Owned::OnMesh } else { Owned::OffMesh }); }
        let me = self.id;
        self.queue.retain(|q| q.expires_ms > now);
        if !self.silent && !self.links.is_down(me) {
            for q in &mut self.queue {
                for p in 0..self.peers.len() {
                    if let (false, true, Some(addr)) = (q.sent[p], self.links.link(me, p), self.peers[p]) {
                        let mut d = vec![me as u8, q.topic.len() as u8];
                        d.extend_from_slice(q.topic.as_bytes());
                        d.extend_from_slice(&q.wire);
                        if self.sock.send_to(&d, addr).is_ok() { q.sent[p] = true; }
                    }
                }
            }
        }
        self.queue.retain(|q| q.sent.iter().any(|&s| !s));
        let mut buf = [0u8; MAX_DATAGRAM + 1];
        for _ in 0..64 {
            if self.inbox.len() >= 1024 { break; }
            let Ok((n, src)) = self.sock.recv_from(&mut buf) else { break };
            if n > MAX_DATAGRAM { continue; }
            let Some((&from, rest)) = buf[..n].split_first() else { continue };
            let Some((&tl, rest)) = rest.split_first() else { continue };
            let (from, tl) = (from as usize, tl as usize);
            if self.peers.get(from) != Some(&Some(src)) || self.links.is_down(me) { continue; } // the source must be the listed peer it names; the sender decided the link, and a second reading at a boundary would lose the message
            let Some((topic, wire)) = rest.split_at_checked(tl) else { continue };
            let Ok(topic) = std::str::from_utf8(topic) else { continue };
            if !self.listens.iter().any(|t| t == topic) { continue; }
            self.inbox.push_back(Owned::Message(topic.to_string(), wire.to_vec()));
        }
    }
}

impl Transport for Lan {
    type Error = io::Error;

    fn send(&mut self, topic: &str, wire: &[u8], ttl_secs: u32) -> Result<(), io::Error> {
        if topic.len() > 255 { return Err(io::Error::new(io::ErrorKind::InvalidInput, "topic too long")); }
        let mut sent = vec![false; self.peers.len()];
        sent[self.id] = true;
        self.queue.push_back(Queued { topic: topic.into(), wire: wire.into(), expires_ms: self.now_ms() + u64::from(ttl_secs) * 1000, sent });
        self.work();
        Ok(())
    }

    fn listen(&mut self, topic: &str) -> Result<(), io::Error> {
        self.listens.push(topic.into());
        Ok(())
    }

    fn poll(&mut self) -> Option<Rx<'_>> {
        self.work();
        self.cur = self.inbox.pop_front();
        Some(match self.cur.as_ref()? {
            Owned::Message(t, w) => Rx::Message { topic: t, wire: w },
            Owned::OnMesh => Rx::OnMesh,
            Owned::OffMesh => Rx::OffMesh,
        })
    }

    fn silent(&mut self, on: bool) { self.silent = on; }
}
