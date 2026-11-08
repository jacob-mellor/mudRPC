---
name: mudrpc-go
description: How to integrate, verify and test the Go binding for mudRPC, and its drop-in for gRPC.
---
mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The Go binding is one package over the C ABI in `bindings/c/mudrpc.h` through cgo: `mud.go`, the peer, with no dependency beyond the standard library, and `mud_grpc.go` beside it, the drop-in for gRPC, with the `google.golang.org/grpc` and `google.golang.org/protobuf` modules the product already has.

## Integrate

**1. Build the library.** A Rust toolchain is the one build dependency, until prebuilt binaries ship at 3.0:

```sh
cargo build --release -p mudrpc-c
```

That gives `target/release/libmudrpc.dylib` (`.so` on Linux). Copy the folder `bindings/go` into your module as a package named `mud`, with `mudrpc.h` beside its two files (a product with no gRPC leaves `mud_grpc.go` out), or from 3.0 `go get github.com/jacob-mellor/mudRPC/bindings/go/v3`, the same two files as a module, and tell cgo where the library is: `CGO_LDFLAGS="-L<dir>"` in the environment of `go build` and `go run`, `<dir>` the library's folder, and `-ldflags="-extldflags=-Wl,-rpath,<dir>"` on the command, once, for the final link alone (in `CGO_LDFLAGS` the rpath reaches every cgo package and the linker warns of the duplicate).

**2. Make one call.** The smallest complete program: two peers in one process, a call and a reply moved by hand between them.

```go
package main

import (
	"crypto/rand"
	"fmt"
	"os"

	"example/mud" // the package you copied in, under your module's path
)

func main() {
	key := make([]byte, 32) // the fleet key: 32 bytes from the OS, provisioned to every peer; never a constant and never a passphrase
	rand.Read(key)
	epoch := uint32(1)                        // persisted beside the key; raise it on every boot and restore, write it first
	gcs, err := mud.New("gcs", key, 1, epoch) // roster numbers; 0 cannot send
	if err != nil {
		panic(err)
	}
	drone, _ := mud.New("drone", key, 2, epoch)
	drone.Listen("nav.Flight") // the drone's transport will subscribe to nav.Flight

	gcs.Call(0, "nav.Flight", "Arm", []byte{8, 1}, 30) // now 0; a protobuf request, which the generated code encodes for you

	peers := []*mud.Peer{gcs, drone}
	for now := uint32(0); now < 3; now++ { // your loop: pump each peer, every tick
		for i, p := range peers {
			for o, _ := p.Outgoing(now); o != nil; o, _ = p.Outgoing(now) {
				if !o.Listen { // hand o.Topic, o.Wire, o.TTL to your transport; a listen: subscribe to o.Topic
					in, _ := peers[1-i].Deliver(now, o.Topic, o.Wire)
					switch in.Kind {
					case mud.Call:
						fmt.Println("drone:", in.Method)
						peers[1-i].Reply(now, in, []byte("armed"))
					case mud.Reply:
						fmt.Println("gcs: status", in.Status, string(in.Bytes))
						os.Exit(0)
					case mud.Dropped:
						fmt.Println("dropped:", in.Dropped) // log in development, count in the field
					}
				}
				p.Sent() // only after the transport took it; if it refused, break instead
			}
			for e, _ := p.Tick(now); e != nil; e, _ = p.Tick(now) {
				fmt.Println("expired")
			}
		}
	}
	os.Exit(2)
}
```

`go run .` prints `drone: Arm` then `gcs: status 0 armed` and exits 0.

**3. The loop.** Every tick, drain `Outgoing(now)` into your transport, calling `Sent()` once it took each item (if it refused, `break`: the item is there next tick); feed what it received to `Deliver(now, topic, wire)`; call `Tick(now)`. `Outgoing` also yields listen items, one for the peer's own name (its reply inbox) and one per `Listen`, and what a peer hears is what its transport subscribes to. `now` is a `uint32`, your uptime in whole seconds from a clock that only moves forward; the 12-byte id `Call` returns is the `Id` on the reply that answers it, `Seq` and `End` carry streams, `Status` is gRPC's status number and `mud.StatusName(status)` its name. `Incoming.Kind` is `mud.Call`, `mud.Reply`, `mud.Event` (a publish), `mud.Expired` (from `Tick`, with the call's `Id`), `mud.Dropped`, `mud.Flood` or `mud.Rollback`, and all but `Expired` and `Dropped` name their `Sender`. `Save()`, `mud.Restore(saved, epoch)`, `Renew(epoch)` and `Close()` keep and end a peer under the host contract at the top of `bindings/c/mudrpc.h`, which every binding shares; every error is a `*mud.Error` whose `Code` is that header's. A peer's methods serialize on a mutex, so any goroutine may call them, one at a time; the drop-in's channel owns its peer and lends it out through `Use`.

**4. Streams.** Answer one call with `ReplyItem(now, call, seq, status, detail, payload, end)` for `seq` 0, 1, 2… below 64 (above it -3), `end` true on the last item only; `Reply(now, call, payload)` is seq 0, status 0 and end, a unary reply. The items arrive in any order, each a `Reply` with its `Seq` and `End`, and the call completes once every `Seq` up to the lowest `End` it has seen has arrived, or is `Expired` at its ttl with one missing.

**5. A signed fleet and the cipher.** `target/release/mud provision fleet 3` writes the fleet key, one device file per sender and the roster, as the C skill describes; a device builds `mud.New(name, key, sender, epoch, mud.Config{Seed: seed, Budget: 1000})` from them and adds `Roster(sender, publicKey, nil)` for every sender it believes, with `mud.PublicKey(seed)` and `Revoke(sender)`. `Config{Hub: true}` believes 256 senders in place of 16. The cipher is `Config`'s zero value, AES-256-GCM, unless `Cipher: mud.CipherChaCha20Poly1305` is set; a fleet runs one.

**6. UDP.** `mud.NewUdp("10.0.0.2:7000", "10.0.0.3:7000")` sends each wire to every listed peer and hears only them, on the topics its peer listens to; a public address, `0.0.0.0` included, is -1 unless the library was built with the cargo feature `internet` and `MUDRPC_INTERNET=listen-on-public-addresses`, which no release is (`mud.Internet()` says which), and a bind the system refuses is -2. `Send(item)` takes an outgoing item and fails -2 when no peer took a wire; `Poll()` gives the next topic and wire, the wire nil when none waits; after `Close()`, every call is `mud.ErrClosed`.

## Drop-in for gRPC

What every language's drop-in shares, the one-line change, gRPC without HTTP/2, the dependencies and what is not carried, is [From gRPC](../../README.md#from-grpc) in the README.

The product keeps its `.proto`, protoc with protoc-gen-go and protoc-gen-go-grpc, and the client and server they generate. `mud_grpc.go` is a `grpc.ClientConnInterface`, what `pb.NewDroneClient` takes, and a `grpc.ServiceRegistrar`, what `pb.RegisterDroneServer` takes, over one peer and its radio; no `grpc.NewClient`, no `grpc.NewServer` and no listener:

```go
// was: conn, err := grpc.NewClient("10.0.0.3:5001", grpc.WithTransportCredentials(insecure.NewCredentials()))
radio, err := mud.NewUdp("10.0.0.2:7000", "10.0.0.3:7000")
peer, err := mud.New("gcs", fleetKey, 1, epoch)
conn := mud.NewChannel(peer, radio)
drone := pb.NewDroneClient(conn)
ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
defer cancel()
reply, err := drone.Arm(ctx, &pb.ArmRequest{Id: 7})
```

A server is `pb.RegisterDroneServer(conn, &droneService{})`, the line that took `grpc.NewServer()`, in any process; one channel is a client and a server at once, and there is no `Serve` to call. While it lives the peer is reached through `conn.Use(func(p *mud.Peer) { saved, err = p.Save() })`, the same for a key's `Stage`, `Rotate` and `Retire`, and the function must not call the channel but to `Close` it. Another radio is a `mud.RadioFunc`, taking each outgoing item on the pump's goroutine with no lock held, so it may wait on a goroutine that calls into the channel, and it hands every wire it receives to `conn.Deliver(topic, wire)`; the first error it returns on an item goes to the function of the `mud.Fault` option, run on the pump after its round with no lock held, and the item is tried for a second, then let go. The default fault writes to stderr, as grpc logs. `mud.TTL(secs)` sets the life of a call made with no deadline. `conn.Close()` ends every call `Unavailable` and stops the pump, from any goroutine, a handler, `Use`, the radio's `Send` or the fault function included; it returns once the pump has stopped, or at once while any goroutine is inside `Use` or `Send`, the pump ending on its next round. A channel closed under its owner, by a peer or socket gone, answers every call so; close the channel before the peer.

- **A deadline is the call's ttl**: the context's; none is the channel's `TTL`, 60 by default, where gRPC's is infinite, and a stream's too: a subscription that must outlive it is called again, or is a publish. `DeadlineExceeded` comes at the deadline, from the context's own timer, but for one past a week, the longest a message lives, which expires at a week. A context cancelled is `Canceled`, before the call starts too; a call already over and read keeps its status.
- **A status is gRPC's**: a handler's `status.Error` arrives with its code and message, the first 128 bytes of its UTF-8 kept and any other byte dropped; an error that is no status arrives `Unknown` with its text, as grpc sends it, and a handler's panic `Unknown` "panic: …", where grpc would end the process. A server's items reach the caller before its status; a cancel, a deadline or a closed channel cuts them off, unread items included. A reply too large for the wire reaches the caller as `ResourceExhausted`, and ends a stream there, its handler's `Send` failing. A handler's context is done once its call's ttl passes, counted from the call's arrival, its channel closes or its `Stop` grace passes, and a stream's `Send` fails from then.
- **`peer.FromContext(ctx)`** names the caller as `mudrpc:` and its roster number, proven per device under a signed fleet.
- **A call on a service the channel does not serve** gets no answer, so a fleet where every node hears every wire still has one server answer it; a method missing from a service it serves is `Unimplemented` "unknown method", grpc's words, and one the service leaves out answers as its `Unimplemented…Server` does. Handlers run on up to 64 goroutines; a call waits for one, 1024 at most, and one whose caller has given up is shed unrun. The channel owes one caller 1024 replies at most, sixteen full streams, and all its callers 4096, taking them in turn, the longest line giving way past that; a reply that finds no room ends its call for its handler, `ResourceExhausted`, and a flooding sender's calls go unanswered. Under one fleet key a holder can write a new sender number on every call, and the 32 a second are shared among all of them: sign the fleet where that matters. A service's full name is 1 to 64 bytes of UTF-8, or `RegisterService` panics, as grpc's does for a handler of the wrong type.
- **`Stop(grace)`** is `Stop` and `GracefulStop` in one: no new calls from then; past `grace`, at once for 0, the running and waiting are answered `Canceled`, their contexts done. The channel it returns is closed once every handler has returned.
- **Refused before sending**: a message past the wire, about 460 bytes, and a 17th call in flight, as `ResourceExhausted`; a service name that is not 1 to 64 bytes of UTF-8 and a method name that is not UTF-8 as `InvalidArgument`; a context already done, with its own status. A call cancelled after it was sent keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
- **Paced**: a server sends 32 replies a second, half what a receiver takes from one sender, each caller in turn; a 63-item stream takes two seconds. A client is not paced: keep it under 32 calls a second, for the same reason.
- **Not carried**: metadata, so headers and trailers are empty; call options, codecs and compression, since messages are `proto.Marshal`'s; interceptors on the channel, which wrap the client or the handler in the product's code; a connection's state, since there is none; client and bidirectional streams, `Unimplemented` from `NewStream`, as a native caller's call on one is answered. Over `Udp` a channel listens on its own name in `NewChannel` and on a service in `RegisterService`, before the call returns, so a server answers a call made the moment it is registered; another radio takes its listen items on the pump.
- **With a native peer**: a drop-in server ends a stream with a reply that carries no item, end, OK and the detail `end` (`mud.StreamEnd`) and sends a unary reply as one item with end; a native server's stream ends on its last item, and the drop-in client reads both.

`cargo test -p mudrpc-c --test abi -- --include-ignored the_go_drop_in` builds protoc-gen-go and protoc-gen-go-grpc from the modules `bindings/c/tests/grpc-go/go.sum` pins, generates `drone.proto` with protoc, and runs a grpc-go client and server through every case above, over two peers, over UDP and against a native peer, plain and under the race detector, and step 2's program; it needs Go and protoc.

## Verify

- A library the linker cannot find fails `go build` with `library 'mudrpc' not found`, never a crash at run time: `CGO_LDFLAGS` names its folder.
- `mud.Restore` refuses bytes of neither shape's size as -4 before touching the library, and a closed peer answers -4 at every call, its gauges 0.
- Every `Outgoing` item reaches the transport before `Sent()`, which takes the item the last `Outgoing` showed and nothing else.
- `Deliver` never fails for bad bytes: it returns a `Dropped` verdict with a reason.
- Two goroutines on one peer take turns on the binding's mutex; the library's -10, two threads inside one entry, is never reached through it.

The host mistakes the contract names, each with what it answers:

| Mistake | Answer | Kind |
| --- | --- | --- |
| `Restore` or `Renew` with an epoch at or below the saved one | `*Error`, `Code == -6` | error |
| sender 0 sending | `Code == -7`, at the first send | error |
| a key or seed of 32 equal bytes, or of another length | `Code == -1` | error |
| `Save` bytes of another length to `Restore` | `Code == -4` | error |
| `Hub` without a seed | `Code == -1` | error |
| ttl 0, or past a week | sent as 1, or a week | clamp |
| a topic or name over 64 bytes, or a payload over the ceiling | `Code == -3` | error |
| an empty name or topic, or one that is not UTF-8 | `Code == -1` | error |
| a reply to a call this peer does not have | `Deliver` returns `Dropped == 5` | silent |
| 16 calls pending, or 16 items queued undrained | `Code == -2`: decide what not to queue; `Pending` and `Queued` count them | error |
| the 33rd send in one second | `Code == -9`: the next second sends | error |

## Test at each rung

- **In-process:** step 2's program, or the drop-in test's two peers.
- **Loopback (lan):** `cargo test -p mudrpc-adapters -- --include-ignored partition_heal_over_loopback` is the reference behaviour over real sockets.
- **Relay:** the same on a box on your own LAN, never on the internet.

## Known edges

- The host contract every binding shares (the epoch, save, restore, the nonce, the window, the ttl, threads, bounds, codes, close, the cipher) is the list at the top of `bindings/c/mudrpc.h`.
- `Counts()` gives `Managed`, `Rollbacks`, `Floods` and `Dropped`, saturating and kept across restores.
- `Close()` zeroes the library's copy of the key; the bytes `Save` gives, and the key you passed in, are yours to `clear` once stored.
- Each call into the library pins its arguments for the call and copies its results out; nothing holds a pointer into Go memory past the call, and the block and the scratch buffer are C memory, freed by `Close`.
- cgo: a C compiler at build time and `CGO_ENABLED=1`, the default on a host with one; a cross build needs the library built for the target.
