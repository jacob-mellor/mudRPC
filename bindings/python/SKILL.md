---
name: mudrpc-python
description: How to integrate, verify and test the Python binding for mudRPC.
---
mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The Python binding is one file, `mud.py`, over the C ABI through `ctypes`, with no pip package.

## Integrate

From a clone to a call answered over the simulator, then your transport, your `.proto` and a signed fleet.

**1. Build the library.** A Rust toolchain is the one build dependency, until prebuilt binaries ship at 3.0:

```sh
cargo build --release -p mudrpc-c -p mudrpc-gen
```

That gives `target/release/libmudrpc.dylib` (`.so` on Linux) and `target/release/mud`. Copy `bindings/python/mud.py` beside your script and put the library next to `mud.py`, or point `MUDRPC_LIB` at it. From 3.0, `pip install mudrpc` gives the same: `mud.py`, `mud_grpc.py` and the library for your platform beside them, so `import mud` is all.

**2. Make one call.** The smallest complete program: two peers in one process, a call and a reply moved by hand between them.

```python
import os
import mud

key = os.urandom(32)                                    # the fleet key: 32 bytes from the OS, provisioned to every peer; never a constant and never a passphrase (32 equal bytes are refused, other constants are not)
epoch = 1                                               # persisted beside the key; raise it on every boot and restore, write it first
gcs = mud.Peer("gcs", key, sender=1, epoch=epoch, cipher=mud.CIPHER_DEFAULT)       # roster numbers; 0 cannot send
drone = mud.Peer("drone", key, sender=2, epoch=epoch, cipher=mud.CIPHER_DEFAULT)
drone.listen("nav.Flight")                                     # the drone's transport will subscribe to nav.Flight

call_id = gcs.call(0, "nav.Flight", "Arm", b"\x08\x01", ttl_secs=30)   # now 0; a protobuf request, which the stubs encode for you

peers = [gcs, drone]
for now in range(3):                                    # your loop: pump each peer, every tick
    for i, p in enumerate(peers):
        while (o := p.outgoing(now)) is not None:
            if o.kind == "send":                        # hand o.topic, o.wire, o.ttl_secs to your transport; "listen": subscribe to o.topic
                incoming = peers[1 - i].deliver(now, o.topic, o.wire)
                if incoming.kind == "call":
                    print("drone:", incoming.method)
                    peers[1 - i].reply(now, incoming, status=0, payload=b"armed")
                elif incoming.kind == "reply":
                    print("gcs: status", incoming.status, incoming.bytes.decode())
                    raise SystemExit(0)
                elif incoming.kind == "dropped":
                    print("dropped:", incoming.dropped)  # log in development, count in the field
            p.sent()                                    # only after the transport took it; if it refused, break instead
        while p.tick(now) is not None:
            print("expired")
raise SystemExit(2)
```

`MUDRPC_LIB=/path/to/libmudrpc.dylib python3 example.py` prints `drone: Arm` then `gcs: status 0 armed` and exits 0.

**3. The loop.** Every tick, drain `outgoing()` into your transport, calling `sent()` once it took each item (if it refused, `break`: the item is there next tick); feed what it received to `deliver()`; call `tick()`. `outgoing()` also yields `listen` items, one for the peer's own name (its reply inbox) and one per `listen()`, and what a peer hears is what its transport subscribes to. `now` is an `int`, your uptime in seconds; the 12-byte `bytes` id returned by `call` is the `id` on the reply that answers it, `seq` and `end` carry streams, `status` is gRPC's status number and `mud.status_name(status)` its name. `Incoming.kind` is `call`, `reply`, `event` (a publish), `expired` (from `tick()`, with the call's `id`), `dropped`, `flood` or `rollback`, and all but `expired` and `dropped` name their `sender`. `save()`, `Peer.restore(saved, epoch)`, `renew(epoch)` and `close()` keep and end a peer under the host contract at the top of `bindings/c/mudrpc.h`, which every binding shares.

**4. Over the simulator.** Replace the hand-moving with `mud.Sim(text)`, one radio per peer in place of your transport. The text is a scenario in the format `adapters/SKILL.md` gives: `mud.Sim("seed 1\npeers 2\nprofile reticulum\n0 partition 0 1\n30000 heal 0 1\n")` cuts the two radios apart until 30 s: a call at 0 with ttl 60 is answered after the heal, one with ttl 10 `expired`. `events()` lists its host events as `(at_ms, kind, a, b, text)` for your harness to perform; a bad line raises `ValueError` naming it. Radio `i` is your `i`th peer, 0 to `peers` − 1, whatever its sender number. Each round, for each peer `i`: hand its outgoing items to `send(i, o.topic, o.wire, o.ttl_secs)` or `listen(i, o.topic)` (True when taken; False while the radio is down, a `peer_down` row, not a partition, whose wire the mesh holds: `break`, no `sent()`), `deliver()` each `("message", topic, wire)` from `poll(i)` until None, skipping `("on_mesh",)` and `("off_mesh",)`, the radio's link news, `tick()`; then `step(100)`. `now` is `now_ms // 1000`; `note(text)` writes a host line and `log()` returns the world's log; `peers` and `last_event_ms` come from the text.

**5. Your transport.** In the simulator's place your transport takes each `send` item and `listen` topic, and what it receives goes to `deliver()`; its rungs are below. Reticulum's and DTN's adapters are their own people's, against `adapters/CONFORMANCE.md`. On an IP network it can be the library's own, `mud.Udp("10.0.0.2:7000", "10.0.0.3:7000")`, with `send(item)`, a listen among them, and `poll()`, which yields only a listed peer's wires on a topic listened to, into `deliver`, or given to a `mud_grpc.Channel`, which pumps it every 10 ms. It binds and sends to private, loopback or link-local addresses alone; `0.0.0.0` is public, since it binds every interface. A library built with the cargo feature `internet`, and `MUDRPC_INTERNET=listen-on-public-addresses` in the builder's environment, may bind and send to public ones; the feature alone fails the build, since any crate in it could set the feature, and no release is built with it. Cargo shows the build's own warning for the workspace alone, so a socket on a public address says so on standard error and `mud.Udp.internet()` is the check a product makes at start. The variable belongs on the command line of the one build that means it, never in a `.cargo/config.toml`, where every build would go public in silence.

**6. Stubs from your `.proto`.** If the product has grpcio code, keep it and read "Drop-in for gRPC" below. Otherwise, `nav = mud.load_proto("nav.proto")` at runtime gives a typed module, every field and function annotated, with a dataclass per message (`encode()`/`decode()`) and a class per service of static methods: `nav.Flight.TOPIC` is `"nav.Flight"` (`package.Service`, what the server listens on), `nav.Flight.Upload(peer, now, req)` calls and returns the id, `nav.Flight.request(incoming)` gives `(method, req)` for a call, `nav.Flight.respond(peer, now, incoming, resp)` replies, and a reply's payload decodes with its message's `decode`, `nav.Ack.decode(incoming.bytes)`. A method with `(mud.ttl_secs)` has its ttl baked in, otherwise pass `ttl_secs`; one with `(mud.topic)` publishes, arrives as `event` at whoever listens on that topic (decode it with the message's `decode`), and has no reply. The options are `proto/mud_options.proto`, which only protoc reads: the generator skips imports. `target/release/mud gen --lang python nav.proto > nav.py` writes the same module to disk if you prefer a file.

**7. Streams.** Answer one call with `reply(now, call, status, payload, end, seq, detail)`, `detail` the status's text and `""` by default, for `seq` 0, 1, 2… below 64 (above it a `MudError`, `code == -3`), `end=True` on the last item only; it is the default, so a unary reply is seq 0 with `end`. The items arrive in any order, each a `reply` with its `seq` and `end`, and the call completes once every `seq` up to the lowest `end` it has seen has arrived, or is `expired` at its ttl with one missing. Send `end` once: an item above the lowest `end` is `dropped == "seq"`, a redelivered item `"duplicate"`, and anything after completion `"no_pending"`.

**8. A signed fleet** (SPEC.md layer 1).

```sh
target/release/mud provision fleet 3
```

That writes `fleet/fleet.key` (`key <hex>`, once), `fleet/device-1` to `fleet/device-3` (`sender <n>`, `epoch 1`, `seed <hex>`, owner-only, each device's one secret) and `fleet/roster` (`<sender> <public key hex>` a line), 32-byte values in lowercase hex; run it again to add devices, numbered after the highest, and `--key` in place of the count for a rotation's next key, `fleet/fleet.key.next`; no file is ever overwritten. A device gets the fleet key, its own device file and the roster, and boots `Peer(name, key, sender, epoch, seed=seed, budget=1000)` from them (`bytes.fromhex` each value), with a `roster(sender, key, next=None)` line for every sender it believes; its epoch starts at the file's and is raised under the host contract. `budget` is the signature checks per second this host affords (at least 1; a wire past them is `dropped` `"open"`, `status` 6, and one from a sender with no line `status` 5); `Peer.public_key(seed)` gives a line's key, and `revoke(sender)` removes one. A peer believes 16 senders and the 17th line raises `Full`, so a ground station that must believe a fleet is built with `hub=True`, the same peer with 256 lines, 256 replay windows and 1024 remembered messages in a larger block. Its wire is version 4 (2 under ChaCha20-Poly1305): an unsigned peer gives it `kind == "dropped"`, `dropped == "version"`, `status == 4`, and a signed peer an unsigned wire the same with `status == 3`, so one fleet key never serves both. A discovered key or seed: SECURITY.md, "If a key is discovered".

`cipher` is `mud.CIPHER_DEFAULT`, AES-256-GCM; `cipher=mud.CIPHER_CHACHA20_POLY1305` builds any of these peers under ChaCha20-Poly1305 instead: the same sizes and calls, wire version 1, or 2 signed, so a peer of the other cipher gives its wires `dropped == "version"` with that version in `status`.

**9. Verify.** `cargo test -p mudrpc-c --test abi -- --include-ignored the_python_skill_example_runs` runs step 2's program and every row of the table below.

## Drop-in for gRPC

What every language's drop-in shares, the one-line change, gRPC without HTTP/2, the dependencies and what is not carried, is [From gRPC](../../README.md#from-grpc) in the README.

The product keeps its `.proto`, `python -m grpc_tools.protoc` and the stub and servicer it generates. `mud_grpc.py`, beside `mud.py`, is a `grpc.Channel` and the server for the servicers added to it, with grpcio, whose types they are; no `grpc.server` and no port:

```python
import mud, mud_grpc
import drone_pb2 as pb, drone_pb2_grpc as rpc

# was: channel = grpc.insecure_channel("drone:5001")
channel = mud_grpc.Channel(mud.Peer("gcs", fleet_key, 1, epoch), mud.Udp("10.0.0.2:7000", "10.0.0.3:7000"))
drone = rpc.DroneStub(channel)
reply = drone.Arm(pb.ArmRequest(id=7), timeout=30)
```

A server adds the generated servicer where `grpc.server` took it, `rpc.add_DroneServicer_to_server(DroneService(), channel)`, in any process, the channel's pool of 16 threads running the servicer as grpc.server's executor would. Over `mud.Udp` a channel listens on its own name when constructed and on a service when it is added, before the call returns. While the channel lives the peer is reached through `channel.use(lambda p: p.save())`, the same for a key's `stage`, `rotate` and `retire`, and the channel is closed before the peer, from any thread, the transport's send included. Another transport is a callable that takes each outgoing item, and it hands every wire it receives to `channel.deliver(topic, wire)`; an exception it raises goes to the channel's `fault` callback, and the item is tried for a second, then let go.

- **A timeout is the call's ttl**: none is the channel's `ttl_secs`, 60 by default. `DEADLINE_EXCEEDED` comes at the deadline, never before it and a pump round at most after, but for one past a week, the longest a message lives, which expires at a week.
- **A call is grpcio's**: what a stub raises is a `grpc.RpcError` that is also the `grpc.Call` and `grpc.Future`, `with_call` and `future` as grpcio's, a stream an iterator that is a `grpc.Call`, `code()` and `details()` waiting for the status, interceptors through `grpc.intercept_channel` as with any channel; a timeout of infinity is none and of NaN already passed. Done callbacks run on a pool of the channel's own, never inside its lock and never behind a servicer; replies are decoded on the caller's thread. A stream dropped unread is let go with it.
- **A status is gRPC's**: `context.abort` and `context.set_code` arrive with their code and details, text or bytes, the first 128 bytes kept; an exception a servicer raises arrives as `UNKNOWN`, "Exception calling application: ...", or "Exception iterating responses: ..." from a stream, as grpcio sends them. A server's items reach the caller before its status; a cancel, a timeout or a closed channel cuts them off, unread items included. A reply too large for the wire reaches the caller as `RESOURCE_EXHAUSTED`, and ends a stream there, its servicer asked for no more items. A servicer sees `context.is_active()` turn false when its call's ttl passes, or its channel's `stop` grace.
- **A call on a service the channel does not serve** gets no answer, so a fleet where every node hears every wire still has one server answer it; a method missing from a service it serves is `UNIMPLEMENTED`. A call's ttl counts from its arrival, so a server behind sheds a call its caller has given up on, unrun, and past 1024 waiting it answers `RESOURCE_EXHAUSTED`. The channel owes one caller 1024 replies at most, sixteen full streams, and all its callers 4096, taking them in turn, the longest line giving way past that; a flooding sender's calls go unanswered. Under one fleet key a holder can write a new sender number on every call, and the 32 a second are shared among all of them: sign the fleet where that matters. A service's full name is 1 to 64 bytes of UTF-8, another a `ValueError`. `channel.stop(grace)` is grpc.Server's: it takes no new calls, and past `grace` seconds, at once for `None`, cancels the running and the waiting, their callers `CANCELLED`; the event it returns is set once every servicer has returned.
- **`context.peer()`** is `mudrpc:<n>`, the caller's roster number, proven per device under a signed fleet.
- **Refused before sending**: a message past the wire, about 460 bytes, and a 17th call in flight, as `RESOURCE_EXHAUSTED`; a service name that is not 1 to 64 bytes of UTF-8, or a method name that is not UTF-8, as `INVALID_ARGUMENT`, a method name of any other length carried; a timeout of 0 or less, as `DEADLINE_EXCEEDED`. A call cancelled after it was sent keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
- **Paced**: a server sends 32 replies a second, half what a receiver takes from one sender, so no edge of a second drops one, each caller in turn; a 63-item stream takes two seconds, and a client keeps under 32 calls a second for the same reason.
- **Not carried**: metadata, so `invocation_metadata()` is empty and trailers are dropped; client and bidirectional streams, `UNIMPLEMENTED`; compression.
- **With a native peer**: a drop-in server ends a stream with a reply that carries no item, end, OK and the detail `end` (`mud_grpc.STREAM_END`); a native server's stream ends on its last item, and the drop-in client reads both.

`cargo test -p mudrpc-c --test abi -- --include-ignored the_python_drop_in` installs grpcio and grpcio-tools into `target/pygrpc` from PyPI, each wheel pinned by its hash in `bindings/c/tests/grpc-python/requirements.txt`, generates the stub, and runs a client and a servicer over two peers, over UDP and against a native server through every case above.

## Verify

- `mud.Peer(...)` raises `FileNotFoundError` naming the path it tried and the build command if the library is missing, never a segfault.
- `MUDRPC_LIB` loads only what it names, with no fallback; set it only to a library you built.
- Every `outgoing()` item reaches the transport before `sent()`, which takes the item the last `outgoing()` showed and nothing else; with nothing shown it is a no-op.
- `deliver()` never raises for bad bytes: it returns an `Incoming` with `kind == "dropped"` and a reason.
- One thread drives a peer: a second thread's call finds the block held and raises `MudError` with `code == -10`, nothing done; a detector of the mistake, not a lock.

The host mistakes the contract names, each with what it raises or returns, every row asserted by `the_python_skill_example_runs`:

| Mistake | Answer | Kind |
| --- | --- | --- |
| `restore` or `renew` with an epoch at or below the saved one | `MudError`, `code == -6` | error |
| sender 0 sending | `MudError`, `code == -7`, at the first send | error |
| a key or seed of 32 equal bytes | `MudError`, `code == -1`; another length is a `ValueError` first | error |
| `save()` bytes of another length to `restore` | `Corrupt`, `code == -4` | error |
| a hub's `save()` cut to a leaf's length | `MudError`, `code == -5` | error |
| `hub=True` without a seed | `MudError`, `code == -1` | error |
| `save()` bytes retagged to the other cipher | `Corrupt`, `code == -4` | error |
| saved bytes sent from without a resume | cannot happen: `restore` resumes | none |
| ttl 0, or past a week | sent as 1, or a week | clamp |
| a topic or name over 64 bytes, or a payload over the ceiling | `TooLarge`, `code == -3` | error |
| an empty name or topic | `MudError`, `code == -1` | error |
| a reply to a call this peer does not have | `deliver` returns `dropped == "no_pending"` | silent |
| 16 calls pending, or 16 items queued undrained | `Full`, `code == -2`: decide what not to queue; `pending` and `queued` count them | error |
| the 33rd send in one second | `MudError`, `code == -9`: the next second sends | error |

## Test at each rung

- **In-process:** step 2's program, or step 4's simulator. `tests/harness.py` next to the C binding runs every scenario in `scenarios/`, unsigned, `--signed`, `--gcm` and both, and its logs must equal `scenarios/<name>.log` (`<name>.gcm.log` where one is checked in) and the Rust harness's signed runs byte for byte.
- **Loopback (lan):** `cargo test -p mudrpc-adapters -- --include-ignored partition_heal_over_loopback` is the reference behaviour over real sockets.
- **Relay:** the same on a box on your own LAN, never on the internet.

## Known edges

- The host contract every binding shares (the epoch, save, restore, the nonce, the window, the ttl, threads, bounds, codes, close, the cipher) is the list at the top of `bindings/c/mudrpc.h`.
- `counts` gives `managed`, `rollbacks`, `floods` and `dropped`, saturating and kept across restores.
- `close()` (or `del`) ends a peer, and every later call raises `MudError` -1. The bytes `save()` gives, and the key you passed in, live in memory Python will not let anyone zero; `close()` zeroes the library's copy only.
- Each call into the library copies its arguments once; this is a scripting binding, and the copies are the price of not holding pointers into Python objects.
- A payload over the header's ceiling, about 460 bytes, is `TooLarge`.
- Payloads are `bytes`; topics and methods are `str`, and a lone surrogate fails in Python's UTF-8 encoder before the library sees it. `now`, ttl, sender and epoch are unsigned 32-bit integers: a float is a `ctypes.ArgumentError`; a negative wraps.
