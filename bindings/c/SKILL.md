---
name: mudrpc-c
description: How to integrate, verify and test the C binding for mudRPC.
---
mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The C ABI, `mudrpc.h`, is the one native surface every other language sits on: you own all memory, and the library never allocates, never keeps your pointers, never calls you back.

## Language and platforms

`mudrpc.h` is strict C89 (ANSI C) and C++17, pedantic with every warning an error, so any C or C++ compiler from 1989 on takes it; the example and the harness below are C11. C++17 has its own header over it, `mud.hpp`, and gRPC C++ over mudRPC: [bindings/cpp/SKILL.md](../cpp/SKILL.md). Where the library is built and run:

| Platform | Built | Run |
| --- | --- | --- |
| macOS arm64 | in CI | every test, every push |
| Linux x86-64 | not yet | not run |
| Cortex-M4 (STM32F4, STM32H7: Pixhawk 6X, Cube Orange+) | in CI, `no_std` | thirteen checks under QEMU, every push |
| RISC-V rv32imc (ESP32-C3, ESP32-C6) | in CI, `no_std` | thirteen checks under QEMU, every push |
| Cortex-M0+ (RP2040), Cortex-M33 (RP2350) | in CI, `no_std`, size checked | not run |
| WebAssembly (browser, Node) | in CI | the JavaScript harness under Node, every push |
| aarch64 Linux (Raspberry Pi, Jetson) | core alone, in CI | not run |
| Windows, Android, iOS | not yet | not yet |

## Integrate

From a clone to a call answered over the simulator, then your transport, your `.proto` and a signed fleet.

**1. Build the library.** A Rust toolchain is the one build dependency, until prebuilt binaries ship at 3.0:

```sh
cargo build --release -p mudrpc-c -p mudrpc-gen
```

That produces `target/release/libmudrpc.a` (and the `.dylib`/`.so` the other bindings load) and `target/release/mud`; copy `bindings/c/mudrpc.h` next to your code.

From 3.0, the vcpkg port and the Conan package `mudrpc` give the same, each built by cargo on your machine: `libmudrpc.a` and `mudrpc.h`, with the C++ headers beside.

**2. Make one call.** The smallest complete program: two peers in one process, a call and a reply moved by hand between them.

```c
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "mudrpc.h"

static mud_buf S(const char* s) { mud_buf b = { (const uint8_t*)s, strlen(s) }; return b; }   /* text only; a payload is bytes with a length */

int main(void) {
    uint8_t key[32];                                       /* the fleet key: 32 bytes from the OS, provisioned to every peer; never a constant and never a passphrase (32 equal bytes are refused, other constants are not) */
    FILE* rnd = fopen("/dev/urandom", "rb"); if (!rnd || fread(key, 1, 32, rnd) != 32) return 1; fclose(rnd);
    uint32_t epoch = 1;                                    /* persisted beside the key; the host raises it on every boot and restore, and writes it first */
    size_t len = mud_size();                               /* every call on a block passes its length after it */
    void* gcs = malloc(len);                               /* malloc is aligned enough; a static buffer needs mud_align() */
    void* drone = malloc(len);
    mud_config c1 = { S("gcs"), { key, 32 }, 1, epoch, { NULL, 0 }, 0, 0, MUD_CIPHER_DEFAULT }, c2 = { S("drone"), { key, 32 }, 2, epoch, { NULL, 0 }, 0, 0, MUD_CIPHER_DEFAULT };   /* roster numbers 1 and 2; 0 cannot send; no seed: an unsigned fleet; no hub: leaves; the default cipher, AES-256-GCM */
    if (mud_new(gcs, len, &c1) != MUD_OK || mud_new(drone, len, &c2) != MUD_OK) return 1;
    if (mud_listen(drone, len, S("nav.Flight")) != MUD_OK) return 1;           /* the drone's transport will subscribe to nav.Flight */

    static const uint8_t req[] = { 8, 1 };                              /* a protobuf request; the stubs encode these for you */
    mud_id id;
    if (mud_call(gcs, len, 0, S("nav.Flight"), S("Arm"), (mud_buf){ req, sizeof req }, 30, &id) != MUD_OK) return 1;   /* now 0, ttl 30 s */

    uint8_t scratch[MUD_MAX_TOPIC + MUD_MSG], plain[MUD_MSG];          /* two buffers, never one */
    void* peers[2] = { gcs, drone };
    for (uint32_t now = 0; now < 3; now++) {                            /* your loop: pump each peer, every tick */
        for (int p = 0; p < 2; p++) {
            mud_out o; int next;
            while ((next = mud_outgoing(peers[p], len, now, (mud_mutbuf){ scratch, sizeof scratch }, &o)) != MUD_NONE) {
                if (next < 0) return 1;
                if (o.kind == MUD_SEND) {                                /* hand o.topic, o.wire, o.ttl_secs to your transport; MUD_LISTEN: subscribe to o.topic */
                    mud_incoming in;
                    int kind = mud_deliver(peers[1 - p], len, now, o.topic, o.wire, (mud_mutbuf){ plain, sizeof plain }, &in);
                    if (kind == MUD_CALL) {
                        printf("drone: %.*s\n", (int)in.method.len, in.method.ptr);
                        if (mud_reply(peers[1 - p], len, now, &in, 0, 0, S(""), S("armed"), 1) != MUD_OK) return 1;
                    } else if (kind == MUD_REPLY && memcmp(in.id.bytes, id.bytes, 12) == 0) {   /* the reply to our call */
                        printf("gcs: status %u %.*s\n", in.status, (int)in.bytes.len, in.bytes.ptr);
                        mud_drop(gcs, len); mud_drop(drone, len);        /* zeroes both blocks, keys included; free them after */
                        return 0;
                    } else if (kind == MUD_DROPPED) {
                        fprintf(stderr, "dropped: reason %d\n", in.dropped);   /* log in development, count in the field */
                    }
                }
                if (mud_sent(peers[p], len) != MUD_OK) return 1;         /* only after the transport took it; if it refused, break instead */
            }
            mud_incoming ex;
            while (mud_tick(peers[p], len, now, &ex) == MUD_EXPIRED) printf("expired\n");
        }
    }
    return 2;
}
```

`cc -std=c11 -Wall -Wextra -Werror example.c libmudrpc.a -o example`, on Linux with `-lpthread -ldl -lm`. Expected output: `drone: Arm` then `gcs: status 0 armed`, exit 0.

**3. The loop.** Every tick, drain `mud_outgoing` into your transport, calling `mud_sent` once the transport took each item (if it refused, `break`: the item is there next tick); feed what it received to `mud_deliver`; call `mud_tick`. `mud_outgoing` also yields `MUD_LISTEN` items, one for the peer's own name (its reply inbox) and one per `mud_listen`, and what a peer hears is what its transport subscribes to. `now` is your uptime in seconds, and every call that takes a block takes `len`, the bytes you gave it, after it. The 12-byte `mud_id` from `mud_call` is the `id` on the reply that answers it, compared with `memcmp`; `MUD_EVENT` is a publish arriving, `MUD_EXPIRED` from `mud_tick` carries the call's id, `seq` and `end` carry streams and `status` is gRPC's status number. `mud_save`, `mud_resume`, `mud_renew` and `mud_drop` keep and end a peer under the host contract at the top of `mudrpc.h`, which every binding shares.

**4. Over the simulator.** Replace the hand-moving with the simulator, one radio per peer in place of your transport: `mud_sim_new(text, &line)` (NULL and the failing line, 0 for a missing header; `mud_sim_free` after). The text is a scenario in the format `adapters/SKILL.md` gives, `"seed 1\npeers 2\nprofile reticulum\n0 partition 0 1\n30000 heal 0 1\n"` cutting the two radios apart until 30 s, so a call at 0 with ttl 60 is answered after the heal and one with ttl 10 expires; `mud_sim_event(s, i, &ev)` hands your harness its host events to perform, `mud_event.kind` numbered in the header, `mud_sim_events` of them. Radio `i` is your `i`th peer, 0 to `mud_sim_peers` − 1, whatever its sender number. Each round, for each peer `i`: hand its outgoing items to `mud_sim_send(s, i, topic, wire, ttl)` or `mud_sim_listen(s, i, topic)` (`MUD_EFULL` while the radio is down, a `peer_down` row, not a partition, whose wire the mesh holds: `break`, no `mud_sent`), `mud_deliver` each message from `mud_sim_poll(s, i, scratch, &topic, &wire)` (1 a message, 2 on-mesh, 3 off-mesh, `MUD_NONE` idle), `mud_tick`; then `mud_sim_step(s, 100)`. `now` is `mud_sim_now_ms(s) / 1000`; `mud_sim_note` writes a host line, `mud_sim_log(s, out)` returns the log's length and fills `out` when it fits; `mud_sim_peers` and `mud_sim_last_event_ms` come from the text.

**5. Your transport.** In the simulator's place your transport takes each send item and listen topic, and what it receives goes to deliver; its rungs are below. On an IP network it can be the library's own: `mud_udp_new(S("10.0.0.2:7000"), S("10.0.0.3:7000 10.0.0.4:7000"), &code)`, then `mud_udp_send` for each send item and `mud_udp_listen` for each listen, and `mud_udp_poll`, which yields only a listed peer's wires on a topic listened to, into `mud_deliver`. It binds and sends to private, loopback or link-local addresses alone; `0.0.0.0` is public, since it binds every interface. A library built with the cargo feature `internet`, and `MUDRPC_INTERNET=listen-on-public-addresses` in the builder's environment, may bind and send to public ones; the feature alone fails the build, since any crate in it could set the feature, and no release is built with it. Cargo shows the build's own warning for the workspace alone, so a socket on a public address says so on standard error and `INTERNET` (`mud_internet()`, `Udp.Internet`) is the check a product makes at start. The variable belongs on the command line of the one build that means it, never in a `.cargo/config.toml`, where every build would go public in silence; `mud_internet()` says which this library is. Reticulum's and DTN's adapters are their own people's, against `adapters/CONFORMANCE.md`.

**6. Stubs from your `.proto`.** `mud gen --lang c nav.proto > nav.h`, or without the binary `mud_gen("c", proto_text, out, &line)` from the library, which returns the header's length in `out` (`MUD_ETOOLARGE` when `out` is short, `MUD_EINVAL` with the line for a bad `.proto`; the imports need no files). It writes a struct per message with `X_encode`/`X_decode` (no allocation, repeated fields bounded by `MUD_REPEATED`), strings and bytes as `mud_view` (`ptr`, `len`), and per service `Flight_TOPIC` (`package.Service`, the string to call on and listen on), `Flight_Upload(mem, len, now, &req, &id)` (a ttl argument unless the method bakes one in), `Flight_Upload_reply(mem, len, now, &in, seq, status, &resp, end)` and `Flight_method`, which gives a `MUD_CALL` on `Flight_TOPIC` its method's index in declaration order. A method with `(mud.ttl_secs)` has its ttl baked in; one with `(mud.topic)` publishes, arrives as `MUD_EVENT` at whoever listens on that topic, and has no reply. Everything is `static inline`, so the header compiles clean under `-Wall -Wextra -Werror` and two headers share one file.

**7. Streams.** Answer one `MUD_CALL` with `mud_reply`, its status gRPC's number (`mud_status_name(status)` gives its name) and its detail the status's UTF-8 text (`S("")` for none), for `seq` 0, 1, 2… below 64 (above it `MUD_ETOOLARGE`), `end` 1 on the last item and on a unary reply, which is seq 0. The items arrive in any order, each a `MUD_REPLY` with its `seq` and `end`, and the call completes once every `seq` up to the lowest `end` it has seen has arrived, or is `MUD_EXPIRED` at its ttl with one missing. Send `end` once: an item above the lowest `end` is `MUD_DROPPED` with `dropped` 7, a redelivered item `dropped` 4, and anything after completion `dropped` 5.

**8. A signed fleet** (SPEC.md layer 1).

```sh
target/release/mud provision fleet 3
```

That writes `fleet/fleet.key` (`key <hex>`, once), `fleet/device-1` to `fleet/device-3` (`sender <n>`, `epoch 1`, `seed <hex>`, owner-only, each device's one secret) and `fleet/roster` (`<sender> <public key hex>` a line), 32-byte values in lowercase hex; run it again to add devices, numbered after the highest, and `--key` in place of the count for a rotation's next key, `fleet/fleet.key.next`; no file is ever overwritten. A device gets the fleet key, its own device file and the roster, and boots its `mud_config` from the files (`key`, `sender`, `epoch` and `seed`, each hex value as 32 bytes), then a `mud_roster` line for every sender it believes; its epoch starts at the file's and is raised under the host contract. Fill `seed` with this sender's 32-byte Ed25519 seed and `budget` with the verifications per second this host affords (at least 1; a wire past them is `MUD_DROPPED`, `dropped` 2, `status` 6), give every sender a line with `mud_roster(mem, len, sender, key, next)` using `mud_public_key` for the keys, and remove one with `mud_revoke`. A peer believes `MUD_ROSTER` senders and the line past them is `MUD_EFULL`, so a ground station that must believe a fleet sets `hub` to 1 in a block of `mud_hub_size()` bytes: `MUD_HUB_ROSTER` lines, 256 replay windows and 1024 remembered messages. Its wire is version 4 (2 under ChaCha20-Poly1305) and 64 bytes longer: an unsigned peer gives it `MUD_DROPPED` with `dropped` 1 (version) and `status` 4, and a signed peer an unsigned wire the same with `status` 3, so one fleet key never serves both. A discovered key or seed: SECURITY.md, "If a key is discovered".

`cipher` `MUD_CIPHER_DEFAULT` is AES-256-GCM, and a C config names it, since a zeroed one is ChaCha20-Poly1305; `MUD_CIPHER_CHACHA20_POLY1305` builds any of these peers under ChaCha20-Poly1305 instead: the same sizes and calls, wire version 1, or 2 signed, so a peer of the other cipher drops its wires with `dropped` 1 and that version in `status`.

**9. Verify.** `cargo test -p mudrpc-c --test abi -- --include-ignored the_skill_example_compiles_and_runs every_host_mistake` runs step 2's program and every row of the table below.

## Verify

- `mud_new` returns `MUD_OK`, and every later call on the block passes the same length.
- Every `mud_outgoing` item reaches the transport before `mud_sent`, which takes the item the last `mud_outgoing` showed and nothing else; with nothing shown it is a no-op.
- Every return code is checked; `MUD_EFULL` and `MUD_ERATE` are the product's to handle, not faults.
- `in.topic`, `in.method`, `in.reply_to`, `in.bytes` are used before `scratch` or `plain` is reused; copy them out if you need them longer.
- `scratch` for `mud_outgoing` holds `MUD_MAX_TOPIC + MUD_MSG` bytes and `plain` for `mud_deliver` holds `MUD_MSG`; smaller is `MUD_EINVAL`, never a silent stall.
- Dropped verdicts are logged in development builds and counted in the field.
- One thread drives a block: a second caller finds its busy byte held and gets `MUD_EBUSY` with nothing done, as does every call after an entry a crash or a panic ended, until a `mud_resume` at a higher epoch clears it; a detector of the mistake, not a lock.

The host mistakes the contract names, each with what it answers; `tests/wrong.c` commits every row in order and prints what it got, and `cargo test -p mudrpc-c --test abi -- --include-ignored every_host_mistake` holds its lines to this table:

| Mistake | Answer | Kind |
| --- | --- | --- |
| `mud_resume` or `mud_renew` with an epoch at or below the saved one | `MUD_EEPOCH` | error |
| a send from sender 0 | `MUD_EUNPROVISIONED` | error |
| a key, staged key or seed of 32 equal bytes | `MUD_EINVAL` | error |
| a block shorter than its shape, `mud_size()` or `mud_hub_size()`, or a hub's at a leaf's length | `MUD_EMEM` | error |
| a block longer than its shape, a leaf's in a hub's length included | accepted, its shape's bytes used | silent |
| `hub` 1 without a seed | `MUD_EINVAL` | error |
| a saved copy sent from without `mud_resume` | `MUD_ECORRUPT` | error |
| a block retagged to the other cipher | `MUD_ECORRUPT` from `mud_resume` | error |
| ttl 0, or past a week | 1, or a week | clamp |
| a topic or name over `MUD_MAX_TOPIC`, or a payload over the ceiling | `MUD_ETOOLARGE` | error |
| an empty name, service or topic | `MUD_EINVAL` | error |
| a reply to a call this peer does not have | `MUD_DROPPED`, `dropped` 5 | silent |
| 16 calls pending, or 16 items queued undrained | `MUD_EFULL`: decide what not to queue; `mud_pending` and `mud_queued` count them | error |
| the 33rd send in one second | `MUD_ERATE`: the next second sends | error |

## Test at each rung

- **In-process:** step 2's program, or step 4's simulator. `tests/harness.c` runs every standard scenario in `scenarios/` and the campaign's fleet-of-256 and reflash-within-the-week, to the simulator's 1 024 peers, unsigned, `--signed`, `--gcm` and both, and its logs must equal `scenarios/<name>.log` (`<name>.gcm.log` where one is checked in) and the Rust harness's signed runs byte for byte. Run it on your product's loop the same way.
- **Loopback (lan):** `cargo test -p mudrpc-adapters -- --include-ignored partition_heal_over_loopback` shows the reference behaviour over real sockets; your product over UDP on 127.0.0.1 should show the same peer events.
- **Relay:** the same binary on a box on your own LAN, never on the internet.

## Known edges

- The host contract every binding shares (the epoch, save, restore, the nonce, the window, the ttl, threads, bounds, codes, drop, the cipher) is the list at the top of `mudrpc.h`.
- A block's `len` under 4 or under the shape its tag names (`mud_size()`, or `mud_hub_size()` for a hub) is `MUD_EMEM`, the tag the one thing read, so a wrong length never reads past your buffer. `mud_save(mem, len, out, out_len)` writes into memory of its shape's size aligned to `mud_align()` that shares no byte with the block (an overlap is `MUD_EINVAL`). Restore by copying back into memory aligned to `mud_align()` and calling `mud_resume(mem, len, epoch)` with the saved length: a short length is `MUD_EMEM`, a block from another build `MUD_ECORRUPT`, an epoch that does not climb `MUD_EEPOCH`, and a block restored without `mud_resume` answers `MUD_ECORRUPT` instead of reusing a nonce.
- `mud_count(mem, len, which)` gives four saturating counters the block keeps across restores, numbered in the header.
- `MUD_EPANIC` is what a panic inside the library becomes, and only while the library unwinds: the default build does. A firmware links `libmudrpc.a` from `cargo build --release -p mudrpc-c --no-default-features --target <t>`: `no_std`, the peer entries without the simulator and `mud_gen`, and no panic to catch, since the no-panic tripwire links those entries and proves none is reachable; were one, it would call your C library's `abort`.
- What each shape costs a board, its block, scratch and stack and the library's flash per target, is the table in SPEC.md layer 5.
- The ABI instantiates one Peer size: 512-byte wires, 16 calls in flight, 64 remembered messages, 16 queued sends. If that does not fit a device, widen the principle in core (a second instantiation), do not patch here.
- Strings are UTF-8 with a length, never NUL-terminated. A non-UTF-8 topic or method is `MUD_EINVAL`, as is an empty topic; an empty method is accepted.
