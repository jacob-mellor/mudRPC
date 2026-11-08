---
name: mudrpc-php
description: How to integrate, verify and test the PHP binding for mudRPC, and run the client grpc_php_plugin generates over it.
---
mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The PHP binding is one file, `Mud.php`, over the C ABI through PHP's FFI extension (`php -m` lists `FFI`; the default `ffi.enable=preload` lets the CLI load it, while php-fpm and Apache need `ffi.enable=1` or `Mud.php` in `opcache.preload`), with no composer package.

## Integrate

From a clone to a call answered by hand, then your transport, your `.proto` and a signed fleet.

**1. Build the library.** A Rust toolchain is the one build dependency, until prebuilt binaries ship at 3.0:

```sh
cargo build --release -p mudrpc-c
```

That gives `target/release/libmudrpc.dylib` (`.so` on Linux). Copy `bindings/php/Mud.php` beside your script and put the library next to it, or point `MUDRPC_LIB` at it.

**2. Make one call.** The smallest complete program: two peers in one process, a call and a reply moved by hand between them.

```php
<?php
require __DIR__ . '/Mud.php';

use Mud\Peer;

$key = random_bytes(32);                                 // the fleet key: 32 bytes from the OS, provisioned to every peer; never a constant and never a passphrase (32 equal bytes are refused, other constants are not)
$epoch = 1;                                              // persisted beside the key; raise it on every boot and restore, write it first
$gcs = new Peer('gcs', $key, sender: 1, epoch: $epoch, cipher: Mud\CIPHER_DEFAULT);     // roster numbers; 0 cannot send
$drone = new Peer('drone', $key, sender: 2, epoch: $epoch, cipher: Mud\CIPHER_DEFAULT);
$drone->listen('nav.Flight');                            // the drone's transport will subscribe to nav.Flight

$callId = $gcs->call(0, 'nav.Flight', 'Arm', "\x08\x01", ttl: 30);   // now 0; a protobuf request, which the stubs encode for you

$peers = [$gcs, $drone];
for ($now = 0; $now < 3; $now++) {                       // your loop: pump each peer, every tick
    foreach ($peers as $i => $p) {
        while (($o = $p->outgoing($now)) !== null) {
            if ($o->kind === 'send') {                   // hand $o->topic, $o->wire, $o->ttl to your transport; 'listen': subscribe to $o->topic
                $in = $peers[1 - $i]->deliver($now, $o->topic, $o->wire);
                if ($in->kind === 'call') {
                    echo "drone: $in->method\n";
                    $peers[1 - $i]->reply($now, $in, status: 0, payload: 'armed');
                } elseif ($in->kind === 'reply') {
                    echo "gcs: status $in->status $in->bytes\n";
                    exit(0);
                } elseif ($in->kind === 'dropped') {
                    echo "dropped: $in->dropped\n";        // log in development, count in the field
                }
            }
            $p->sent();                                  // only after the transport took it; if it refused, break instead
        }
        while ($p->tick($now) !== null) {
            echo "expired\n";
        }
    }
}
exit(2);
```

`MUDRPC_LIB=/path/to/libmudrpc.dylib php example.php` prints `drone: Arm` then `gcs: status 0 armed` and exits 0.

**3. The loop.** Every tick, drain `outgoing()` into your transport, calling `sent()` once it took each item (if it refused, `break`: the item is there next tick); feed what it received to `deliver()`; call `tick()`. `outgoing()` also yields `listen` items, one for the peer's own name (its reply inbox) and one per `listen()`, and what a peer hears is what its transport subscribes to. `$now` is an `int`, your uptime in seconds; the 12-byte string `call()` returns is the `id` on the reply that answers it, `seq` and `end` carry streams and `status` is gRPC's status number. `Incoming::$kind` is `call`, `reply`, `event` (a publish), `expired` (from `tick()`, with the call's `id`), `dropped`, `flood` or `rollback`, and all but `expired` and `dropped` name their `sender`. `save()`, `Peer::restore($saved, $epoch)`, `renew($epoch)` and `close()` keep and end a peer under the host contract at the top of `bindings/c/mudrpc.h`, which every binding shares.

**4. Over UDP.** `new Mud\Udp('10.0.0.5:4000', '10.0.0.7:4000')` binds the first address and sends every wire to each of the others; it is a `Mud\Radio`, `send($item)` for each outgoing item and `poll()` for the next `[$topic, $wire]`, so the loop above is the same with it in place of the hand-moving. Private addresses only: a public bind or peer is `Mud\Error` -1 in every release build, and `Mud\Udp::internet()` says whether this library may do otherwise. Your own transport is a class with those two methods.

**5. Your `.proto`.** The composer package `google/protobuf` and `protoc --php_out` give the messages; `serializeToString()` is the payload of a call or reply and `mergeFromString()` reads one. For the services, the drop-in below runs the client `grpc_php_plugin` generates; a PHP server answers each `call` with `reply()` as step 2 does, `seq` 0, 1, 2… and `end` on the last item of a stream, or `detail: Mud\STREAM_END` with no item when the stream has none.

**6. A signed fleet and the cipher.** `new Peer($name, $key, $sender, $epoch, seed: $seed)` signs every wire with the sender's 32-byte Ed25519 seed, and a receiver believes only the senders on its roster: `roster($sender, Peer::publicKey($seed))`, 16 lines for a leaf, 256 with `hub: true`. `stage($next)`, `rotate()` and `retire()` turn the fleet key over in three steps; `opened()` says which slot opened the last wire. `cipher:` is `Mud\CIPHER_AES_256_GCM` (the default) or `Mud\CIPHER_CHACHA20_POLY1305`; a fleet runs one.

## Drop-in for gRPC

What every language's drop-in shares, the one-line change, gRPC without HTTP/2, the dependencies and what is not carried, is [From gRPC](../../README.md#from-grpc) in the README.

A gRPC PHP product keeps its `.proto`, the client `grpc_php_plugin` generates and the composer package `grpc/grpc`, and changes the options that build the stub. `MudGrpc.php` beside `Mud.php` is a `Grpc\CallInvoker` over one peer; the grpc extension is not needed, and where it is not loaded the file defines the `Grpc\STATUS_*` numbers and the credentials class `BaseStub` asks about.

```php
// before
$drone = new Test\DroneClient('drone.example.com:50051', ['credentials' => Grpc\ChannelCredentials::createInsecure()]);
// after
$mud = new Mud\Channel(new Mud\Peer('gcs', $key, 1, $epoch), new Mud\Udp('10.0.0.5:4000', '10.0.0.7:4000'));
$drone = new Test\DroneClient('drone', ['credentials' => null, 'grpc_call_invoker' => $mud]);
```

`$drone->Arm($request)->wait()` gives `[$reply, $status]` and `$drone->Telemetry($request)->responses()` the items, then `getStatus()`, as with gRPC's own channel. What differs:

- **A client, as gRPC's PHP is.** PHP has one thread, so the channel pumps the peer and the radio while a call waits, in `wait()`, `responses()` and `getStatus()`, and in `pump()` when your own loop calls it between those; the peer is yours otherwise, `$mud->peer`, for `save()`, `stage()`, `rotate()`, `roster()` and `counts()`. A server in PHP answers with `Peer::reply` as step 5 says.
- **A timeout is the call's ttl**: `['timeout' => 2_000_000]` in microseconds, as gRPC PHP's, is `DEADLINE_EXCEEDED` after two seconds; without one the call lives the channel's `ttl` (60 s unless given), a week at most; `INF` or a non-number is no deadline, 0 or less `DEADLINE_EXCEEDED` at once. `cancel()` ends a call with `CANCELLED`; a call cancelled after it was sent keeps its place among the 16 in flight until its ttl, since nothing reaches the server to say so.
- **Refused where mudRPC cannot carry it**: a request over about 460 bytes, or the seventeenth call in flight, `RESOURCE_EXHAUSTED` with the library's reason; a service name that is not 1 to 64 bytes of UTF-8, or a method name that is not UTF-8, `INVALID_ARGUMENT`; a client or bidirectional stream, `UNIMPLEMENTED`, its `read()` null. Each call object is a `Grpc\UnaryCall`, `Grpc\ServerStreamingCall`, `Grpc\ClientStreamingCall` or `Grpc\BidiStreamingCall`, as the generated client declares; a call `wait()`ed for without `start()` is `INTERNAL`, not a wait forever.
- **The first reply wins**; items of a stream are taken in `seq` order whatever order they arrive, and the server's status comes after the items before it. A reply that does not decode throws protobuf's own `GPBDecodeException` from `wait()` or `responses()`, as gRPC PHP's does.
- **Closed**: `$mud->close()`, or the stub's `close()`, ends every call in flight `UNAVAILABLE`, as does the peer closed first or the socket closed first; the channel never closes the peer.
- **A radio's exception** from `send()` or `poll()` goes to the channel's `fault` callable; the item `send()` raised on is tried again each round for a second, then let go, and the round goes on, so a call still meets its deadline.
- **Not carried**: metadata, so `getMetadata()` and `getTrailingMetadata()` are empty; interceptors, which `grpc/grpc` routes through its own extension's channel; compression.
- **With a native peer**: a native server's stream ends on its last item, or with a reply that carries no item and the detail `end` (`Mud\STREAM_END`), and the client reads both.

`cargo test -p mudrpc-c --test abi -- --include-ignored the_php_drop_in` installs `grpc/grpc` and `google/protobuf` into `target/grpc-php` from Packagist, pinned by `bindings/c/tests/grpc-php/composer.lock`, generates the client with `grpc_php_plugin`, and runs it over a peer in-process against a native PHP server, then over UDP to one in its own process, through every case above.

## Verify

- `new Mud\Peer(...)` throws a `RuntimeException` naming the path it tried and the build command if the library is missing, never a segfault.
- `MUDRPC_LIB` loads only what it names, with no fallback; set it only to a library you built.
- Every `outgoing()` item reaches the transport before `sent()`, which takes the item the last `outgoing()` showed and nothing else; with nothing shown it is a no-op.
- `deliver()` never throws for bad bytes on a UTF-8 topic up to the 4 KB argument buffer: it returns an `Incoming` with `$kind === 'dropped'` and a reason; a wire past the buffer is `Mud\Error` -3 before the library sees it, a topic that is not UTF-8 -1.

The host mistakes the contract names, each with what it throws or returns:

| Mistake | Answer | Kind |
| --- | --- | --- |
| `restore` or `renew` with an epoch at or below the saved one | `Mud\Error`, `getCode() === -6` | error |
| sender 0 sending | `Mud\Error`, `-7`, at the first send | error |
| a key or seed of 32 equal bytes | `Mud\Error`, `-1`; another length is an `InvalidArgumentException` first | error |
| `save()` bytes of another length to `restore` | `Mud\Error`, `-4` | error |
| a hub's `save()` cut to a leaf's length | `Mud\Error`, `-5` | error |
| `hub: true` without a seed | `Mud\Error`, `-1` | error |
| ttl 0, or past a week | sent as 1, or a week | clamp |
| a topic or name over 64 bytes, or a payload over the ceiling | `Mud\Error`, `-3` | error |
| an empty name or topic | `Mud\Error`, `-1` | error |
| a reply to a call this peer does not have | `deliver` returns `dropped === 'no_pending'` | silent |
| 16 calls pending, or 16 items queued undrained | `Mud\Error`, `-2`: decide what not to queue; `pending()` and `queued()` count them | error |
| the 33rd send in one second | `Mud\Error`, `-9`: the next second sends | error |
| a peer after `close()` | `Mud\Error`, `-4`; `pending()`, `queued()`, `pressure()` and `counts()` read 0 | error |

## Test at each rung

- **In-process:** step 2's program, or two peers over a `Mud\Radio` of your own that moves wires between them, as the drop-in's test does.
- **Loopback (lan):** `cargo test -p mudrpc-adapters -- --include-ignored partition_heal_over_loopback` is the reference behaviour over real sockets; step 4 over two `Mud\Udp` on 127.0.0.1 is the PHP form.
- **Relay:** the same on a box on your own LAN, never on the internet.

## Known edges

- The host contract every binding shares (the epoch, save, restore, the nonce, the window, the ttl, threads, bounds, codes, close, the cipher) is the list at the top of `bindings/c/mudrpc.h`.
- `counts()` gives `managed`, `rollbacks`, `floods` and `dropped`, saturating and kept across restores.
- The bytes `save()` gives, and the key you passed in, live in PHP strings nobody can zero; `close()` zeroes the library's copy only.
- Each call into the library copies its arguments once into one 4 KB buffer the binding owns; an argument list past it is `Mud\Error` -3, as the library would say of any of them.
- Payloads, topics and methods are PHP strings, bytes; `$now`, ttl, sender and epoch are unsigned 32-bit integers to the library, so a negative wraps.
- The pump runs on the caller's thread: a product that must do other work while a reply is hours away keeps its own loop and calls `$mud->pump()` from it, then `wait()` once the call is due.
