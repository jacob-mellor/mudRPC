# mudRPC

gRPC repurposed for the real world: unstable and outdoor networks, the Internet of Things, air, land, underwater and space. mudRPC carries gRPC's service model, services from a `.proto` with calls, replies, streams, events, deadlines and status codes, as sealed, self-contained packets over mesh networks, radios and delay-tolerant links: drone-to-drone and drone-to-ground communication, drone controllers and ground stations, field teams, IoT sensors on a LoRa mesh, underwater and deep-space links, any connection that comes and goes. A call returns at once with an id; its reply comes when the link allows, or the call expires at its ttl. Nothing blocks and nothing waits on a connection, because there is none.

It is a `no_std` Rust core with no dependencies and no allocation, under one C ABI, its header for C and a one-file wrapper each for C++, Python, .NET, Java, JavaScript, Swift, Go and PHP; a simulated mesh to test against before any radio; and `mud gen`, a `.proto` in and stubs out for Rust, Python and C.

```
  Product (any language)        ← one vendored wrapper file + one skill
        │ call / reply / publish / deliver / outgoing
        ▼
  mudRPC core (Rust, no_std)    ← envelope · codec · seal · peer state machine   (+ Sealer: Key · Gcm · Signed)
        ▲ the host pumps bytes between core and the transport
        │
  Transport (adapter)           ← sim · lan · Reticulum (theirs) · DTN (theirs) · yours
```

Core never touches a transport. The host moves bytes between them. That is what lets an adapter be written in Python for Reticulum, in Go for a relay, or in C on a flight controller, with no callbacks into Rust.

## Where it fits

Wherever the link is intermittent: a drone fleet, a sensor mesh, a field team, an acoustic modem under water, a deep-space pass, the links delay-tolerant networking was built for. Beside what such a team already runs, each doing its own job:

- **MAVLink** is the vehicle's own protocol, between autopilot, ground station and payload, and stays so. mudRPC carries the product's service calls around it (a mission handed between drones, a payload's command, a report to the ground) as calls that wait out a broken link; the camera's protocol stays the camera's, and mudRPC carries its control and its stills ([SPEC.md, "The messaging protocol"](SPEC.md#the-messaging-protocol)).
- **Meshtastic and MeshCore** channels and their pre-shared keys are the model for mudRPC's fleet key. mudRPC is the RPC layer a product puts on such a mesh: typed calls and replies from a `.proto`, with expiry, replay protection and, signed, a sender proven per device.
- **Reticulum and DTN** (Bundle Protocol v7, RFC 9171) are the transports it rides: they move packets across any medium and hold them through a partition. Their adapters are written by their own people against the contract in [adapters/CONFORMANCE.md](adapters/CONFORMANCE.md), and the simulator carries a mock of each to test against first.
- **ROS 2 and Zenoh** are tooling for robots on a good network, with discovery and sessions. mudRPC is for the link that goes away: no discovery, no session, a call that waits out its ttl.
- **MQTT** has a broker and a session to it. mudRPC has neither, by design: a topic is the address, any listener may answer and the first reply wins, and nothing central has to be reachable.

## Try it in five minutes

From a clone, with rustup (`rust-toolchain.toml` brings the toolchain):

```
cargo test
cargo run -p mudrpc-adapters --example scenario -- scenarios/partition-heal.scenario
```

From 3.0, each language's package manager carries the wrapper and the built library: `cargo add mudrpc-core`, `npm install mudrpc`, `dotnet add package mudrpc`, `pip install mudrpc`, Maven's `io.github.jacob-mellor:mudrpc`, `go get github.com/jacob-mellor/mudRPC/bindings/go/v3`, and vcpkg's and Conan's `mudrpc`; `./ci.sh release` builds every one from the signed tag, sums it and installs it into a fresh project first ([packages/](packages/)).

The first runs the unit layer in seconds. The second plays [scenarios/partition-heal.scenario](scenarios/partition-heal.scenario) on the simulator: two peers cut apart from the start and healed at 30 s, `gcs` calling `Ping` on the drone every 15 s. The 10 s call is `expired` at 10 000 ms; the 60 s call and the two made during the cut are answered at 31 300 ms, after the heal. The log is every world action and every peer verdict, the same as [the checked-in one](scenarios/partition-heal.log) byte for byte.

## Tutorials

Each skill's Integrate section is the tutorial for its language, numbered from a clone to a call answered over the simulator, then your transport, your `.proto` and a signed fleet from `mud provision`. Each is also the one document an LLM integrates from: give it the `SKILL.md` and your `.proto`.

- Rust, the peer, the simulator and loopback: [adapters/SKILL.md](adapters/SKILL.md#integrate)
- C, the ABI every other language sits on: [bindings/c/SKILL.md](bindings/c/SKILL.md#integrate)
- C++, and gRPC C++ over mudRPC: [bindings/cpp/SKILL.md](bindings/cpp/SKILL.md#integrate)
- Go, and grpc-go over mudRPC: [bindings/go/SKILL.md](bindings/go/SKILL.md#integrate)
- PHP, and gRPC PHP's client over mudRPC: [bindings/php/SKILL.md](bindings/php/SKILL.md#integrate)
- Python: [bindings/python/SKILL.md](bindings/python/SKILL.md#integrate)
- .NET: [bindings/dotnet/SKILL.md](bindings/dotnet/SKILL.md#integrate)
- Java: [bindings/java/SKILL.md](bindings/java/SKILL.md#integrate); on Android, over JNI: [bindings/android/SKILL.md](bindings/android/SKILL.md#integrate)
- JavaScript, Node or a browser: [bindings/js/SKILL.md](bindings/js/SKILL.md#integrate)
- Swift, macOS and Linux: [bindings/swift/SKILL.md](bindings/swift/SKILL.md#integrate)

The host contract every language shares is the top comment of [bindings/c/mudrpc.h](bindings/c/mudrpc.h). If a key is discovered, the operator's page is [SECURITY.md](SECURITY.md#if-a-key-is-discovered).

## From gRPC

A gRPC product keeps its `.proto`, the code `protoc` and its language's plugin generate, and the IDE that runs them. The line that built the channel is the change; the generated client and service run unchanged, and the same `.proto` runs on both sides of the table.

| Language | Was | Is | The rest |
| --- | --- | --- | --- |
| C# / .NET | `GrpcChannel.ForAddress("https://drone:5001")` | `new MudChannel(peer, new Udp("10.0.0.2:7000", "10.0.0.3:7000"))` | [the drop-in](bindings/dotnet/SKILL.md#drop-in-for-grpc) |
| Java / Kotlin | `ManagedChannelBuilder.forAddress("drone", 5001).usePlaintext().build()` | `new MudChannel(peer, new Mud.Udp(...))` | [the drop-in](bindings/java/SKILL.md#drop-in-for-grpc), [Android](bindings/android/SKILL.md) |
| Python | `grpc.insecure_channel("drone:5001")` | `mud_grpc.Channel(peer, mud.Udp(...))` | [the drop-in](bindings/python/SKILL.md#drop-in-for-grpc) |
| Go | `grpc.NewClient("10.0.0.3:5001", ...)` | `mud.NewChannel(peer, radio)` | [the drop-in](bindings/go/SKILL.md#drop-in-for-grpc) |
| JavaScript / TypeScript | `createConnectTransport({ baseUrl })` | `new MudTransport(peer, await Udp.open(...))` | [the drop-in](bindings/js/SKILL.md#drop-in-for-grpc) |
| Swift | `.http2NIOPosix(target: ..., transportSecurity: .plaintext)` | `MudTransport(peer: peer, udp: udp).client` | [the drop-in](bindings/swift/SKILL.md#drop-in-for-grpc) |
| C++ | `grpc::CreateChannel("10.0.0.3:5001", ...)` | `mud::CreateChannel(peer, udp)`, `grpc::` becoming `mud::` | [the drop-in](bindings/cpp/SKILL.md#drop-in-for-grpc) |
| PHP | `['credentials' => ChannelCredentials::createInsecure()]` | `['credentials' => null, 'grpc_call_invoker' => $mud]` | [the drop-in](bindings/php/SKILL.md#drop-in-for-grpc) |

A server is the generated base bound where gRPC's server took it, in any process, with no server or port of gRPC's: a .NET service runs in a console app with no ASP.NET Core, Kestrel or IIS; a Java one with no grpc-netty; a Python one with no `grpc.server`. One channel is a client and a server at once.

**gRPC without HTTP/2.** The longer form is [docs/grpc-without-http2.md](docs/grpc-without-http2.md), with its pages on UDP, LoRa and mesh radio, drones, the Internet of Things, C# without IIS and the dependency-free library. mudRPC carries gRPC's service model with no HTTP at all: calls, replies, server streams, deadlines and status codes as sealed packets of at most 512 bytes over UDP, a radio, a mesh or any transport that moves bytes, and the JavaScript binding is a Connect transport over the wasm, where a browser would otherwise need gRPC-Web's proxy; a transport over HTTP/1.1 between listed peers is item 16 of the backlog.

**Dependencies, exactly.** Core has none: a `no_std` Rust library with no crates under one C ABI, and a binding is one file over it. A drop-in adds its language's gRPC API package and nothing of gRPC's transport: `Grpc.Core.Api` for .NET; `io.grpc:grpc-api` for Java, with `grpc-stub` and `grpc-protobuf` for the generated code and `grpc-kotlin-stub` for Kotlin; `grpcio` for Python; `google.golang.org/grpc` for Go; `@connectrpc/connect` and `@bufbuild/protobuf` for JavaScript; `grpc-swift` 2 for Swift; `grpc/grpc` for PHP; for C++, protobuf alone, since `protoc-gen-mud` writes the stub and nothing of gRPC's is linked.

**Not a drop-in, said once.** A message is about 460 bytes, a service's full name at most 64; unary and server-streaming calls only, a client or bidirectional stream `UNIMPLEMENTED`; at most 63 items a stream, the first reply wins, 16 calls in flight; a deadline is the call's ttl, a week at most, and a call cancelled after it was sent keeps its place among the 16 until its ttl; a server sends 32 replies a second, its callers in turn, and a client keeps under 32 calls a second; no metadata, so headers and trailers are empty both ways; no compression, and no interceptor that gRPC runs inside its own transport. Each skill's drop-in section says how its language shows these.

## Break it

The claims are the gates in SPEC.md's "The bar", each a test: no panic and no unsafe, constant time measured, every entry point fuzzed, the peer held to a model of its promises (`core/tests/property.rs`), and the multi-node campaign's fourteen holes fixed, its readers' scenarios held in all six harnesses. What it promises is SPEC.md's "Delivery semantics" and layer 1's "Threats, stated".

A scenario is a text file the simulator plays: `seed`, `peers`, `profile reticulum|dtn`, then `at_ms kind args` rows, every kind with its arguments and the log's lines in [adapters/SKILL.md](adapters/SKILL.md), and what each does in the checked-in logs in `scenarios/`. Peer 0, `gcs`, calls `Ping` on `nav`, which every drone answers, every 15 s; the log is every world action and every peer verdict. The six harnesses, `adapters/harness.rs` and five in `bindings/c/tests/`, must print the same log byte for byte, and each takes `--signed` and `--gcm` for the other three sealers. From a clone, with rustup (`rust-toolchain.toml` brings the toolchain and every target) and what each comment names:

```
printf 'seed 1\npeers 3\nprofile dtn\n0 partition 0 1\n20000 heal 0 1\n25000 replay 2\n' > mine.scenario
cargo run -p mudrpc-adapters --example scenario -- mine.scenario                     # Rust
cargo build -p mudrpc-c && cargo build -p mudrpc-c --target wasm32-unknown-unknown   # the library the other five load
cc -Ibindings/c bindings/c/tests/harness.c target/debug/libmudrpc.a -o target/harness && target/harness mine.scenario   # C; Linux adds -lpthread -ldl -lm
export MUDRPC_LIB=$PWD/target/debug/libmudrpc.dylib                                  # .so on Linux
PYTHONPATH=bindings/python python3 bindings/c/tests/harness.py mine.scenario          # Python 3
dotnet run -c Release --project bindings/c/tests/dotnet -- mine.scenario             # .NET 8
javac -d target/java bindings/java/Mud.java bindings/c/tests/java/Harness.java && java --enable-native-access=ALL-UNNAMED -cp target/java Harness mine.scenario   # JDK 22
MUDRPC_WASM=target/wasm32-unknown-unknown/debug/mudrpc.wasm node bindings/c/tests/js/harness.js mine.scenario   # Node 22
MUDRPC_SEED=7 MUDRPC_FUZZ=1000000 cargo test --release -p mudrpc-core --test fuzz --test property   # another seed, more rounds
cargo test --release -p mudrpc-auth --test ct -- --ignored --nocapture              # the timing probe, Welch's t
cargo test --release -p mudrpc-core -p mudrpc-auth -p mudrpc-gcm --test scrub       # the stack probes: what is left after a call
(cd qemu && cargo build --profile tripwire --target thumbv7em-none-eabihf --features icount && qemu-system-arm -M mps2-an386 -cpu cortex-m4 -semihosting-config enable=on,target=native -icount shift=0 -nographic -kernel target/thumbv7em-none-eabihf/tripwire/qemu)   # Cortex-M4: the checks, then instruction counts; qemu-system-arm
(cd qemu && cargo build --profile tripwire --target riscv32imc-unknown-none-elf && qemu-system-riscv32 -M virt -bios none -nographic -kernel target/riscv32imc-unknown-none-elf/tripwire/qemu)   # RISC-V; qemu-system-riscv32
```

A failing test, or a scenario and its log, is the report: a trap, a log showing what SPEC.md says never happens, or one that differs between two runs or two harnesses. Send it as [SECURITY.md](SECURITY.md) says.

Accepted by design, each with its reason in SPEC.md: anything a fleet-key holder does unsigned (layer 1, "Threats, stated"); traffic analysis of sizes and timing (Decisions, "Sizes and timing stay the product's"); the transport beneath, the payload above (layer 1, "Three layers of encryption"); a host clock stepping back (layer 2, "Time is an argument"); the campaign's stated residual costs (BACKLOG.md, Done, "The multi-node campaign, closed").

## Where things are

- [core/](core/): the four modules, `codec.rs`, `envelope.rs`, `seal.rs`, `rpc.rs`; its tests, fuzz and property test in `core/tests/`.
- [auth/](auth/) and [gcm/](gcm/): the signed sealer (Ed25519, SHA-512) and the AES-256-GCM sealer, behind core's one seam.
- [adapters/](adapters/): the transport contract, `sim` and `lan`, the Rust harness, the scenario example, [its skill](adapters/SKILL.md) and [CONFORMANCE.md](adapters/CONFORMANCE.md) for a transport's authors.
- [bindings/c/](bindings/c/): the C ABI and [mudrpc.h](bindings/c/mudrpc.h), with the five foreign harnesses in [bindings/c/tests/](bindings/c/tests/).
- [bindings/python/](bindings/python/), [bindings/dotnet/](bindings/dotnet/), [bindings/java/](bindings/java/), [bindings/js/](bindings/js/), [bindings/go/](bindings/go/), [bindings/php/](bindings/php/), [bindings/android/](bindings/android/): one wrapper file and one skill each.
- [gen/](gen/): `mud gen`, the stubs from a `.proto`, and `mud provision`, a fleet's keys, seeds and roster; [proto/](proto/) holds `mud_options.proto`, the two method options.
- [scenarios/](scenarios/): the standard scenarios and their checked-in logs.
- [tripwire/](tripwire/) and [qemu/](qemu/): the no-panic link and the two boards under QEMU, both in CI.
- [packages/](packages/): one manifest per package manager and `build.sh`, which the release runs.
- [demos/](demos/): two use cases, drones and sensors, one program per language, each printing the lines the test holds it to.
- [docs/](docs/): the longer pages, one phrase each, and the map of which page owns which; every folder has a `README.md` front door, and GitHub Pages renders the repository as it is, `_config.yml` at the root.

[SPEC.md](SPEC.md) says what we build and why, [SECURITY.md](SECURITY.md) how to report and what to do when a key is out, [BACKLOG.md](BACKLOG.md) what is being built now, [CLAUDE.md](CLAUDE.md) how we work.

MIT: [LICENSE](LICENSE).
