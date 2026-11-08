---
name: mudrpc-swift
description: How to integrate, verify and test the Swift binding for mudRPC, and its drop-in for grpc-swift.
---
mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The Swift binding is one file, `Mud.swift`, over the C ABI, which Swift imports from `bindings/c/mudrpc.h` through `bindings/c/module.modulemap` as the module `CMudRPC`; `MudGrpc.swift` beside it is the drop-in for grpc-swift. Swift 6.1 or later, on macOS 15 or later and on Linux; iOS's library is built in the gate, its simulator run waiting on Xcode.

## Integrate

**1. Build the library.** A Rust toolchain is the one build dependency, until prebuilt binaries ship at 3.0:

```sh
cargo build --release -p mudrpc-c
```

That gives `target/release/libmudrpc.dylib` (`.so` on Linux). In your Swift package, copy `mudrpc.h` and `module.modulemap` into `Sources/CMudRPC/` and declare it `.systemLibrary(name: "CMudRPC", path: "Sources/CMudRPC")`; copy `Mud.swift` into the target that uses it, which depends on `"CMudRPC"`. Link with `swift build -Xlinker -L<dir> -Xlinker -rpath -Xlinker <dir>`, `<dir>` the library's folder, or the same as `linkerSettings` in Xcode.

For iOS, `rustup target add aarch64-apple-ios aarch64-apple-ios-sim` and `cargo rustc --release -p mudrpc-c --target aarch64-apple-ios --crate-type staticlib` (`-sim` for the simulator) give `libmudrpc.a` for the device and the simulator, which the app target links beside `CMudRPC`; the gate builds both.

**2. Make one call.** The smallest complete program: two peers in one process, a call and a reply moved by hand between them.

```swift
import Foundation

let key = (0..<32).map { _ in UInt8.random(in: .min ... .max) }   // the fleet key: 32 bytes from the system's secure generator, provisioned to every peer; never a constant and never a passphrase
let epoch: UInt32 = 1                                   // persisted beside the key; raise it on every boot and restore, write it first
let gcs = try Peer(name: "gcs", key: key, sender: 1, epoch: epoch)       // roster numbers; 0 cannot send
let drone = try Peer(name: "drone", key: key, sender: 2, epoch: epoch)
try drone.listen("nav.Flight")                          // the drone's transport will subscribe to nav.Flight

_ = try gcs.call(now: 0, service: "nav.Flight", method: "Arm", request: [8, 1], ttlSecs: 30)   // a protobuf request, which the generated code encodes for you

let peers = [gcs, drone]
for now in UInt32(0)..<3 {                              // your loop: pump each peer, every tick
  for (i, p) in peers.enumerated() {
    while let o = try p.outgoing(now: now) {
      if !o.listen {                                    // hand o.topic, o.wire, o.ttlSecs to your transport; a listen: subscribe to o.topic
        let incoming = try peers[1 - i].deliver(now: now, topic: o.topic, wire: o.wire)
        switch incoming.kind {
        case .call:
          print("drone:", incoming.method)
          try peers[1 - i].reply(now: now, to: incoming, payload: Array("armed".utf8))
        case .reply:
          print("gcs: status", incoming.status, String(decoding: incoming.bytes, as: UTF8.self))
          exit(0)
        case .dropped: print("dropped:", incoming.dropped)   // log in development, count in the field
        default: break
        }
      }
      try p.sent()                                      // only after the transport took it; if it refused, break instead
    }
    while try p.tick(now: now) != nil { print("expired") }
  }
}
exit(2)
```

It prints `drone: Arm` then `gcs: status 0 armed` and exits 0.

**3. The loop.** Every tick, drain `outgoing(now:)` into your transport, calling `sent()` once it took each item (if it refused, `break`: the item is there next tick); feed what it received to `deliver(now:topic:wire:)`; call `tick(now:)`. `outgoing` also yields listen items, one for the peer's own name (its reply inbox) and one per `listen`, and what a peer hears is what its transport subscribes to. `now` is your uptime in whole seconds, from a clock that only moves forward; the 12-byte id `call` returns is the `id` on the reply that answers it, `seq` and `end` carry streams and `status` is gRPC's status number. `Incoming.kind` is `.call`, `.reply`, `.event` (a publish), `.expired` (from `tick`, with the call's `id`), `.dropped`, `.flood` or `.rollback`, and all but `.expired` and `.dropped` name their `sender`. `save()`, `Peer.restore(_:epoch:)`, `renew(epoch:)` and `close()` keep and end a peer under the host contract at the top of `bindings/c/mudrpc.h`, which every binding shares; every error is a `MudError` whose `code` is that header's. A peer is one thread's: the drop-in's transport owns it and lends it out through `use`.

**4. A signed fleet and the cipher.** `target/release/mud provision fleet 3` writes the fleet key, one device file per sender and the roster, as the C skill describes; a device builds `Peer(name:key:sender:epoch:seed:budget:)` from them and adds `roster(sender:key:)` for every sender it believes, with `Peer.publicKey(seed:)` and `revoke(sender:)`. `hub: true` believes 256 senders in place of 16. `cipher:` is `Peer.cipherDefault`, AES-256-GCM, unless `Peer.cipherChaCha20Poly1305` is passed; a fleet runs one.

**5. UDP.** `Udp(bind: "10.0.0.2:7000", peers: ["10.0.0.3:7000"])` sends each wire to every listed peer and hears only them, on the topics its peer listens to; a public address, `0.0.0.0` included, is `MudError` -1 unless the library was built with the cargo feature `internet` and `MUDRPC_INTERNET=listen-on-public-addresses`, which no release is (`Udp.internet` says which), and a bind the system refuses is -2. `send(_:)` takes an outgoing item and throws -2 when no peer took a wire; `poll()` gives the next `(topic, wire)` or nil.

## Drop-in for gRPC

What every language's drop-in shares, the one-line change, gRPC without HTTP/2, the dependencies and what is not carried, is [From gRPC](../../README.md#from-grpc) in the README.

The product keeps its `.proto`, `protoc-gen-swift` and `protoc-gen-grpc-swift-2` (or the `GRPCProtobufGenerator` build plugin, which runs them for Xcode and SwiftPM), what they generate, and the service it implements. `MudTransport`, in `MudGrpc.swift`, is grpc-swift 2's `ClientTransport` and `ServerTransport` over one peer, with `GRPCCore` from grpc-swift-2 and `GRPCProtobuf` from grpc-swift-protobuf; no HTTP/2, no server stack and no port:

```swift
// was: .http2NIOPosix(target: .ipv4(address: "10.0.0.3", port: 5001), transportSecurity: .plaintext)
let mud = MudTransport(peer: try Peer(name: "gcs", key: fleetKey, sender: 1, epoch: epoch), udp: try Udp(bind: "10.0.0.2:7000", peers: ["10.0.0.3:7000"]))
try await withGRPCClient(transport: mud.client) { grpc in
  let drone = Nav_Drone.Client(wrapping: grpc)
  let reply = try await drone.arm(.with { $0.id = 7 }, options: { var o = CallOptions.defaults; o.timeout = .seconds(30); return o }())
}
```

A server is `GRPCServer(transport: mud.server, services: [DroneService()])` and `try await server.serve()`, in any process; one transport can be a client and one server at once, a second server `failedPrecondition`. `beginGracefulShutdown()` takes no new calls and `serve()` returns once the running and waiting ones have finished; cancelling the task running `serve()`, grpc-swift's shutdown past a deadline, cancels the running and the waiting, their callers `cancelled`. While it lives the peer is reached through `mud.use { try $0.save() }`, the same for a key's `stage`, `rotate` and `retire`, and the transport is closed before the peer. Another radio is a closure, `MudTransport(peer:) { outgoing in ... }`, taking each outgoing item on the pump's thread with no lock held, so it may wait on a thread that calls into the transport, and it hands every wire it receives to `mud.deliver(topic:wire:)`; an error it throws goes to `fault:`, called the same way, and the item is tried for a second, then let go. The transport pumps on a thread of its own, asleep when it has no socket and nothing timed waits; deadlines are grpc-swift's, on `ContinuousClock`, so a wall clock stepped by NTP moves none. `close()` lets the client's `connect()` return; a transport closed under it, by a peer or socket gone, answers every call `unavailable` until it is closed.

- **A timeout is the call's ttl**: `CallOptions.timeout` nil is the transport's `ttlSecs`, 60 by default. `deadlineExceeded` comes at the deadline, but for one past a week, the longest a message lives, which expires at a week. A cancelled task is `cancelled`.
- **A status is gRPC's**: an `RPCError` a handler throws arrives with its code and message, the first 128 bytes kept; any other error as grpc-swift makes it, `unknown`. A server's items reach the caller before its status; a cancel, a deadline or a closed transport cuts them off, unread items included. A reply too large for the wire reaches the caller as `resourceExhausted`, and ends a stream there, its handler's writes throwing. A handler's `context.cancellation` is cancelled when its call's ttl passes, counted from the call's arrival, when its transport closes and when the task running `serve()` is cancelled, and its writes throw `cancelled` from then.
- **`context.remotePeer`** is `mudrpc:` and the caller's roster number, proven per device under a signed fleet.
- **A call on a service the transport does not serve** gets no answer, so a fleet where every node hears every wire still has one server answer it; a method missing from a service it serves is `unimplemented`. A server behind sheds a call its caller has given up on, unrun. The transport owes one caller 1024 replies at most, sixteen full streams, and all its callers 4096, taking them in turn, the longest line giving way past that; a flooding sender's calls go unanswered. Under one fleet key a holder can write a new sender number on every call, and the 32 a second are shared among all of them: sign the fleet where that matters. A service's full name is at most 64 bytes, or `serve()` throws.
- **Refused before sending**: a message past the wire, about 460 bytes, and a 17th call in flight, as `resourceExhausted`; a service name that is not 1 to 64 bytes as `invalidArgument`, the call's own refusal. A call cancelled after it was sent keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
- **Paced**: a server sends 32 replies a second, half what a receiver takes from one sender, each caller in turn; a 63-item stream takes two seconds. A client is not paced: keep it under 32 calls a second, for the same reason, and the 65th in one second is `resourceExhausted`.
- **Not carried**: metadata, empty both ways; client and bidirectional streams, `unimplemented`; a service config's method settings. A retry or hedging policy in `CallOptions` runs each attempt as a new call, each keeping its place among the 16 until its ttl. A server answers once its listen has reached its socket, so start servers before their callers.
- **With a native peer**: a drop-in server ends a stream with a reply that carries no item, end, OK and the detail `end` (`streamEnd`) and sends a unary reply as one item with end, a method registered by hand with no `type` in its descriptor as a stream; a native server's stream ends on its last item, and the drop-in client reads both.

`cargo test -p mudrpc-c --test abi -- --include-ignored the_swift_drop_in` builds `bindings/c/tests/grpc-swift/` in `target/grpc-swift`, its packages pinned to their commits by `Package.resolved`, generates with protoc and both plugins, and runs a grpc-swift client and service over two peers, over UDP and against a native server through every case above, and step 2's program.

## Verify

- `Peer.restore` refuses bytes of neither shape's size as `MudError` -4 before touching the library, and a closed peer answers -4 at every call, its counts 0; `close()` waits for an entry another thread is in to end, and the block's memory lives as long as the `Peer`.
- Every `outgoing` item reaches the transport before `sent()`, which takes the item the last `outgoing` showed and nothing else.
- `deliver` never throws for bad bytes: it returns a `.dropped` verdict with a reason.
- Two threads on one peer is `MudError` -10 with nothing done, a detector of the host's fault and not a lock: hold a peer on one thread, or let `MudTransport` hold it.
