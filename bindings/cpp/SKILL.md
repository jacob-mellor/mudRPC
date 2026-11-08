---
name: mudrpc-cpp
description: How to integrate, verify and test the C++ binding for mudRPC, and its drop-in for gRPC C++.
---
mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The C++ binding is one header, `mud.hpp`, over the C ABI in `bindings/c/mudrpc.h`; `mud_grpc.hpp` beside it is gRPC C++'s sync API over mudRPC, and `protoc-gen-mud.cc` the protoc plugin that writes a `.proto`'s `Stub` and `Service` for it. C++17, the standard library and, for the drop-in, the protobuf the product already builds with.

## Integrate

**1. Build the library.** A Rust toolchain is the one build dependency, until prebuilt binaries ship at 3.0:

```sh
cargo build --release -p mudrpc-c
```

That gives `target/release/libmudrpc.dylib` (`.so` on Linux, `libmudrpc.a` to link statically). Put `mud.hpp` and `mudrpc.h` where your code includes them and link with `-L<dir> -lmudrpc`, `<dir>` the library's folder, and `-Wl,-rpath,<dir>` for the shared one.

From 3.0, the vcpkg port and the Conan package `mudrpc` give the same, each built by cargo on your machine: `libmudrpc.a`, `mudrpc.h`, `mud.hpp` and `mud_grpc.hpp`.

**2. Make one call.** The smallest complete program: two peers in one process, a call and a reply moved by hand between them.

```cpp
#include <cstdio>
#include <fstream>

#include "mud.hpp"

int main() {
  std::string key(32, '\0');   // the fleet key: 32 bytes from the OS, provisioned to every peer; never a constant and never a passphrase
  if (!std::ifstream("/dev/urandom", std::ios::binary).read(key.data(), 32)) return 1;
  uint32_t epoch = 1;   // persisted beside the key; raise it on every boot and restore, write it first
  mud::Peer gcs("gcs", key, 1, epoch);   // roster numbers; 0 cannot send
  mud::Peer drone("drone", key, 2, epoch);
  drone.listen("nav.Flight");   // the drone's transport will subscribe to nav.Flight

  gcs.call(0, "nav.Flight", "Arm", "\x08\x01", 30);   // a protobuf request, which the generated code encodes for you

  mud::Peer* peers[] = {&gcs, &drone};
  for (uint32_t now = 0; now < 3; now++) {   // your loop: pump each peer, every tick
    for (int i = 0; i < 2; i++) {
      while (auto o = peers[i]->outgoing(now)) {
        if (!o->listen) {   // hand o->topic, o->wire, o->ttl_secs to your transport; a listen: subscribe to o->topic
          mud::Incoming in = peers[1 - i]->deliver(now, o->topic, o->wire);
          if (in.kind == mud::Kind::call) {
            std::printf("drone: %s\n", in.method.c_str());
            peers[1 - i]->reply(now, in, "armed");
          } else if (in.kind == mud::Kind::reply) {
            std::printf("gcs: status %u %s\n", in.status, in.bytes.c_str());
            return 0;
          } else if (in.kind == mud::Kind::dropped) {
            std::printf("dropped: %d\n", in.dropped);   // log in development, count in the field
          }
        }
        peers[i]->sent();   // only after the transport took it; if it refused, break instead
      }
      while (peers[i]->tick(now)) std::printf("expired\n");
    }
  }
  return 2;
}
```

It prints `drone: Arm` then `gcs: status 0 armed` and exits 0.

**3. The loop.** Every tick, drain `outgoing(now)` into your transport, calling `sent()` once it took each item (if it refused, `break`: the item is there next tick); feed what it received to `deliver(now, topic, wire)`; call `tick(now)`. `outgoing` also yields listen items, one for the peer's own name (its reply inbox) and one per `listen`, and what a peer hears is what its transport subscribes to. `now` is your uptime in whole seconds, from a clock that only moves forward; the 12-byte id `call` returns is the `id` on the reply that answers it, `seq` and `end` carry streams and `status` is gRPC's status number, `mud::status_name(status)` its name. `Incoming::kind` is `call`, `reply`, `event` (a publish), `expired` (from `tick`, with the call's `id`), `dropped`, `flood` or `rollback`, and all but `expired` and `dropped` name their `sender`. Bytes are `std::string`, taken as `std::string_view`. `save()`, `Peer::restore(saved, epoch)`, `renew(epoch)` and `close()` keep and end a peer under the host contract at the top of `bindings/c/mudrpc.h`, which every binding shares; every error is a `mud::Error` whose `code` is that header's. A peer is one thread's: the drop-in's channel owns it and lends it out through `use`.

**4. A signed fleet and the cipher.** `target/release/mud provision fleet 3` writes the fleet key, one device file per sender and the roster, as the C skill describes; a device builds `mud::Peer(name, key, sender, epoch, seed, budget)` from them and adds `roster(sender, public_key)` for every sender it believes, with `Peer::public_key(seed)` and `revoke(sender)`. `hub` true believes 256 senders in place of 16. The cipher is `Peer::cipher_default`, AES-256-GCM, unless `Peer::cipher_chacha20_poly1305` is passed; a fleet runs one.

**5. UDP.** `mud::Udp("10.0.0.2:7000", "10.0.0.3:7000")` sends each wire to every listed peer, apart by spaces or commas, and hears only them, on the topics its peer listens to; a public address, `0.0.0.0` included, is `mud::Error` -1 unless the library was built with the cargo feature `internet` and `MUDRPC_INTERNET=listen-on-public-addresses`, which no release is (`Udp::internet()` says which), and a bind the system refuses is -2. `send(item)` takes an outgoing item and throws -2 when no peer took a wire; `poll()` gives the next `(topic, wire)` or none.

## Drop-in for gRPC

What every language's drop-in shares, the one-line change, gRPC without HTTP/2, the dependencies and what is not carried, is [From gRPC](../../README.md#from-grpc) in the README.

gRPC C++ publishes no transport to replace, so `protoc-gen-mud`, adapted from gRPC's C++ plugin, writes the same `Stub` and `Service` for each service of a `.proto`, its sync API, into `X.mud.pb.h` beside protoc's own `X.pb.h`. The product keeps its `.proto`, protoc's messages and its service; it builds the plugin once against the protobuf it already has, and runs protoc with it in place of `grpc_cpp_plugin`:

```sh
c++ -std=c++17 protoc-gen-mud.cc $(pkg-config --cflags --libs protobuf) -lprotoc -o protoc-gen-mud
protoc --plugin=protoc-gen-mud=./protoc-gen-mud --cpp_out=. --mud_out=. drone.proto
```

In CMake those are an `add_executable` and the protoc line in an `add_custom_command`. Then its include changes, `grpc::` becomes `mud::`, and the line that builds the channel names a peer and its radio; no HTTP/2, no server stack and no port:

```cpp
#include "drone.mud.pb.h"   // was: <grpcpp/grpcpp.h> and "drone.grpc.pb.h"

// was: auto channel = grpc::CreateChannel("10.0.0.3:5001", grpc::InsecureChannelCredentials());
auto channel = mud::CreateChannel(mud::Peer("gcs", fleet_key, 1, epoch), std::make_shared<mud::Udp>("10.0.0.2:7000", "10.0.0.3:7000"));
auto stub = nav::Drone::NewStub(channel);
mud::ClientContext context;
context.set_deadline(std::chrono::system_clock::now() + std::chrono::seconds(30));
nav::ArmReply reply;
mud::Status status = stub->Arm(&context, request, &reply);
```

A server is `mud::ServerBuilder`, `AddListeningPort(channel)` in place of an address and its credentials, `RegisterService(&service)`, `BuildAndStart()` and `Wait()`, in any process; one channel is a client and one server at once. While it lives the peer is reached through `channel->use([](mud::Peer& p) { return p.save(); })`, the same for a key's `stage`, `rotate` and `retire`, and its function must not call the channel. Another radio is a function, `mud::CreateChannel(peer, [](const mud::Outgoing& o) { ... })`, taking each outgoing item on the pump's thread with no lock held, so it may wait on a thread that calls into the channel, and it hands every wire it receives to `channel->deliver(topic, wire)`; the first error it throws on an item goes to the fault function, as does a listen the peer refuses, CreateChannel's last argument, called the same way, and the item is tried for a second, then let go. The default fault writes to stderr, as gRPC logs. The channel pumps on a thread of its own, asleep when it has no socket and nothing timed waits, and holds itself only for a round, so the last `shared_ptr` let go frees it: a radio that reaches its own channel holds a `weak_ptr`. Deadlines run on `steady_clock`, so a wall clock stepped by NTP moves none. `close()` ends every call `UNAVAILABLE`, and a channel closed under its owner, by a peer or socket gone, answers every call so.

- **A deadline is the call's ttl**: `set_deadline` on any clock; none, or `time_point::max()`, is the channel's `ttl_secs`, 60 by default, where gRPC's is infinite, and a stream's too: a subscription that must outlive it is called again, or is a publish. `set_wait_for_ready` asks nothing more, since every call waits for its link until its deadline. `DEADLINE_EXCEEDED` comes at the deadline, but for one past a week, the longest a message lives, which expires at a week. `TryCancel`, from any thread, is `CANCELLED`, before the call starts too, and a context destroyed mid-call cancels it; a call already over and read keeps its status.
- **A status is gRPC's**: a handler's `Status` arrives with its code and message, the first 128 bytes of its UTF-8 kept and any other byte dropped; a handler's exception arrives `UNKNOWN` "Unexpected error in RPC handling", gRPC's words. A server's items reach the caller before its status; a cancel, a deadline or a closed channel cuts them off, unread items included, and a reader past its deadline keeps only a call that had ended and been read. A reply too large for the wire reaches the caller as `RESOURCE_EXHAUSTED`, and ends a stream there, its handler's writes failing. A handler's `IsCancelled()` is true once its call's ttl passes, counted from the call's arrival, its channel closes or its server's `Shutdown` deadline passes, and its `Write` returns false from then. A `ServerContext` built by a test, to call a handler directly, is never cancelled and names no caller.
- **`context->peer()`** is `mudrpc:` and the caller's roster number, proven per device under a signed fleet.
- **A call on a service the channel does not serve** gets no answer, so a fleet where every node hears every wire still has one server answer it; a method missing from a service it serves is `UNIMPLEMENTED`. Handlers run on up to 64 threads; a call waits for one, 1024 at most, and one whose caller has given up is shed unrun. The channel owes one caller 1024 replies at most, sixteen full streams, and all its callers 4096, taking them in turn, the longest line giving way past that; a flooding sender's calls go unanswered. Under one fleet key a holder can write a new sender number on every call, and the 32 a second are shared among all of them: sign the fleet where that matters. A service's full name is 1 to 64 bytes of UTF-8, or `BuildAndStart()` returns `nullptr` and tells the fault why, as it does for a second server on one channel.
- **`Shutdown()`** takes no new calls and returns once the running and waiting ones have finished; `Shutdown(deadline)` cancels the running and the waiting at the deadline, their callers `CANCELLED`. `Wait()` returns then, or once the channel closes and its handlers have returned.
- **Refused before sending**: a message past the wire, about 460 bytes, and a 17th call in flight, as `RESOURCE_EXHAUSTED`; a service name that is not 1 to 64 bytes of UTF-8 and a method name that is not UTF-8 as `INVALID_ARGUMENT`. A call cancelled after it was sent keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
- **Paced**: a server sends 32 replies a second, half what a receiver takes from one sender, each caller in turn; a 63-item stream takes two seconds. A client is not paced: keep it under 32 calls a second, for the same reason, and the 65th in one second is `RESOURCE_EXHAUSTED`.
- **Not carried**, so none of it compiles: metadata and binary details (`AddMetadata`, `SendInitialMetadata`, `WaitForInitialMetadata`, `error_details`), write options, channel arguments and message-size settings, and a connection's state (`GetState`, `WaitForConnected`), since there is none; the async (`CompletionQueue`) and callback APIs, which are not generated; client and bidirectional streams, generated so the product compiles, whose `Write` is false and `Finish` `UNIMPLEMENTED`, as a native caller's call on one is answered. Over `mud::Udp` a channel listens on its own name in `CreateChannel` and on its services in `BuildAndStart`, before the call returns, so a server is called the line after it starts; over a function radio the listens reach it on the pump's thread.
- **With a native peer**: a drop-in server ends a stream with a reply that carries no item, end, OK and the detail `end` and sends a unary reply as one item with end; a native server's stream ends on its last item, and the drop-in client reads both.

`cargo test -p mudrpc-c --test abi -- --include-ignored the_cpp_drop_in` builds the plugin, generates `bindings/c/tests/grpc-cpp/drone.proto` with protoc, and runs a gRPC C++ client and service through every case above, over two peers, over UDP and against a native peer, plain and under ThreadSanitizer and AddressSanitizer with UndefinedBehaviorSanitizer, and step 2's program; it finds protobuf, protoc and libprotoc by `pkg-config`.

## Verify

- `Peer::restore` refuses bytes of neither shape's size as `mud::Error` -4 before touching the library, and a closed peer answers -4 at every call, its counts 0; `close()` waits for an entry another thread is in to end, and the block's memory lives as long as the `Peer`.
- Every `outgoing` item reaches the transport before `sent()`, which takes the item the last `outgoing` showed and nothing else.
- `deliver` never throws for bad bytes: it returns a `dropped` verdict with a reason.
- Two threads on one peer is `mud::Error` -10 with nothing done, a detector of the host's fault and not a lock: hold a peer on one thread, or let the channel hold it.
