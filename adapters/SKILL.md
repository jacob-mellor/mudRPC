---
name: mudrpc-sim
description: How to integrate, verify and test the sim and lan adapters for mudRPC.
---
mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. A Rust product drives `mudrpc-core`'s `Peer` directly; the simulator, `sim`, is where it is tested before anything touches a radio, and the loopback adapter, `lan`, is the next rung, both in the `mudrpc-adapters` crate.

## Integrate

From a clone to a call answered over the simulator, then loopback, your `.proto` and a signed fleet.

**1. Depend on the crates.** A Rust toolchain is the one build dependency: `mudrpc-core`, `mudrpc-adapters` and, for stubs, `mudrpc-gen` as a build dependency, by path or git until packages at 3.0, and no other crate. By path, from a clone:

```toml
[dependencies]
mudrpc-core = { path = "../mudRPC/core" }
mudrpc-adapters = { path = "../mudRPC/adapters" }
```

**2. Make one call over the simulator,** in a product's own tests:

```rust
use mudrpc_adapters::{sim, transport::pump};
use mudrpc_core::{rpc::{Incoming, Peer}, seal::Key};

let s = sim::Scenario::parse("seed 1\npeers 2\nprofile reticulum\n0 partition 0 1\n30000 heal 0 1").unwrap();
let (world, mut radios) = sim::world(&s);
let epoch = 1; // the host persists this beside the key and raises it on every boot and restore
let mut key = [0; 32]; // the fleet key, from the host's random source once at provisioning; never a constant
std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom").unwrap(), &mut key).unwrap();
let mut gcs = Peer::<Key, 512, 16, 64, 16, 16>::new("gcs", Key::new(key), 1, epoch).unwrap();   // roster number 1
let mut drone = Peer::<Key, 512, 16, 64, 16, 16>::new("drone", Key::new(key), 2, epoch).unwrap(); // roster number 2
drone.listen("nav").unwrap();
gcs.call(0, "nav", "Arm", b"", 60).unwrap();
for t in (0..40_000).step_by(100) {
    let now = (t / 1000) as u32;
    pump(&mut gcs, &mut radios[0], now, |_, i| if let Incoming::Reply { status, bytes, .. } = i { println!("gcs: status {status} {} at {t} ms", String::from_utf8_lossy(bytes)) }).unwrap();
    pump(&mut drone, &mut radios[1], now, |drone, i| if let Incoming::Call(c) = i { drone.reply(now, &c, 0, 0, "", b"armed", true).unwrap() }).unwrap();
    world.borrow_mut().step(100);
}
```

It prints `gcs: status 0 armed at 31300 ms`: the partition holds the call, ttl 60, until the heal at 30 s.

**3. The loop.** Each round, `pump(&mut peer, &mut transport, now, |peer, incoming| ...)` drains `outgoing` into the transport, calling `sent` after each item it took, feeds what the transport received to `deliver`, runs `tick`, and hands the peer and every `Incoming` to the closure; a transport error returns at once, leaving the item, the poll and the tick for the next round. Any `Transport` (`send`, `listen`, `poll`, `silent`) plugs in, and the peer's first `Outgoing::Listen` is its own name, its reply inbox. `now` is uptime in seconds; the `Id` from `call` is the id on the `Reply` that answers it, `seq` and `end` carry streams and `status` is gRPC's status number. `Incoming` is `Call`, `Reply`, `Event`, `Expired`, `Flood`, `Rollback` or `Dropped(why)`; match `Call` and answer with `reply`. The block is its own save file: write a copy with `disarm()` called on it, bring it back with `resume(epoch)`, raise a live peer's epoch with `renew(epoch)`, all under the host contract at the top of `bindings/c/mudrpc.h`.

**4. The scenario.** A scenario is a text file: `seed`, `peers`, `profile reticulum|dtn`, then `at_ms kind args` rows (`#` comments). Radio faults the world applies: `partition a b`, `heal a b`, `jam a`, `heal a`, `peer_down a`, `peer_up a`, `bandwidth a bytes_per_sec`, `malformed a topic`, `replay a`, `flood a topic per_second` (to 20 000; past 1000 a step's junk lands at once, one log line a receiver), `noise a per_thousand` (bits flipped per thousand bytes in the peer's copy), `hold a ms` (a relay that keeps each wire it hears, once, and sends it again as it was on the air `ms` later; 0 stops it). Host events the harness performs: `clone a b` (radio b becomes a copy of peer a from an old image; b is empty until its first clone and has no boot image of its own, so a host event its peer performs before then, or a storage fault on it at any time, is refused at its line; a `clone` from it boots its source's roster line), `spam a per_second` (a key holder floods), `stage`, `rotate a`, `retire` (keys), `warp ms` (the clock jumps; a replay floor needs a week; gcs calls every 150 rounds, so a warp off the round grid keeps it, and the round a warp lands on runs again at the new clock, and the run ends 50 s after the later of the last event and the clock a warp left), `hostile a` (a method no terminal should see raw), `stale_reboot a`, `torn_save a` (storage faults), `revoke a b` (peer a drops peer b's roster line), `telemetry` (every drone publishes one position to gcs); while a flood is on, every ten rounds, every peer that took events says how many. The log leads each line with its millisecond, nine wide, and holds a peer's identical lines in one round as one line with `×n`; an event names its sender (`peer0 event "gcs" "pos" from sender 5`), a drop none; a call, publish or reply the peer refuses is a line, `peer2 refused reply Full`; past 100 000 world lines the last line counts the rest by kind, and every peer line is written; past 1 048 576 wires in flight, about 500 MB at 1 024 radios, a send is not queued and is counted there as `full`.

**5. Loopback,** two radios on one machine over real UDP (see `adapters/tests/lan.rs` for the full pattern):

```rust
let start = std::time::Instant::now();
let mut a = mudrpc_adapters::lan::Lan::new(&s, 0, "127.0.0.1:0".parse().unwrap(), 100, start).unwrap();
let mut b = mudrpc_adapters::lan::Lan::new(&s, 1, "127.0.0.1:0".parse().unwrap(), 100, start).unwrap();
let (pa, pb) = (a.local_addr(), b.local_addr());
a.peer(1, pb).unwrap(); b.peer(0, pa).unwrap();   // then pump exactly as above; wait on the wall clock instead of world.step
```

`speed` scales scenario time: 100 means one scenario second per 10 ms of wall clock. Every radio must share `start`.

On a real network the product's transport is `mudrpc_adapters::udp::Udp::new(addr, &peers)`, pumped as above: each wire goes to every listed peer, and a datagram from any other address, or on a topic its peer does not listen to, is dropped unread. It binds and sends to private, loopback or link-local addresses alone; `0.0.0.0` is public, since it binds every interface. A library built with the cargo feature `internet`, and `MUDRPC_INTERNET=listen-on-public-addresses` in the builder's environment, may bind and send to public ones; the feature alone fails the build, since any crate in it could set the feature, and no release is built with it. Cargo shows the build's own warning for the workspace alone, so a socket on a public address says so on standard error and `INTERNET` (`mud_internet()`, `Udp.Internet`) is the check a product makes at start. The variable belongs on the command line of the one build that means it, never in a `.cargo/config.toml`, where every build would go public in silence. Your own transport implements `Transport` and plugs into `pump` the same way; Reticulum's and DTN's adapters are their own people's, against [CONFORMANCE.md](CONFORMANCE.md).

**6. Stubs from your `.proto`.** `mudrpc_gen::build("proto")` in `build.rs` turns every `.proto` in `proto/` into `$OUT_DIR/<stem>.rs`, brought in with `include!(concat!(env!("OUT_DIR"), "/nav.rs"));`, or `mud gen nav.proto > nav.rs` writes it by hand. It gives a struct per message with `encode`/`decode` over `mudrpc_core::codec` and a module per service: `Flight::TOPIC` (`package.Service`, what the server listens on), `Flight::Upload(&mut peer, now, &req)` returning the `Id` (a `ttl_secs` argument unless the method bakes one in), `Flight::request(&call)` decoding a call into `Flight::Request`, and `Flight::respond(&mut peer, now, &call, seq, status, detail, &resp, end)`, `detail` the status's text or `""`. A method with `(mud.topic)` publishes and has no reply. The stubs need `alloc`; a heapless peer hand-writes its messages over `codec`.

**7. Streams.** Answer one `Call` with `reply(now, &call, seq, status, detail, bytes, end)`, `status` gRPC's number and `detail` its text (`""` for none), for `seq` 0, 1, 2… below `MAX_SEQ` (64; above it `Err(Error::TooLarge)`), `end` true on the last item and on a unary reply, which is seq 0. The items arrive in any order, each an `Incoming::Reply` with its `seq` and `end`, and the call completes once every `seq` up to the lowest `end` it has seen has arrived, or is `Expired` at its ttl with one missing. Send `end` once: an item above the lowest `end` is `Dropped::Seq`, a redelivered item `Dropped::Duplicate`, and anything after completion `Dropped::NoPending`.

**8. A signed fleet** (SPEC.md layer 1).

```sh
cargo run --release -p mudrpc-gen --bin mud -- provision fleet 2
```

That writes `fleet.key` (`key <hex>`, written once), `device-<sender>` (`sender <n>`, `epoch 1` and `seed <hex>`, owner-only, the device's one secret) and `roster` (`<sender> <public key hex>` a line), all 32-byte values in lowercase hex; again to add devices, numbered after the highest, and `--key` in place of the count for a rotation's next key, `fleet/fleet.key.next`; no file is ever overwritten. A host boots from its device file, the fleet key and the roster lines it believes: `Signed::<Key, 16>::new(Key::new(key), seed, budget)` from `mudrpc-auth`, `roster(sender, public_key, None)` a line, `Peer::new(name, signed, sender, epoch)`, the epoch raised from 1 under the host contract. `Gcm` from `mudrpc-gcm` in place of `Key` is AES-256-GCM, signed or not, the ABI's default; a fleet runs one cipher. A discovered key or seed: [SECURITY.md, "If a key is discovered"](../SECURITY.md#if-a-key-is-discovered).

**9. Verify.** `cargo test -p mudrpc-adapters --test e2e -- --include-ignored the_adapter_skill_examples_run` compiles every `rust` block above as one program and runs it.

## Verify

- `cargo run -p mudrpc-adapters --example scenario -- scenarios/partition-heal.scenario` prints the reference log; your
  product's peer events on the same scenario should match it line for line (the lines with `peer` and `gcs`).
- Same seed, same log, byte for byte, across two runs. If not, something in your product is reading a clock or an RNG.
- `lan` refuses to bind or send to anything but loopback or a private address; that refusal is a test, not a warning.

The host mistakes the contract names, each with what `Peer` answers in `core/rpc.rs`:

| Mistake | Answer | Kind |
| --- | --- | --- |
| `resume` or `renew` with an epoch at or below the saved one | `Err(Error::Epoch)` | error |
| sender 0 sending | `Err(Error::Unprovisioned)` | error |
| a key or seed of 32 equal bytes | accepted: that refusal is the ABI's, so draw the key from the OS | silent |
| bytes that are not a live peer of this type, a block retagged to the other cipher included | `resume` returns `Err(Error::Corrupt)` | error |
| a copy written with `disarm()` and sent from without `resume` | `Err(Error::Corrupt)` | error |
| `SENDERS` below the sealer's `ROSTER` | refused at compile time | error |
| ttl 0, or past `MAX_TTL` | 1, or `MAX_TTL` | clamp |
| a topic or name over `MAX_TOPIC`, or a message over `MSG` | `Err(Error::TooLarge)` | error |
| an empty name or topic | `Err(Error::Empty)`; a wire on one is `Dropped::Topic` | error |
| a reply to a call this peer does not have | `Incoming::Dropped(Dropped::NoPending)` | silent |
| `PENDING` calls pending, or `OUT` items queued undrained | `Err(Error::Full)`: decide what not to queue; `pending()` and `queued()` count them | error |
| the 33rd send in one second | `Err(Error::Rate)`: the next second sends | error |

A block's length and the hub's seed are types in Rust, and a second thread on one peer is a borrow (`&mut self`), so those mistakes do not compile; the ABI's `MUD_EBUSY` has no Rust twin.

## Test at each rung

- **In-process:** the simulator, every build, seconds.
- **Loopback:** `lan` on 127.0.0.1, two or more processes or one process with several radios, `--include-ignored`. It
  carries the link kinds (`partition`, `heal`, `jam`, `peer_down`, `peer_up`) and every host event; `Lan::new` refuses,
  naming the line, `malformed`, `replay`, `flood`, `noise`, `hold` and `bandwidth`, which only the simulator's `World`
  applies. One process keeps 256 radios in step through 200 link events at SPEED 20 (3.5 s); at SPEED 200 it cannot: the
  round in which 255 drones answer gcs is 130 050 datagrams, 0.4 s of wall clock and 80 s of the scaled clock, so a
  60 s call waiting at its sender for a heal dies before the sender's next turn.
- **LAN and relay:** the same `lan` on real devices on your own network, or the same binary as a relay on a box on your
  LAN. Never on the internet. Reference rig: three Raspberry Pis on Wi-Fi, one as relay; a LilyGO T-Beam running RNode on
  each for the Reticulum rung when its community's adapter exists; one Pixhawk with a CM4 companion for fit-for-purpose.

## Known edges

- The host contract every binding shares (the epoch, save, restore, the nonce, the window, the ttl, threads, bounds, codes, drop, the cipher) is the list at the top of `bindings/c/mudrpc.h`; in Rust its codes are `rpc::Error`'s variants. A relay that holds wires past their ttl (`hold`, relay-holds-past-ttl) is the one way a receiver sees a wire older than its ttl: where it keeps the sender's window or floor the wire never runs twice (`Duplicate`, `Replay` past the window or a week's floor, or `Rollback` where it is of an older epoch and arrives past its ttl from the receiver's first hearing of the newer one), where it never heard the sender it runs once, as any first wire does, and only after a full table refused or evicted the sender's window can it run again, which `pressure()` counted then.
- The Reticulum mock assumes a destination is reachable one second after its `listen` (announce propagation) and that
  messages wait at the sender with no store-and-forward in the mesh. The DTN mock keeps a copy on every radio that
  received it. Both are our best reading of the published protocols and open with a "what we think you do" comment for
  their maintainers to correct; widen the mock, do not patch a test.
- Bandwidth limits apply at the sending radio only; relays are not throttled.
- `lan` shares the scenario clock by construction; on real devices the clocks drift, and a 100 ms step of skew is normal.
