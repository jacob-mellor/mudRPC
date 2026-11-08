---
name: mudrpc-js
description: How to integrate, verify and test the JavaScript binding for mudRPC.
---
mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The JavaScript binding is one file, `mud.js`, an ES module over the C ABI compiled to WebAssembly, with no package, no bundler and no TypeScript step: copy it into your project and your IDE sees every type and comment from the JSDoc. Node 22 or later, or a browser.

## Integrate

From a clone to a call answered over the simulator, then your transport, your `.proto` and a signed fleet.

**1. Build the library.** A Rust toolchain is the one build dependency, until prebuilt binaries ship at 3.0:

```sh
cargo build --release -p mudrpc-c --target wasm32-unknown-unknown && cargo build --release -p mudrpc-gen
```

That gives the wasm and `target/release/mud`. Copy `bindings/js/mud.js` into your project. Put it next to `mud.js` under that name, or point `MUDRPC_WASM` at it under Node; a browser fetches it from beside `mud.js`. From 3.0, `npm install mudrpc` gives the same: `mud.js`, `mud-connect.js` and the wasm, `import { Peer } from 'mudrpc'`, and this skill copied under your project's `.claude/skills/` when that folder exists.

**2. Make one call.** The smallest complete program: two peers in one process, a call and a reply moved by hand between them.

```js
import { Peer } from './mud.js';

function main() {
  const key = crypto.getRandomValues(new Uint8Array(32));      // the fleet key: 32 bytes from the OS, provisioned to every peer; never a constant and never a passphrase (32 equal bytes are refused, other constants are not)
  const epoch = 1;                                             // persisted beside the key; raise it on every boot and restore, write it first
  const gcs = new Peer('gcs', key, 1, epoch), drone = new Peer('drone', key, 2, epoch);   // roster numbers; 0 cannot send; the cipher is mud.js's CIPHER_DEFAULT
  drone.listen('nav.Flight');                                         // the drone's transport will subscribe to nav.Flight
  const id = gcs.call(0, 'nav.Flight', 'Arm', Uint8Array.of(8, 1), 30);   // now 0, ttl 30 s, a protobuf request; the reply carries this id
  const peers = [gcs, drone];
  for (let now = 0; now < 3; now++)                            // your loop: pump each peer, every tick
    for (let i = 0; i < 2; i++) {
      const p = peers[i];
      for (let o; (o = p.outgoing(now)); p.sent()) {           // sent() only after the transport took it; if it refused, break instead
        if (o.listen) continue;                                // listen: the transport subscribes to o.topic; else hand o.topic, o.wire, o.ttlSecs to it
        const incoming = peers[1 - i].deliver(now, o.topic, o.wire);
        if (incoming.kind === 'call') {
          console.log('drone: ' + incoming.method);
          peers[1 - i].reply(now, incoming, 0, new TextEncoder().encode('armed'));
        } else if (incoming.kind === 'reply' && incoming.id.every((b, k) => b === id[k])) {   // the reply to our call
          console.log('gcs: status ' + incoming.status + ' ' + new TextDecoder().decode(incoming.bytes));
          return 0;
        } else if (incoming.kind === 'dropped') console.log('dropped: ' + incoming.dropped);   // log in development, count in the field
      }
      while (p.tick(now)) console.log('expired');
    }
  return 2;
}
process.exitCode = main();
```

`node example.js` with `mud.js` and `mudrpc.wasm` beside it. Expected output: `drone: Arm` then `gcs: status 0 armed`, exit 0.

**3. The loop.** Every tick, drain `outgoing()` into your transport, calling `sent()` once it took each item (if it refused, `break`: the item is there next tick); feed what it received to `deliver()`; call `tick()`. `outgoing()` also yields listen items, one for the peer's own name (its reply inbox) and one per `listen()`, and what a peer hears is what its transport subscribes to. `now` is your uptime in seconds; the 12-byte `Uint8Array` id returned by `call` is the `id` on the reply that answers it, compared byte for byte, never with `===`, `seq` and `end` carry streams and `status` is gRPC's status number. `incoming.kind` is `'call'`, `'reply'`, `'event'` (a publish), `'expired'` (from `tick()`, with the call's `id`), `'dropped'`, `'flood'` or `'rollback'`, all but `'expired'` and `'dropped'` naming their `sender`, and payloads are `Uint8Array`, a call's request and a reply's payload both in `bytes`. `save()`, `Peer.restore(saved, epoch)`, `renew(epoch)` and `close()` keep and end a peer under the host contract at the top of `bindings/c/mudrpc.h`, which every binding shares.

**4. Over the simulator.** Replace the hand-moving with the simulator, `new Sim(text)` from `mud.js`, one radio per peer in place of your transport. The text is a scenario in the format `adapters/SKILL.md` gives, `"seed 1\npeers 2\nprofile reticulum\n0 partition 0 1\n30000 heal 0 1\n"` cutting the two radios apart until 30 s, so a call at 0 with ttl 60 is answered after the heal and one with ttl 10 expires; `events()` lists its host events as `{ atMs, kind, a, b, text }` for your harness to perform; a bad line throws an `Error` naming it. Radio `i` is your `i`th peer, 0 to `peers` − 1, whatever its sender number. Each round, for each peer `i`: hand its outgoing items to `send(i, o.topic, o.wire, o.ttlSecs)` or `listen(i, o.topic)` (false while the radio is down, a `peer_down` row, not a partition, whose wire the mesh holds: `break`, no `sent()`), `deliver()` each `poll(i)` result with `kind === 1` until null (2 is on-mesh, 3 off-mesh), `tick()`; then `step(100)`. `now` is `Math.floor(nowMs() / 1000)`; `note(text)` writes a host line and `log()` returns the world's log; `peers` and `lastEventMs` come from the text.

**5. Your transport.** In the simulator's place your transport takes each send item and listen topic, and what it receives goes to deliver; its rungs are below. On Node over an IP network it can be the binding's own, `await Udp.open('10.0.0.2:7000', '10.0.0.3:7000')`, with `send(item)`, a listen among them, and `poll()`, which yields only a listed peer's wires on a topic listened to, into `deliver`, or given to a `MudTransport`, which pumps it every 10 ms. It binds and sends to private, loopback or link-local addresses alone, always: `0.0.0.0` is public, since it binds every interface, and JavaScript has no build to opt into more, so a public address takes the native library built with the cargo feature `internet`. Reticulum's and DTN's adapters are their own people's, against `adapters/CONFORMANCE.md`.

**6. Your `.proto`.** If the product has Connect code, keep it and read "Drop-in for gRPC" below. Otherwise `mud gen` has no JavaScript output yet; encode request and reply bytes with your protobuf library of choice. The wire is protobuf; only the payload is yours. A proto service `Flight` in `package nav` is the service string `'nav.Flight'`, which the server listens on, with the rpc name as the method; `(mud.ttl_secs)` is the `ttlSecs` argument; a method with `(mud.topic)` is `publish` on that topic, arrives as `'event'` and has no reply.

**7. Streams.** Answer one call with `reply(now, call, status, payload, end, seq, detail)`, `detail` the status's text and `''` by default, for `seq` 0, 1, 2… below 64 (above it a `MudError`, `code === -3`), `end` true on the last item only; it is the default, so a unary reply is seq 0 with `end`. The items arrive in any order, each a `'reply'` with its `seq` and `end`, and the call completes once every `seq` up to the lowest `end` it has seen has arrived, or is `'expired'` at its ttl with one missing. Send `end` once: an item above the lowest `end` is `dropped === 'seq'`, a redelivered item `'duplicate'`, and anything after completion `'noPending'`.

**8. A signed fleet** (SPEC.md layer 1).

```sh
target/release/mud provision fleet 3
```

That writes `fleet/fleet.key` (`key <hex>`, once), `fleet/device-1` to `fleet/device-3` (`sender <n>`, `epoch 1`, `seed <hex>`, owner-only, each device's one secret) and `fleet/roster` (`<sender> <public key hex>` a line), 32-byte values in lowercase hex; run it again to add devices, numbered after the highest, and `--key` in place of the count for a rotation's next key, `fleet/fleet.key.next`; no file is ever overwritten. A device gets the fleet key, its own device file and the roster, and boots `new Peer(name, key, sender, epoch, seed, budget)` from them (each hex value as a 32-byte `Uint8Array`), then a `roster(sender, key)` line for every sender it believes; its epoch starts at the file's and is raised under the host contract. `new Peer(name, key, sender, epoch, seed, budget)` with this sender's 32-byte Ed25519 seed and the signature checks per second this host affords (default 1000, at least 1; a wire past them is `'dropped'` with `dropped === 'open'`, `status` 6), a `roster(sender, key, next)` line for every sender using `Peer.publicKey(seed)`, `revoke(sender)` to remove one. A peer believes 16 senders and the 17th line throws `MudError` with `isFull`, so a ground station that must believe a fleet is built with `hub` true (the seventh argument), the same peer with 256 lines, 256 replay windows and 1024 remembered messages in a larger block. Its wire is version 4 (2 under ChaCha20-Poly1305): an unsigned peer gives it `'dropped'` with `dropped === 'version'` and `status === 4`, and a signed peer an unsigned wire the same with `status === 3`, so one fleet key never serves both. A discovered key or seed: SECURITY.md, "If a key is discovered".

`cipher`, the eighth argument, is `CIPHER_DEFAULT` unless passed, AES-256-GCM; `CIPHER_CHACHA20_POLY1305` there builds any of these peers under ChaCha20-Poly1305 instead: the same sizes and calls, wire version 1, or 2 signed, so a peer of the other cipher gives its wires `dropped === 'version'` with that version in `status`.

**9. Verify.** `cargo test -p mudrpc-c --test abi -- --include-ignored the_js_skill_example_runs` runs step 2's program and every row of the table below.

## Drop-in for gRPC

What every language's drop-in shares, the one-line change, gRPC without HTTP/2, the dependencies and what is not carried, is [From gRPC](../../README.md#from-grpc) in the README.

The product keeps its `.proto`, `protoc-gen-es` and what it generates, and the Connect client and services it already has. `mud-connect.js`, beside `mud.js`, is a Connect `Transport` and the router for the services added to it, with `@bufbuild/protobuf` and `@connectrpc/connect`, whose types they are; no HTTP server and no port:

```js
import { createClient } from '@connectrpc/connect';
import { Drone } from './gen/drone_pb.js';
import { Peer, Udp } from './mud.js';
import { MudTransport } from './mud-connect.js';

// was: const transport = createConnectTransport({ baseUrl: 'https://drone:5001' });
const transport = new MudTransport(new Peer('gcs', fleetKey, 1, epoch), await Udp.open('10.0.0.2:7000', '10.0.0.3:7000'));
const drone = createClient(Drone, transport);
const reply = await drone.arm({ id: 7 }, { timeoutMs: 30000 });
```

A server adds the implementation where `connectNodeAdapter` routed it, `transport.routes((router) => router.service(Drone, impl))` or `transport.service(Drone, impl)`, in any Node process. While the transport lives the peer is reached through `transport.use((p) => p.save())`, the same for a key's `stage`, `rotate` and `retire`, and the transport is closed before the peer. Another radio is a function that takes each outgoing item, and it hands every wire it receives to `transport.deliver(topic, wire)`; an exception it throws goes to the `fault` option, and the item is tried for a second, then let go. The pump holds the Node process while calls are in flight or services are served, as a socket would, and lets it go when idle; a finished stream left unread does not hold it. Deadlines are measured on `performance.now()`, so a wall clock stepped by NTP or a resume moves none. A closed transport aborts every handler's `ctx.signal` with `Unavailable`. A radio that is neither a `Udp` nor a function is a `TypeError` when the transport is built.

- **A timeout is the call's ttl**: `timeoutMs` none is the transport's `ttlSecs`, 60 by default, and Infinity is none. `DeadlineExceeded` comes at the deadline, never before it, but for one past a week, the longest a message lives, which expires at a week; 0, negative or NaN is already passed. A `signal` aborted before or during the call is `Canceled`.
- **A status is Connect's**: a `ConnectError` thrown by a handler arrives with its code and message, the first 128 bytes kept; any other throw as `ConnectError.from` makes it, `Unknown` with its message. A server's items reach the caller before its status; a cancel, a timeout or a closed transport cuts them off, unread items included, and a reader that breaks out of a stream ends its call. A reply too large for the wire reaches the caller as `ResourceExhausted`, and ends a stream there, its handler's `ctx.signal` aborting and its generator no longer read. A handler's `ctx.signal` aborts when its call's ttl passes, and `ctx.timeoutMs()` counts it from the call's arrival.
- **`ctx.values.get(CALLER)`** is the caller's roster number, proven per device under a signed fleet; `CALLER` is exported by `mud-connect.js`.
- **A call on a service the transport does not serve** gets no answer, so a fleet where every node hears every wire still has one server answer it; a method missing from a service it serves is `Unimplemented`. A server behind sheds a call its caller has given up on, unrun. A service's full name is 1 to 64 bytes of UTF-8, or `service` throws `InvalidArgument`.
- **Refused before sending**: a message past the wire, about 460 bytes, and a 17th call in flight, as `ResourceExhausted`; a service name that is not 1 to 64 bytes of UTF-8 and a method name that is not UTF-8, a lone surrogate among them, as `InvalidArgument`. A call cancelled after it was sent keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
- **Paced**: a server sends 32 replies a second, half what a receiver takes from one sender, so no edge of a second drops one; a 63-item stream takes two seconds, and a client keeps under 32 calls a second for the same reason. The transport owes one caller 1024 replies at most, sixteen full streams, and all its callers 4096, taking them in turn, the longest line giving way past that; a flooding sender's calls go unanswered. Under one fleet key a holder can write a new sender number on every call, and the 32 a second are shared among all of them: sign the fleet where that matters.
- **Not carried**: headers and trailers, empty both ways; client and bidirectional streams, `Unimplemented`; a transport's interceptors and compression.
- **With a native peer**: a drop-in server ends a stream with a reply that carries no item, end, OK and the detail `end` (`STREAM_END`); a native server's stream ends on its last item, and the drop-in client reads both.

`cargo test -p mudrpc-c --test abi -- --include-ignored the_connect_drop_in` installs the packages into `target/connect` with `npm ci` from `bindings/c/tests/grpc-js/package-lock.json`, each pinned by its integrity hash, generates with protoc and protoc-gen-es, and runs a Connect client and service over two peers, over UDP and against a native server through every case above.

## Verify

- `import './mud.js'` rejects with an error naming the path it tried and the build command if `mudrpc.wasm` is missing, never a crash.
- `MUDRPC_WASM` loads only what it names, with no fallback; set it only to a binary you built.
- Every `outgoing()` item reaches the transport before `sent()`, which takes the item the last `outgoing()` showed and nothing else; with nothing shown it is a no-op.
- `deliver()` never throws for bad bytes: it returns an incoming with `kind === 'dropped'` and a reason.
- One thread drives a peer, which JavaScript keeps: a module instance runs on one thread with no callbacks, so `code === -10`, two threads on one block in the other bindings, arises only on a `Peer` whose call trapped mid-entry: `restore` the last save into a new one.

The host mistakes the contract names, each with what it throws or returns, every row asserted by `the_js_skill_example_runs`:

| Mistake | Answer | Kind |
| --- | --- | --- |
| `restore` or `renew` with an epoch at or below the saved one | `MudError`, `code === -6` | error |
| sender 0 sending | `MudError`, `code === -7`, at the first send | error |
| a key or seed of 32 equal bytes, or of another length | `MudError`, `code === -1` | error |
| `save()` bytes of another length to `restore` | `MudError`, `code === -4` | error |
| a hub's `save()` cut to a leaf's length | `MudError`, `code === -5` | error |
| `hub` true without a seed | `MudError`, `code === -1` | error |
| `save()` bytes retagged to the other cipher | `MudError`, `code === -4` | error |
| saved bytes sent from without a resume | cannot happen: `restore` resumes | none |
| ttl 0, or past a week | sent as 1, or a week | clamp |
| a topic or name over 64 bytes, or a payload over the ceiling | `MudError`, `code === -3` | error |
| an empty name or topic | `MudError`, `code === -1` | error |
| a reply to a call this peer does not have | `deliver()` returns `dropped === 'noPending'` | silent |
| 16 calls pending, or 16 items queued undrained | `MudError`, `isFull`, `code === -2`: decide what not to queue; `pending()` and `queued()` count them | error |
| the 33rd send in one second | `MudError`, `code === -9`: the next second sends | error |

## Test at each rung

- **In-process:** step 2's program, or step 4's simulator. `tests/js` next to the C binding runs every scenario in `scenarios/`, unsigned, `--signed`, `--gcm` and both, and its logs must equal `scenarios/<name>.log` (`<name>.gcm.log` where one is checked in) and the Rust harness's signed runs byte for byte.
- **Loopback (lan):** `cargo test -p mudrpc-adapters -- --include-ignored partition_heal_over_loopback` is the reference behaviour over real sockets.
- **Relay:** the same on a box on your own LAN, never on the internet.

## Known edges

- The host contract every binding shares (the epoch, save, restore, the nonce, the window, the ttl, threads, bounds, codes, close, the cipher) is the list at the top of `bindings/c/mudrpc.h`.
- `counts()` gives `managed`, `rollbacks`, `floods` and `dropped`, saturating and kept across restores.
- `Peer` and `Sim` own wasm memory: `close()` them when done; every later call on a peer throws `MudError` with `code === -1`, and the module's memory only grows. The bytes `save()` gives, and the key you passed in, live in memory JavaScript will not let anyone zero; `close()` zeroes the wasm copy only.
- `mud.js` is an ES module with a top-level `await`, so it and your entry must load as modules: `"type": "module"` in the `package.json` above them (Node 22.7 and later also detect it when there is none); in a CommonJS project rename it `mud.mjs` and `import()` it; `require()` cannot load it. The first `import` is asynchronous and everything after it is synchronous.
- Each call copies its arguments into the module's memory and frees them when it returns; results are copied out, so nothing you hold points into wasm memory.
- A Rust panic cannot be caught in wasm: it surfaces as a `RuntimeError` from the call, not as `MudError` -99, and the module is dead after it; so is a failed allocation, reported as `MudError` "bad memory" before anything is written.
- A payload over the header's ceiling, about 460 bytes, is `MudError` with `code === -3`.
- Byte arguments must be `Uint8Array`; `MudError` "invalid argument" otherwise. Strings are encoded as UTF-8, so a lone surrogate becomes U+FFFD. Numbers cross as unsigned 32-bit: `now` is seconds of uptime, not `Date.now()`; a negative or fractional number is not an error.
