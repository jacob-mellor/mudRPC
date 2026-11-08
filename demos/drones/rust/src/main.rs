//! gRPC drone telemetry in Rust: a ground station and a mock drone, two peers over UDP on one machine, through the stubs
//! `mud gen` writes from ../drone.proto at build time.
mod drone { include!(concat!(env!("OUT_DIR"), "/drone.rs")); }
use drone::{ArmReply, ArmRequest, Drone::{self, Request}, LandReply, LandRequest, Sample, TelemetryRequest};
use mudrpc_adapters::{transport::pump, udp::Udp};
use mudrpc_core::{envelope::Id, rpc::{status_name, Call, Incoming, Peer}, seal::Key};
use std::net::UdpSocket;
use std::time::{Duration, Instant};

type P = Peer<Key, 512, 16, 64, 16, 16>;
const PILOT: &str = "gcs";

struct Link { peer: P, udp: Udp }
struct Reply { seq: u32, status: u32, detail: String, bytes: Vec<u8> }

/// The mock drone answers a call from its one state, armed or not.
fn serve(d: &mut P, now: u32, armed: &mut bool, c: &Call) {
    match Drone::request(c) {
        Some(Ok(Request::Arm(_))) => { *armed = true; Drone::respond(d, now, c, 0, 0, "", &ArmReply { armed: true, drone: "drone-1".into() }, true).unwrap(); }
        Some(Ok(Request::Telemetry(t))) => for i in 0..t.samples {
            let n = i as i32;
            Drone::respond(d, now, c, i, 0, "", &Sample { seq: i, altitude_m: 10 * (n + 1), battery_pct: 100 - 2 * n }, i + 1 == t.samples).unwrap();
        },
        Some(Ok(Request::Land(_))) if !*armed => Drone::respond(d, now, c, 0, 9, "not armed", &LandReply::default(), true).unwrap(),
        Some(Ok(Request::Land(_))) => { *armed = false; Drone::respond(d, now, c, 0, 0, "", &LandReply { landed: true }, true).unwrap(); }
        _ => {}
    }
}

/// Pumps both peers until the call `id` is answered in full; its replies, by seq.
fn answer(gcs: &mut Link, drone: &mut Link, armed: &mut bool, id: Id, start: Instant) -> Vec<Reply> {
    let (mut got, mut end) = (Vec::new(), None);
    while end != Some(got.len()) {
        let now = start.elapsed().as_secs() as u32;
        pump(&mut gcs.peer, &mut gcs.udp, now, |_, i| match i {
            Incoming::Reply { id: r, seq, end: e, status, detail, bytes, .. } if r == id => { got.push(Reply { seq, status, detail: detail.into(), bytes: bytes.into() }); if e { end = Some(seq as usize + 1); } }
            Incoming::Expired { id: r } if r == id => panic!("the call expired"),
            _ => {}
        }).unwrap();
        pump(&mut drone.peer, &mut drone.udp, now, |d, i| if let Incoming::Call(c) = i { serve(d, now, armed, &c); }).unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    got.sort_by_key(|r| r.seq);
    got
}

fn main() {
    let key: [u8; 32] = std::array::from_fn(|i| i as u8 + 1); // the fleet key: a product provisions 32 random bytes once and keeps them
    let (ga, da) = { let (a, b) = (UdpSocket::bind("127.0.0.1:0").unwrap(), UdpSocket::bind("127.0.0.1:0").unwrap()); (a.local_addr().unwrap(), b.local_addr().unwrap()) }; // two free ports, let go here
    let mut gcs = Link { peer: P::new("gcs", Key::new(key), 1, 1).unwrap(), udp: Udp::new(ga, &[da]).unwrap() };
    let mut drone = Link { peer: P::new("drone-1", Key::new(key), 2, 1).unwrap(), udp: Udp::new(da, &[ga]).unwrap() };
    drone.peer.listen(Drone::TOPIC).unwrap();
    let (start, mut armed) = (Instant::now(), false);
    let mut ask = |call: fn(&mut P, u32) -> Id| { let id = call(&mut gcs.peer, start.elapsed().as_secs() as u32); answer(&mut gcs, &mut drone, &mut armed, id, start) };
    let land = ask(|p, now| Drone::Land(p, now, &LandRequest::default(), 10).unwrap()).remove(0);
    println!("land: {} {}", status_name(land.status), land.detail);
    let armed = ask(|p, now| Drone::Arm(p, now, &ArmRequest { pilot: PILOT.into() }, 10).unwrap()).remove(0);
    println!("armed {} for {PILOT}", ArmReply::decode(&armed.bytes).unwrap().drone);
    let samples: Vec<String> = ask(|p, now| Drone::Telemetry(p, now, &TelemetryRequest { samples: 3 }, 10).unwrap()).iter().map(|r| { let s = Sample::decode(&r.bytes).unwrap(); format!("{}:{}m {}%", s.seq, s.altitude_m, s.battery_pct) }).collect();
    println!("telemetry {}", samples.join(" "));
    let landed = ask(|p, now| Drone::Land(p, now, &LandRequest::default(), 10).unwrap()).remove(0);
    if LandReply::decode(&landed.bytes).unwrap().landed { println!("landed"); }
}
