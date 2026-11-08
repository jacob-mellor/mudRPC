//! What the adapter suites share: the scenario files, a world from a few lines of scenario, a radio's messages.
#![allow(dead_code)] // each suite takes what it needs
use mudrpc_adapters::sim::{self, Scenario};
use mudrpc_adapters::transport::{Rx, Transport};
use std::cell::RefCell;
use std::rc::Rc;

pub const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../scenarios/");

pub fn scenario(peers: usize, profile: &str, events: &str) -> Scenario {
    Scenario::parse(&format!("seed 1\npeers {peers}\nprofile {profile}\n{events}")).unwrap()
}

pub fn world(peers: usize, profile: &str, events: &str) -> (Rc<RefCell<sim::World>>, Vec<sim::Sim>) {
    sim::world(&scenario(peers, profile, events))
}

pub fn run(w: &Rc<RefCell<sim::World>>, ms: u64) { for _ in 0..ms / 100 { w.borrow_mut().step(100); } }

/// The millisecond a log line leads with, read to its first space, so a clock past the column still parses.
pub fn at(l: &str) -> u64 { l.split_whitespace().next().and_then(|t| t.parse().ok()).unwrap_or(0) }

/// A log line without its time.
pub fn text(l: &str) -> &str { l.trim_start().split_once(' ').map_or(l, |(_, t)| t) }

/// What the radio has received, its link changes skipped.
pub fn messages(t: &mut impl Transport) -> Vec<(String, Vec<u8>)> {
    let mut v = Vec::new();
    while let Some(rx) = t.poll() { if let Rx::Message { topic, wire } = rx { v.push((topic.to_string(), wire.to_vec())); } }
    v
}
