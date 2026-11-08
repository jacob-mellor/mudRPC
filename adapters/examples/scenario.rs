//! Run a scenario file through the standard harness and print the log.
//!   cargo run -p mudrpc-adapters --example scenario -- scenarios/partition-heal.scenario [--signed] [--gcm] [--peer-lines]
#![warn(clippy::pedantic, clippy::nursery)]
use mudrpc_adapters::{harness, sim::{self, Scenario}};

fn main() {
    let path = std::env::args().nth(1).expect("usage: scenario <file>");
    let text = std::fs::read_to_string(&path).expect("read scenario");
    let s = match Scenario::parse(&text) { Ok(s) => s, Err((line, msg)) => { eprintln!("{path}:{line}: {msg}"); std::process::exit(2) } };
    let flag = |f: &str| std::env::args().any(|a| a == f);
    let log = match (flag("--gcm"), flag("--signed")) {
        (false, false) => harness::run(&s),
        (false, true) => harness::run_signed(&s),
        (true, false) => harness::run_gcm(&s),
        (true, true) => harness::run_gcm_signed(&s),
    };
    for line in if flag("--peer-lines") { sim::peer_lines(&log) } else { log } { println!("{line}"); }
}
