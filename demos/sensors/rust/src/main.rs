//! Sensor telemetry over UDP in Rust: a gateway and two sensors, three peers over UDP in one loop, with the messages
//! `mud gen` writes from ../sensor.proto at build time.
mod sensor { include!(concat!(env!("OUT_DIR"), "/sensor.rs")); }
use mudrpc_adapters::{transport::pump, udp::Udp};
use mudrpc_core::{rpc::{Incoming, Peer}, seal::Key};
use sensor::{Ack, Encode, Offset, Reading, Sensor::{self, Request}};
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

type P = Peer<Key, 512, 16, 64, 16, 16>;
const READINGS: &str = "sensors.Readings";

fn degrees(centi: i32) -> String { format!("{:.2}C", f64::from(centi) / 100.0) }

/// A peer and its socket, the other two listed.
fn link(name: &str, sender: u32, key: [u8; 32], ports: &[SocketAddr]) -> (P, Udp) {
    let i = sender as usize - 1;
    let others: Vec<SocketAddr> = ports.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, a)| *a).collect();
    (P::new(name, Key::new(key), sender, 1).unwrap(), Udp::new(ports[i], &others).unwrap())
}

fn publish(p: &mut P, sensor: &str, centi_degrees: i32) {
    let mut buf = [0u8; 512];
    let n = Reading { sensor: sensor.into(), centi_degrees }.encode(&mut buf).unwrap();
    p.publish(0, READINGS, &buf[..n], 60).unwrap();
}

fn main() {
    let key: [u8; 32] = std::array::from_fn(|i| i as u8 + 1); // the fleet key: a product provisions 32 random bytes once and keeps them
    let ports: Vec<SocketAddr> = { let s: Vec<_> = (0..3).map(|_| UdpSocket::bind("127.0.0.1:0").unwrap()).collect(); s.iter().map(|s| s.local_addr().unwrap()).collect() }; // three free ports, let go here
    let (mut gw, mut gu) = link("gateway", 1, key, &ports);
    let (mut s1, mut u1) = link("sensor-1", 2, key, &ports);
    let (mut s2, mut u2) = link("sensor-2", 3, key, &ports);
    gw.listen(READINGS).unwrap();
    s2.listen(Sensor::TOPIC).unwrap();
    publish(&mut s1, "sensor-1", 2150);
    publish(&mut s2, "sensor-2", 1900);
    let (start, mut readings, mut done) = (Instant::now(), Vec::new(), false);
    while !done {
        let now = start.elapsed().as_secs() as u32;
        pump(&mut gw, &mut gu, now, |gw, i| match i {
            Incoming::Event { bytes, .. } => {
                readings.push(Reading::decode(bytes).unwrap());
                if readings.len() == 2 {
                    readings.sort_by(|a, b| a.sensor.cmp(&b.sensor));
                    for r in &readings { println!("reading {} {}", r.sensor, degrees(r.centi_degrees)); }
                    Sensor::Calibrate(gw, now, &Offset { centi_degrees: 50 }, 10).unwrap();
                }
            }
            Incoming::Reply { bytes, .. } => { let a = Ack::decode(bytes).unwrap(); println!("calibrated {} by {}", a.sensor, degrees(a.offset_centi_degrees)); done = true; }
            _ => {}
        }).unwrap();
        pump(&mut s1, &mut u1, now, |_, _| {}).unwrap();
        pump(&mut s2, &mut u2, now, |s2, i| if let Incoming::Call(c) = i {
            if let Some(Ok(Request::Calibrate(o))) = Sensor::request(&c) { Sensor::respond(s2, now, &c, 0, 0, "", &Ack { sensor: "sensor-2".into(), offset_centi_degrees: o.centi_degrees }, true).unwrap(); }
        }).unwrap();
        std::thread::sleep(Duration::from_millis(3));
    }
}
