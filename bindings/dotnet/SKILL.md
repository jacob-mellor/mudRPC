---
name: mudrpc-dotnet
description: How to integrate, verify and test the .NET binding for mudRPC.
---
mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The .NET binding is one file, `Mud.cs`, over the C ABI through P/Invoke, with no package and no unsafe code: copy it into your project and your IDE sees every type and comment. .NET 8 or later.

## Integrate

From a clone to a call answered over the simulator, then your transport, your `.proto` and a signed fleet.

**1. Build the library.** A Rust toolchain is the one build dependency, until prebuilt binaries ship at 3.0:

```sh
cargo build --release -p mudrpc-c -p mudrpc-gen
```

That gives the library and `target/release/mud`. Copy `bindings/dotnet/Mud.cs` into your project. From 3.0, `dotnet add package mudrpc` gives the same: `Mud.cs` and `MudGrpc.cs` compiled with their documentation, and the library under `runtimes/<rid>/native`, where the resolver finds it.

**2. Make one call.** The smallest complete program: two peers in one process, a call and a reply moved by hand between them.

```csharp
using System;
using System.Linq;
using System.Text;
using MudRpc;

var key = System.Security.Cryptography.RandomNumberGenerator.GetBytes(32);   // the fleet key: 32 bytes from the OS, provisioned to every peer; never a constant and never a passphrase (32 equal bytes are refused, other constants are not)
uint epoch = 1;                                         // persisted beside the key; raise it on every boot and restore, write it first
using var gcs = new Peer("gcs", key, sender: 1, epoch: epoch, cipher: Peer.CipherDefault);     // roster numbers; 0 cannot send
using var drone = new Peer("drone", key, sender: 2, epoch: epoch, cipher: Peer.CipherDefault);
drone.Listen("nav.Flight");                                    // the drone's transport will subscribe to nav.Flight

byte[] callId = gcs.Call(now: 0, "nav.Flight", "Arm", new byte[] { 8, 1 }, ttlSecs: 30);   // a protobuf request, encoded by your protobuf library

var peers = new[] { gcs, drone };
for (uint now = 0; now < 3; now++)                      // your loop: pump each peer, every tick
{
    for (int i = 0; i < 2; i++)
    {
        var p = peers[i];
        while (p.Outgoing(now) is Outgoing o)
        {
            if (!o.Listen)                              // hand o.Topic, o.Wire, o.TtlSecs to your transport; Listen: subscribe to o.Topic
            {
                var incoming = peers[1 - i].Deliver(now, o.Topic, o.Wire);
                switch (incoming.Kind)
                {
                    case Kind.Call:
                        Console.WriteLine("drone: " + incoming.Method);
                        peers[1 - i].Reply(now, incoming, status: 0, payload: Encoding.UTF8.GetBytes("armed"));
                        break;
                    case Kind.Reply:
                        Console.WriteLine("gcs: status " + incoming.Status + " " + Encoding.UTF8.GetString(incoming.Bytes));
                        return 0;
                    case Kind.Dropped:
                        Console.WriteLine("dropped: " + incoming.Dropped);   // log in development, count in the field
                        break;
                }
            }
            p.Sent();                                   // only after the transport took it; if it refused, break instead
        }
        while (p.Tick(now) != null) Console.WriteLine("expired");
    }
}
return 2;
```

Point `MUDRPC_LIB` at the library, or put it in the build output next to your assembly: `<None Include="libmudrpc.dylib" CopyToOutputDirectory="PreserveNewest" />` in the csproj does that. Expected output: `drone: Arm` then `gcs: status 0 armed`, exit 0.

**3. The loop.** Every tick, drain `Outgoing()` into your transport, calling `Sent()` once it took each item (if it refused, `break`: the item is there next tick); feed what it received to `Deliver()`; call `Tick()`. `Outgoing()` also yields `Listen` items, one for the peer's own name (its reply inbox) and one per `Listen()`, and what a peer hears is what its transport subscribes to. `now` is your uptime in seconds; the 12-byte `byte[]` id returned by `Call` is the `Id` on the reply that answers it, compared with `SequenceEqual`, `Seq` and `End` carry streams and `Status` is gRPC's status number. `Incoming.Kind` is `Call`, `Reply`, `Event` (a publish), `Expired` (from `Tick()`, with the call's `Id`), `Dropped`, `Flood` or `Rollback`, and all but `Expired` and `Dropped` name their `Sender`. `Save()`, `Peer.Restore(saved, epoch)`, `Renew(epoch)` and `Dispose()` keep and end a peer under the host contract at the top of `bindings/c/mudrpc.h`, which every binding shares.

**4. Over the simulator.** Replace the hand-moving with the simulator, `new Sim(text)`, one radio per peer in place of your transport. The text is a scenario in the format `adapters/SKILL.md` gives, `"seed 1\npeers 2\nprofile reticulum\n0 partition 0 1\n30000 heal 0 1\n"` cutting the two radios apart until 30 s, so a call at 0 with ttl 60 is answered after the heal and one with ttl 10 expires; `Events()` lists its host events as `SimEvent(AtMs, Kind, A, B, Text)` for your harness to perform; a bad line throws `ArgumentException` naming it. Radio `i` is your `i`th peer, 0 to `Peers` − 1, whatever its sender number. Each round, for each peer `i`: hand its outgoing items to `Send(i, o.Topic, o.Wire, o.TtlSecs)` or `Listen(i, o.Topic)` (false while the radio is down, a `peer_down` row, not a partition, whose wire the mesh holds: `break`, no `Sent()`), `Deliver()` each `Poll(i)` result with `Kind == 1` until null (2 is on-mesh, 3 off-mesh), `Tick()`; then `Step(100)`. `now` is `(uint)(NowMs / 1000)`; `Note(text)` writes a host line and `Log()` returns the world's log; `Peers` and `LastEventMs` come from the text.

**5. Your transport.** In the simulator's place your transport takes each send item and listen topic, and what it receives goes to deliver; its rungs are below. On an IP network it can be the library's own, `new Udp("10.0.0.2:7000", "10.0.0.3:7000")`, with `Send(item)`, a listen among them, and `Poll()`, which yields only a listed peer's wires on a topic listened to, into `Deliver`, or given to a `MudChannel`, which pumps it every 10 ms, the most a reply waits there before it is read. It binds and sends to private, loopback or link-local addresses alone; `0.0.0.0` is public, since it binds every interface. A library built with the cargo feature `internet`, and `MUDRPC_INTERNET=listen-on-public-addresses` in the builder's environment, may bind and send to public ones; the feature alone fails the build, since any crate in it could set the feature, and no release is built with it. Cargo shows the build's own warning for the workspace alone, so a socket on a public address says so on standard error and `INTERNET` (`mud_internet()`, `Udp.Internet`) is the check a product makes at start. The variable belongs on the command line of the one build that means it, never in a `.cargo/config.toml`, where every build would go public in silence; `Udp.Internet` says which this library is. Reticulum's and DTN's adapters are their own people's, against `adapters/CONFORMANCE.md`.

**6. Your `.proto`.** If the product has gRPC code, keep it and read "Drop-in for gRPC" below. Otherwise encode request and reply bytes with your protobuf library of choice. The wire is protobuf; only the payload is yours. A proto service `Flight` in `package nav` is the service string `"nav.Flight"`, which the server listens on, with the rpc name as the method; `(mud.ttl_secs)` is the `ttlSecs` argument; a method with `(mud.topic)` is `Publish` on that topic, arrives as `Kind.Event` and has no reply.

**7. Streams.** Answer one call with `Reply(now, call, status, payload, end, seq, detail)`, `detail` the status's text and `""` by default, for `seq` 0, 1, 2… below 64 (above it a `MudException`, `Code == -3`), `end: true` on the last item only; it is the default, so a unary reply is seq 0 with `End`. The items arrive in any order, each a `Kind.Reply` with its `Seq` and `End`, and the call completes once every `Seq` up to the lowest `End` it has seen has arrived, or is `Kind.Expired` at its ttl with one missing. Send `end` once: an item above the lowest `End` is `Dropped.Seq`, a redelivered item `Dropped.Duplicate`, and anything after completion `Dropped.NoPending`.

**8. A signed fleet** (SPEC.md layer 1).

```sh
target/release/mud provision fleet 3
```

That writes `fleet/fleet.key` (`key <hex>`, once), `fleet/device-1` to `fleet/device-3` (`sender <n>`, `epoch 1`, `seed <hex>`, owner-only, each device's one secret) and `fleet/roster` (`<sender> <public key hex>` a line), 32-byte values in lowercase hex; run it again to add devices, numbered after the highest, and `--key` in place of the count for a rotation's next key, `fleet/fleet.key.next`; no file is ever overwritten. A device gets the fleet key, its own device file and the roster, and boots `new Peer(name, key, sender, epoch, seed, budget)` from them (`Convert.FromHexString` each value), then a `Roster(sender, key, null)` line for every sender it believes; its epoch starts at the file's and is raised under the host contract. `new Peer(name, key, sender, epoch, seed, budget)` with this sender's 32-byte Ed25519 seed and the signature checks per second this host affords (default 1000, at least 1; a wire past them is `Dropped.Open`, `Status` 6), a `Roster(sender, key, next)` line for every sender using `Peer.PublicKey(seed)`, `Revoke(sender)` to remove one. A peer believes 16 senders and the 17th line throws `MudException` with `IsFull`, so a ground station that must believe a fleet is built with `hub: true`, the same peer with 256 lines, 256 replay windows and 1024 remembered messages in a larger block. Its wire is version 4 (2 under ChaCha20-Poly1305): an unsigned peer gives it `Kind.Dropped` with `Dropped.Version` and `Status == 4`, and a signed peer an unsigned wire the same with `Status == 3`, so one fleet key never serves both. A discovered key or seed: SECURITY.md, "If a key is discovered".

`cipher` is `Peer.CipherDefault`, AES-256-GCM; `cipher: Peer.CipherChaCha20Poly1305` builds any of these peers under ChaCha20-Poly1305 instead: the same sizes and calls, wire version 1, or 2 signed, so a peer of the other cipher gives its wires `Dropped.Version` with that version in `Status`.

**9. Verify.** `cargo test -p mudrpc-c --test abi -- --include-ignored the_dotnet_skill_example_runs` runs step 2's program and every row of the table below.

## Drop-in for gRPC

What every language's drop-in shares, the one-line change, gRPC without HTTP/2, the dependencies and what is not carried, is [From gRPC](../../README.md#from-grpc) in the README.

The product keeps its `.proto`, Grpc.Tools in Visual Studio or Rider, and the client and service base it generates. `MudGrpc.cs`, beside `Mud.cs`, implements gRPC's own `ChannelBase` and `ServiceBinderBase` over a peer, with the NuGet package `Grpc.Core.Api` whose types they are:

```xml
<ItemGroup>
  <PackageReference Include="Google.Protobuf" Version="3.36.2" />
  <PackageReference Include="Grpc.Core.Api" Version="2.84.0" />
  <PackageReference Include="Grpc.Tools" Version="2.84.0" PrivateAssets="All" />
  <Protobuf Include="drone.proto" GrpcServices="Both" />
  <Compile Include="mudrpc/Mud.cs" />
  <Compile Include="mudrpc/MudGrpc.cs" />
</ItemGroup>
```

Building the channel is the line that changes; the client is the generated one, as before:

```csharp
// was: var channel = GrpcChannel.ForAddress("https://drone:5001");
var channel = new MudChannel(new Peer("gcs", fleetKey, sender: 1, epoch: epoch), new Udp("10.0.0.2:7000", "10.0.0.3:7000"));
var client = new Drone.DroneClient(channel);
var reply = await client.ArmAsync(new ArmRequest { Id = 7 }, deadline: DateTime.UtcNow.AddSeconds(30));
```

A server binds the generated base where ASP.NET Core mapped it, `Drone.BindService(channel.Binder, new DroneService())`, in any process: a console app, a service, a board's Linux, with no ASP.NET Core, Kestrel or IIS and no HTTP/2. Over `Udp` the channel sends and receives itself; another transport hands every wire it receives to `channel.Deliver(topic, wire)`, from any thread, and the channel gives it each item through the `send` it was built with; the channel pumps the peer on its own thread, so while it lives the peer is reached through `channel.Use(p => p.Save())`, the same for a key's `Stage`, `Rotate` and `Retire`, and the channel is disposed before the peer, from any thread, the transport's send included. An exception the transport throws goes to the channel's `fault` callback, and the item it threw on is tried for a second, then let go; past 1024 wires waiting for the pump, `Deliver` drops one, as core would drop a flood.

- **A deadline is the call's ttl**: no deadline is the channel's `ttlSecs`, 60 by default. `DeadlineExceeded` comes at the deadline, never before it, but for one past a week, the longest a message lives, which expires at a week; a deadline that is not UTC is an `InvalidOperationException`, as in Grpc.Net.Client.
- **A status is gRPC's**: an `RpcException` thrown by a handler, or a status set on `context.Status`, arrives with its code and detail; any other exception as `Unknown`, "Exception was thrown by handler.", as gRPC sends it. A reply too large for the wire reaches the caller as `ResourceExhausted`, and ends a stream there, its handler's writes failing.
- **A call on a service the channel does not serve** gets no answer, so a fleet where every node hears every wire still has one server answer it; a method missing from a service it serves is `Unimplemented`. A call's ttl counts from its arrival. Handlers run on up to 64 threads; a call waits for one, 1024 at most, and one whose caller has given up is shed unrun. The channel owes one caller 1024 replies at most, sixteen full streams, and all its callers 4096, taking them in turn, the longest line giving way past that; a flooding sender's calls go unanswered. Under one fleet key a holder can write a new sender number on every call, and the 32 a second are shared among all of them: sign the fleet where that matters.
- **`ServerCallContext.Peer`** is `mudrpc:<n>`, the caller's roster number, proven per device under a signed fleet.
- **Refused before sending**: a message past the wire, about 460 bytes, and a 17th call in flight, as `ResourceExhausted`; a service name that is empty, over 64 bytes or not UTF-8, and a method name that is not UTF-8, a lone surrogate, as `InvalidArgument`; a token already cancelled, as `Cancelled`. A call cancelled after it was sent keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
- **Paced**: a server sends 32 replies a second, half what a receiver takes from one sender, so no edge of a second drops one, each caller in turn; a 63-item stream takes two seconds, and a client keeps under 32 calls a second for the same reason.
- **A server stream** is read in order whatever order its items arrive, at most 63 items. A server's items are read before its status; a cancel, a deadline or a closed channel cuts them off, unread items included.
- **Not carried**: metadata, so headers and trailers are empty both ways; client and bidirectional streams, `Unimplemented`; context propagation.
- **With a native peer**: a drop-in server ends a stream with a reply that carries no item, end, OK and the detail `end` (`MudChannel.StreamEnd`); a native server's stream ends on its last item, and the drop-in client reads both.

`cargo test -p mudrpc-c --test abi -- --include-ignored the_dotnet_grpc_drop_in` runs a client and a service from Grpc.Tools over two peers, then against a native server, through every case above.

## Verify

- `new Peer(...)` throws `DllNotFoundException` naming the path it tried and the build command if the library is missing, never a crash.
- `MUDRPC_LIB` loads only what it names, with no fallback; set it only to a library you built.
- Every `Outgoing()` item reaches the transport before `Sent()`, which takes the item the last `Outgoing()` showed and nothing else; with nothing shown it is a no-op.
- `Deliver()` never throws for bad bytes: it returns an `Incoming` with `Kind == Kind.Dropped` and a reason.
- One thread drives a peer: a second thread's call finds the block held and throws `MudException` with `Code == -10`, nothing done; a detector of the mistake, not a lock.

The host mistakes the contract names, each with what it throws or returns, every row asserted by `the_dotnet_skill_example_runs`:

| Mistake | Answer | Kind |
| --- | --- | --- |
| `Restore` or `Renew` with an epoch at or below the saved one | `MudException`, `Code == -6` | error |
| sender 0 sending | `MudException`, `Code == -7`, at the first send | error |
| a key or seed of 32 equal bytes | `MudException`, `Code == -1`; another length is an `ArgumentException` first | error |
| `Save()` bytes of another length to `Restore` | `MudException`, `Code == -4` | error |
| a hub's `Save()` cut to a leaf's length | `MudException`, `Code == -5` | error |
| `hub: true` without a seed | `MudException`, `Code == -1` | error |
| `Save()` bytes retagged to the other cipher | `MudException`, `Code == -4` | error |
| saved bytes sent from without a resume | cannot happen: `Restore` resumes | none |
| ttl 0, or past a week | sent as 1, or a week | clamp |
| a topic or name over 64 bytes, or a payload over the ceiling | `MudException`, `Code == -3` | error |
| an empty name or topic | `MudException`, `Code == -1` | error |
| a reply to a call this peer does not have | `Deliver` returns `Dropped == Dropped.NoPending` | silent |
| 16 calls pending, or 16 items queued undrained | `MudException`, `IsFull`, `Code == -2`: decide what not to queue; `Pending` and `Queued` count them | error |
| the 33rd send in one second | `MudException`, `Code == -9`: the next second sends | error |

## Test at each rung

- **In-process:** step 2's program, or step 4's simulator. `tests/dotnet` next to the C binding runs every scenario in `scenarios/`, unsigned, `--signed`, `--gcm` and both, and its logs must equal `scenarios/<name>.log` (`<name>.gcm.log` where one is checked in) and the Rust harness's signed runs byte for byte.
- **Loopback (lan):** `cargo test -p mudrpc-adapters -- --include-ignored partition_heal_over_loopback` is the reference behaviour over real sockets.
- **Relay:** the same on a box on your own LAN, never on the internet.

## Known edges

- The host contract every binding shares (the epoch, save, restore, the nonce, the window, the ttl, threads, bounds, codes, close, the cipher) is the list at the top of `bindings/c/mudrpc.h`.
- `Counts` gives `managed`, `rollbacks`, `floods` and `dropped`, saturating and kept across restores.
- `Peer` and `Sim` own native memory: `Dispose()` them, or `using`; every later call on a peer throws `MudException` with `Code == -1`. The bytes `Save()` gives, the key you passed in and any copy the garbage collector made of them live in managed memory nobody can zero; `Dispose()` zeroes the library's copy only.
- Each call into the library pins and copies its arguments once; the copies are the price of not holding pointers into managed objects.
- A payload over the header's ceiling, about 460 bytes, is `MudException` with `Code == -3`.
- Topics and methods are UTF-8; a lone surrogate becomes U+FFFD. `now` and ttl are `uint`. A null byte array is a `NullReferenceException`; pass an empty one.
- `Mud.cs` registers the assembly's `DllImport` resolver; an assembly with a resolver of its own loads the library itself and drops that line.
