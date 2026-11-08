---
name: mudrpc-java
description: How to integrate, verify and test the Java binding for mudRPC.
---
mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The Java binding is one file, `Mud.java`, over the C ABI through the foreign function API in `java.lang.foreign`, with no package and no JNI: copy it into your project as `mudrpc/Mud.java` (it declares `package mudrpc`) and your IDE sees every type and comment. Java 22 or later.

## Integrate

From a clone to a call answered over the simulator, then your transport, your `.proto` and a signed fleet.

**1. Build the library.** A Rust toolchain is the one build dependency, until prebuilt binaries ship at 3.0:

```sh
cargo build --release -p mudrpc-c -p mudrpc-gen
```

That gives the library and `target/release/mud`. Copy `bindings/java/Mud.java` into your project as `mudrpc/Mud.java`. Point `MUDRPC_LIB` at the library, or put it next to your classes (the `javac -d` directory) or jar. From 3.0, the Maven Central artefact `io.github.jacob-mellor:mudrpc` gives the same: the classes with the library inside the jar, copied out for the run when none is beside it, and the sources and javadoc jars.

**2. Make one call.** The smallest complete program: two peers in one process, a call and a reply moved by hand between them.

```java
import mudrpc.Mud.*;

import java.nio.charset.StandardCharsets;
import java.util.Arrays;

public class Example {
    public static void main(String[] args) {
        byte[] key = new byte[32]; new java.security.SecureRandom().nextBytes(key);   // the fleet key: 32 bytes from the OS, provisioned to every peer; never a constant and never a passphrase (32 equal bytes are refused, other constants are not)
        int epoch = 1;                                               // persisted beside the key; raise it on every boot and restore, write it first
        try (Peer gcs = new Peer("gcs", key, 1, epoch); Peer drone = new Peer("drone", key, 2, epoch)) {   // roster numbers; 0 cannot send; the shorter constructors pass Peer.CIPHER_DEFAULT
            drone.listen("nav.Flight");                                     // the drone's transport will subscribe to nav.Flight
            byte[] callId = gcs.call(0, "nav.Flight", "Arm", new byte[] { 8, 1 }, 30);   // now 0, ttl 30 s; a protobuf request, encoded by your protobuf library
            Peer[] peers = { gcs, drone };
            for (int now = 0; now < 3; now++)                        // your loop: pump each peer, every tick
                for (int i = 0; i < 2; i++) {
                    Peer p = peers[i];
                    Outgoing o;
                    while ((o = p.outgoing(now)) != null) {
                        if (!o.listen()) {                           // hand o.topic(), o.wire(), o.ttlSecs() to your transport; listen: subscribe to o.topic()
                            Incoming incoming = peers[1 - i].deliver(now, o.topic(), o.wire());
                            switch (incoming.kind()) {
                                case CALL -> {
                                    System.out.println("drone: " + incoming.method());
                                    peers[1 - i].reply(now, incoming, 0, "armed".getBytes(StandardCharsets.UTF_8));
                                }
                                case REPLY -> {
                                    System.out.println("gcs: status " + incoming.status() + " " + new String(incoming.bytes(), StandardCharsets.UTF_8));
                                    return;
                                }
                                case DROPPED -> System.out.println("dropped: " + incoming.dropped());   // log in development, count in the field
                                default -> {}
                            }
                        }
                        p.sent();                                    // only after the transport took it; if it refused, break instead
                    }
                    while (p.tick(now) != null) System.out.println("expired");
                }
        }
        System.exit(2);
    }
}
```

`javac -d out mudrpc/Mud.java Example.java`, copy `libmudrpc.dylib` into `out` (or set `MUDRPC_LIB`), then `java --enable-native-access=ALL-UNNAMED -cp out Example`: that flag is the JDK's permission for any native call and the only one the binding needs. Expected output: `drone: Arm` then `gcs: status 0 armed`, exit 0.

**3. The loop.** Every tick, drain `outgoing()` into your transport, calling `sent()` once it took each item (if it refused, `break`: the item is there next tick); feed what it received to `deliver()`; call `tick()`. `outgoing()` also yields listen items, one for the peer's own name (its reply inbox) and one per `listen()`, and what a peer hears is what its transport subscribes to. `now` is your uptime in seconds; the 12-byte `byte[]` id returned by `call` is the `id()` on the reply that answers it, compared with `Arrays.equals`, `seq()` and `end()` carry streams and `status()` is gRPC's status number. `Incoming.kind()` is `CALL`, `REPLY`, `EVENT` (a publish), `EXPIRED` (from `tick()`, with the call's `id()`), `DROPPED`, `FLOOD` or `ROLLBACK`, and all but `EXPIRED` and `DROPPED` name their `sender()`. `save()`, `Peer.restore(saved, epoch)`, `renew(epoch)` and `close()` keep and end a peer under the host contract at the top of `bindings/c/mudrpc.h`, which every binding shares.

**4. Over the simulator.** Replace the hand-moving with the simulator, `new Sim(text)`, one radio per peer in place of your transport. The text is a scenario in the format `adapters/SKILL.md` gives, `"seed 1\npeers 2\nprofile reticulum\n0 partition 0 1\n30000 heal 0 1\n"` cutting the two radios apart until 30 s, so a call at 0 with ttl 60 is answered after the heal and one with ttl 10 expires; `events()` lists its host events as `SimEvent(atMs, kind, a, b, text)` for your harness to perform; a bad line throws `IllegalArgumentException` naming it. Radio `i` is your `i`th peer, 0 to `peers()` − 1, whatever its sender number. Each round, for each peer `i`: hand its outgoing items to `send(i, o.topic(), o.wire(), o.ttlSecs())` or `listen(i, o.topic())` (false while the radio is down, a `peer_down` row, not a partition, whose wire the mesh holds: `break`, no `sent()`), `deliver()` each `poll(i)` result with `kind() == 1` until null (2 is on-mesh, 3 off-mesh), `tick()`; then `step(100)`. `now` is `(int) (nowMs() / 1000)`; `note(text)` writes a host line and `log()` returns the world's log; `peers()` and `lastEventMs()` come from the text.

**5. Your transport.** In the simulator's place your transport takes each send item and listen topic, and what it receives goes to deliver; its rungs are below. On an IP network it can be the library's own, `new Mud.Udp("10.0.0.2:7000", "10.0.0.3:7000")`, with `send(item)`, a listen among them, and `poll()`, which yields only a listed peer's wires on a topic listened to, into `deliver`, or given to a `MudChannel`, which pumps it every 10 ms. It binds and sends to private, loopback or link-local addresses alone; `0.0.0.0` is public, since it binds every interface. A library built with the cargo feature `internet`, and `MUDRPC_INTERNET=listen-on-public-addresses` in the builder's environment, may bind and send to public ones; the feature alone fails the build, since any crate in it could set the feature, and no release is built with it. Cargo shows the build's own warning for the workspace alone, so a socket on a public address says so on standard error and `Udp.internet()` is the check a product makes at start. The variable belongs on the command line of the one build that means it, never in a `.cargo/config.toml`, where every build would go public in silence. Reticulum's and DTN's adapters are their own people's, against `adapters/CONFORMANCE.md`.

**6. Your `.proto`.** If the product has gRPC code, in Java or Kotlin, keep it and read "Drop-in for gRPC" below. Otherwise encode request and reply bytes with your protobuf library of choice. The wire is protobuf; only the payload is yours. A proto service `Flight` in `package nav` is the service string `"nav.Flight"`, which the server listens on, with the rpc name as the method; `(mud.ttl_secs)` is the `ttlSecs` argument; a method with `(mud.topic)` is `publish` on that topic, arrives as `EVENT` and has no reply.

**7. Streams.** Answer one call with `reply(now, call, status, payload, end, seq)`, or `reply(now, call, status, payload, end, seq, detail)` to give the status its text, for `seq` 0, 1, 2… below 64 (above it a `MudException`, `code == -3`), `end` true on the last item only; the four-argument `reply` passes true and 0, so a unary reply is seq 0 with `end`. The items arrive in any order, each a `REPLY` with its `seq()` and `end()`, and the call completes once every `seq()` up to the lowest `end()` it has seen has arrived, or is `EXPIRED` at its ttl with one missing. Send `end` once: an item above the lowest `end()` is `Dropped.SEQ`, a redelivered item `Dropped.DUPLICATE`, and anything after completion `Dropped.NO_PENDING`.

**8. A signed fleet** (SPEC.md layer 1).

```sh
target/release/mud provision fleet 3
```

That writes `fleet/fleet.key` (`key <hex>`, once), `fleet/device-1` to `fleet/device-3` (`sender <n>`, `epoch 1`, `seed <hex>`, owner-only, each device's one secret) and `fleet/roster` (`<sender> <public key hex>` a line), 32-byte values in lowercase hex; run it again to add devices, numbered after the highest, and `--key` in place of the count for a rotation's next key, `fleet/fleet.key.next`; no file is ever overwritten. A device gets the fleet key, its own device file and the roster, and boots `new Peer(name, key, sender, epoch, seed, budget)` from them (`HexFormat.of().parseHex` each value), then a `roster(sender, key, null)` line for every sender it believes; its epoch starts at the file's and is raised under the host contract. `new Peer(name, key, sender, epoch, seed, budget)` with this sender's 32-byte Ed25519 seed and the signature checks per second this host affords (at least 1; a wire past them is `Dropped.OPEN`, `status()` 6), a `roster(sender, key, next)` line for every sender using `Peer.publicKey(seed)`, `revoke(sender)` to remove one. A peer believes 16 senders and the 17th line throws `MudException` with `isFull()`, so a ground station that must believe a fleet is built with `hub` true (the seventh argument), the same peer with 256 lines, 256 replay windows and 1024 remembered messages in a larger block. Its wire is version 4 (2 under ChaCha20-Poly1305): an unsigned peer gives it `Kind.DROPPED` with `Dropped.VERSION` and `status() == 4`, and a signed peer an unsigned wire the same with `status() == 3`, so one fleet key never serves both. A discovered key or seed: SECURITY.md, "If a key is discovered".

`cipher`, the eighth argument, is `Peer.CIPHER_DEFAULT` from the shorter constructors, AES-256-GCM; `Peer.CIPHER_CHACHA20_POLY1305` there builds any of these peers under ChaCha20-Poly1305 instead: the same sizes and calls, wire version 1, or 2 signed, so a peer of the other cipher gives its wires `Dropped.VERSION` with that version in `status()`.

**9. Verify.** `cargo test -p mudrpc-c --test abi -- --include-ignored the_java_skill_example_runs` runs step 2's program and every row of the table below.

## Drop-in for gRPC

What every language's drop-in shares, the one-line change, gRPC without HTTP/2, the dependencies and what is not carried, is [From gRPC](../../README.md#from-grpc) in the README.

The product keeps its `.proto`, the protobuf Gradle or Maven plugin in IntelliJ, Eclipse or VS Code, and the stubs and service bases grpc-java and grpc-kotlin generate. `MudChannel.java`, beside `Mud.java`, is an `io.grpc.Channel` and the server for the services added to it, with the grpc-java packages whose types they are, and no transport package:

```kotlin
dependencies {
    implementation("io.grpc:grpc-protobuf:1.85.1")
    implementation("io.grpc:grpc-stub:1.85.1")            // and for Kotlin, "io.grpc:grpc-kotlin-stub:1.5.0"
    implementation("com.google.protobuf:protobuf-java:4.36.2")
}   // no grpc-netty, grpc-okhttp or grpc-servlet: MudChannel is the transport
```

Building the channel is the line that changes; the stub is the generated one, as before:

```java
// was: ManagedChannel channel = ManagedChannelBuilder.forAddress("drone", 5001).usePlaintext().build();
MudChannel channel = new MudChannel(new Mud.Peer("gcs", fleetKey, 1, epoch), new Mud.Udp("10.0.0.2:7000", "10.0.0.3:7000"));
DroneGrpc.DroneBlockingStub drone = DroneGrpc.newBlockingStub(channel).withDeadlineAfter(30, TimeUnit.SECONDS);
ArmReply reply = drone.arm(ArmRequest.newBuilder().setId(7).build());
```

Kotlin's is the same channel, `DroneGrpcKt.DroneCoroutineStub(channel)`. A server adds the generated base where `ServerBuilder` did, `channel.addService(new DroneService())`, interceptors included through `ServerInterceptors.intercept`, in any process, with no server or port of gRPC's. While the channel lives the peer is reached through `channel.use(p -> p.save())`, the same for a key's `stage`, `rotate` and `retire`, and the channel is closed before the peer, from any thread, the transport's send included. Another transport takes each item through the `Consumer` the channel was built with and hands every wire it receives to `channel.deliver(topic, wire)`; an exception it throws goes to the channel's `fault` callback, and the item is tried for a second, then let go.

- **A deadline is the call's ttl**, the earlier of the stub's and the `Context`'s: none is the channel's `ttlSecs`, 60 by default. `DEADLINE_EXCEEDED` comes at the deadline, never before it, but for one past a week, the longest a message lives, which expires at a week.
- **Callbacks run as grpc-java's do**, on the call's executor, a blocking stub's own thread included, one at a time and in order, and on the channel's pool when that executor refuses; no callback runs inside the channel's lock. A callback on a direct executor runs on the thread that found it, the pump's among them, and holds it while it runs, as it would hold grpc-java's transport thread.
- **A status is gRPC's**: `onError` with a `StatusRuntimeException` arrives with its code and description; an exception thrown out of a service method arrives as `UNKNOWN`, "Application error processing RPC", as grpc-java sends it, the 64th item of a stream among them. A server's items reach the caller before its status; a cancel, a deadline or a closed channel cuts them off, unread items included. A listener that throws ends its call `CANCELLED`, "Failed to read message.", with the exception as its cause. A reply too large for the wire reaches the caller as `RESOURCE_EXHAUSTED`, and ends a stream there, its handler's `isCancelled()` true and its writes going nowhere. A server whose call's ttl passes before it closes the call hears `onCancel`.
- **A call on a service the channel does not serve** gets no answer, so a fleet where every node hears every wire still has one server answer it; a method missing from a service it serves is `UNIMPLEMENTED`. Handlers run on up to 64 threads; a call waits for one, 1024 at most, past which it is `RESOURCE_EXHAUSTED` "the server is behind", and one whose caller has given up, its ttl counted from its arrival, is shed unrun. The channel owes one caller 1024 replies at most, sixteen full streams, and all its callers 4096, taking them in turn, the longest line giving way past that; a flooding sender's calls go unanswered. Under one fleet key a holder can write a new sender number on every call, and the 32 a second are shared among all of them: sign the fleet where that matters.
- **The caller is `Grpc.TRANSPORT_ATTR_REMOTE_ADDR`**, a `MudChannel.Sender` printing `mudrpc:<n>`, the caller's roster number, proven per device under a signed fleet.
- **Refused before sending**: a message past the wire, about 460 bytes, and a 17th call in flight, as `RESOURCE_EXHAUSTED`; a service name that is empty, over 64 bytes or not UTF-8, and a method name that is not UTF-8, a lone surrogate among them, as `INVALID_ARGUMENT`; a cancelled `Context` or a passed deadline, as `CANCELLED` or `DEADLINE_EXCEEDED`. A call cancelled after it was sent keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
- **Paced**: a server sends 32 replies a second, half what a receiver takes from one sender, so no edge of a second drops one, each caller in turn, so one caller's stream does not hold another's reply behind it; a 63-item stream takes two seconds, and a client keeps under 32 calls a second for the same reason.
- **Not carried**: metadata, so headers and trailers are empty both ways; client and bidirectional streams, `UNIMPLEMENTED`; compression.
- **With a native peer**: a drop-in server ends a stream with a reply that carries no item, end, OK and the detail `end` (`MudChannel.STREAM_END`); a native server's stream ends on its last item, and the drop-in client reads both.

`cargo test -p mudrpc-c --test abi -- --include-ignored the_java_and_kotlin_drop_in` fetches protoc, grpc-java's and grpc-kotlin's plugins and their packages from Maven Central, each pinned by its SHA-256, generates the stubs, and runs a Java client and service over two peers, over UDP and against a native server through every case above, then Kotlin's coroutine stub and base.

## Verify

- `new Peer(...)` throws `UnsatisfiedLinkError` naming the path it tried and the build command if the library is missing, never a crash.
- `MUDRPC_LIB` loads only what it names, with no fallback; set it only to a library you built.
- Every `outgoing()` item reaches the transport before `sent()`, which takes the item the last `outgoing()` showed and nothing else; with nothing shown it is a no-op.
- `deliver()` never throws for bad bytes: it returns an `Incoming` with `kind() == Kind.DROPPED` and a reason.
- One thread drives a peer: a second thread's call finds the block held and throws `MudException` with `code == -10`, nothing done; a detector of the mistake, not a lock.

The host mistakes the contract names, each with what it throws or returns, every row asserted by `the_java_skill_example_runs`:

| Mistake | Answer | Kind |
| --- | --- | --- |
| `restore` or `renew` with an epoch at or below the saved one | `MudException`, `code == -6` | error |
| sender 0 sending | `MudException`, `code == -7`, at the first send | error |
| a key or seed of 32 equal bytes | `MudException`, `code == -1`; another length is an `IllegalArgumentException` first | error |
| `save()` bytes of another length to `restore` | `MudException`, `code == -4` | error |
| a hub's `save()` cut to a leaf's length | `MudException`, `code == -5` | error |
| `hub` true without a seed | `MudException`, `code == -1` | error |
| `save()` bytes retagged to the other cipher | `MudException`, `code == -4` | error |
| saved bytes sent from without a resume | cannot happen: `restore` resumes | none |
| ttl 0, or past a week (a negative `int` is past it) | sent as 1, or a week | clamp |
| a topic or name over 64 bytes, or a payload over the ceiling | `MudException`, `code == -3` | error |
| an empty name or topic | `MudException`, `code == -1` | error |
| a reply to a call this peer does not have | `deliver()` returns `dropped() == Dropped.NO_PENDING` | silent |
| 16 calls pending, or 16 items queued undrained | `MudException`, `isFull()`, `code == -2`: decide what not to queue; `pending()` and `queued()` count them | error |
| the 33rd send in one second | `MudException`, `code == -9`: the next second sends | error |

## Test at each rung

- **In-process:** step 2's program, or step 4's simulator. `tests/java` next to the C binding runs every scenario in `scenarios/`, unsigned, `--signed`, `--gcm` and both, and its logs must equal `scenarios/<name>.log` (`<name>.gcm.log` where one is checked in) and the Rust harness's signed runs byte for byte.
- **Loopback (lan):** `cargo test -p mudrpc-adapters -- --include-ignored partition_heal_over_loopback` is the reference behaviour over real sockets.
- **Relay:** the same on a box on your own LAN, never on the internet.

## Known edges

- The host contract every binding shares (the epoch, save, restore, the nonce, the window, the ttl, threads, bounds, codes, close, the cipher) is the list at the top of `bindings/c/mudrpc.h`.
- `counts()` gives `managed`, `rollbacks`, `floods` and `dropped`, saturating and kept across restores.
- `Peer` and `Sim` own native memory: `close()` them, or try-with-resources; every later call on a peer throws `MudException` with `code == -1`. The bytes `save()` gives, the key you passed in and any copy the garbage collector made of them live in managed memory nobody can zero; `close()` zeroes the library's copy only.
- Each call into the library copies its arguments into a confined arena that closes when the call returns; the copies are the price of not holding pointers into the heap.
- Without `--enable-native-access=ALL-UNNAMED` the JDK prints a warning when the library loads, and a future JDK will refuse it.
- A payload over the header's ceiling, about 460 bytes, is `MudException` with `code == -3`.
- Topics and methods are UTF-8; a lone surrogate becomes `?`. `now`, ttl, sender and epoch cross as unsigned 32-bit, so a negative `int` is the top of the range, not an error. A null byte array is a `NullPointerException`; pass an empty one.
