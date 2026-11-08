# Security

mudRPC is a library beside systems whose failure costs lives. Its bar is written in SPEC.md under "The bar", as a list of gates with a test each; what is open is listed honestly in `BACKLOG.md`.

## Reporting

Report a vulnerability privately through GitHub's private vulnerability reporting: the repository's Security tab, "Report a vulnerability". Do not open a public issue for it. You will hear back within 7 days with whether it is in scope, and within 30 days with a fix, a reasoned refusal or a dated plan; a fix ships in the next tagged release with credit unless you ask otherwise. Please include the version (tag or commit), the layer (core, auth, gcm, adapters, a binding), and the smallest input or scenario that shows it; a failing test in the repository's style, or a scenario and its log, is the best report there is (README.md, "Break it").

Reports received: 0 · median first reply: none yet. Kept by hand, and brought up to date at each release.

## If a key is discovered

The operator's page. Calls are named as Python, Java and JavaScript name them; .NET capitalises them (`Stage`, `Counts`), C prefixes them (`mud_stage(mem, len, key)`, `mud_count`), and Rust reaches them through `peer.sealer_mut()`, with `.inner()` before a fleet-key call under `Signed`. `mud` is `target/release/mud`, from `cargo build --release -p mudrpc-gen`.

**What it costs.** A device's saved block or flash image holds its fleet key and, signed, its seed: losing one is losing both.

| Discovered | Unsigned fleet (`Key`, `Gcm`) | Signed fleet (`Signed`) |
| --- | --- | --- |
| the fleet key | the holder reads every wire and forges any message under any number | the holder reads every wire and forges nothing: each seed never leaves its device, so a forgery is `Open(Sig)` and an unknown number `Open(Roster)` |
| device n's seed | (an unsigned fleet has none) | one identity: the holder signs as n; a seed leaks with its device's files, so count the fleet key as discovered too, and the holder reads everything |

**Past traffic is not protected.** Every wire anyone recorded under a discovered fleet key opens with it, now and later. Protecting it would need forward secrecy, which needs an ephemeral key agreement, a handshake, and the design has none by decision (SPEC.md, layer 1, "Threats, stated" and "No handshake, and what stands in for one"). Recovery protects what is sent after it.

**A discovered fleet key.**

1. New key material, on a machine you trust: `mud provision fleet --key` writes `fleet/fleet.key.next`, `key <hex>`, owner-only, beside the fleet's own, and refuses if one is already there.
2. Carry the new key to every unit out of band, as at provisioning, never in a call or event under the old key, which the holder reads; leave out any unit you believe is held. On each, `stage(new_key)`, the 32 bytes of that hex. A staged key opens wires from that moment, so guard it as the fleet key from then.
3. On each unit's flag day, `rotate()`: it seals under the new key and still opens the old one, now `previous`. No epoch changes.
4. Once every unit has rotated, `retire()` on each: the old key opens nothing there after. `opened()` names the slot that opened the last wire product code saw, so a unit still hearing honest wires through `previous` has a sender not yet rotated; retiring earlier knowingly loses those late wires.
5. Move `fleet/fleet.key.next` over `fleet/fleet.key` by hand (`mud provision` never overwrites a file), so devices added after are issued the new key, and take a new `save()` of every unit; destroy older saves and images, which hold the old key.

**A discovered seed, device n of a signed fleet.**

1. On every unit but n, `revoke(n)`, then `save()`: n's wires are `Open(Roster)` there from then, and a unit restored from an older save believes n again. There is no revocation message; it travels as the key does, device by device. Device n itself gets nothing from here on.
2. Rotate the fleet key, steps 1 to 5 above, leaving device n out: until the units retire the old key, the seed's holder reads everything.
3. To fly the device again, run `mud provision fleet 1` beside `fleet/`: a new number m after the roster's highest, `device-<m>` with a new seed and `epoch 1`, and its roster line, `m <public key>`. Never reuse n or its seed.
4. Delete n's line from `fleet/roster`, so no unit booted from it believes n, and empty `fleet/device-<n>` without removing it: the file's existence is what stops `mud provision` issuing n again.
5. Call `roster(m, public_key)`, the 32 bytes of m's line, on every unit that should believe it, then `save()`; wipe the device, its flash and every saved image, and give it the new fleet key, `device-<m>` and the roster.

**The window while it runs.** Each step reaches the units at the mesh's latency, one by one. Until a unit has revoked n, n's signed wires are believed there; until it has rotated, what it sends is read; until it has retired, under an unsigned fleet the holder's forgeries still land there. Stage as late as the mesh's latency allows: a staged key opens wires from `stage`, not from `rotate`. Late wires: one under the old key arriving after its receiver's `retire()` is `Open(Tag)`, lost; one under the new key reaching a unit before its `stage` is `Open(Tag)` and marks nothing; after the `stage` it lands once inside its ttl. On a leaf whose 16 replay windows are full, the new number m runs on the seen table until n's window has been silent a week, and `pressure()` counts it.

**What the counters say meanwhile.** `counts()` gives `managed`, `rollbacks`, `floods` and `dropped`, kept across restores; each verdict, `dropped` with its `status` among them, is what `deliver()` returns.

- `managed` rises by one for each `stage`, `rotate`, `retire`, `roster` and `revoke` you make. A rise nobody ordered is something on the host rekeying the unit.
- `Open(Roster)`, dropped `open` with status 5: a number with no line. After `revoke(n)`, the seed's holder still sending as n; for a new number, its line not yet on that unit.
- `Open(Sig)`, status 4: a believed number with a wrong signature, a key holder forging a number whose seed it lacks.
- `Open(Tag)`, status 2: after `retire()`, a late honest wire or the holder still on the old key.
- `Rollback`, by name, and `rollbacks`: a wire whose epoch went back, a clone or restored image, or a key holder writing an honest number with a forged epoch, which under an unsigned fleet silences that number until it writes a higher one, for good at 2³² − 1, when the device needs a new number from `mud provision`. Judged before the signature, it names the number the wire carried, proven in no fleet.
- `Flood`, and `floods`: a sender over its rate or over its share of a spent second, or the number whose wire came after 64 from senders with no window had landed that second: the fleet past its windows, not a flooder, so under `Key` there is nothing to revoke: size the fleet to the table or sign; `pressure()`: replay protection degraded for want of room, under an unsigned fleet a key holder cycling numbers.

## Scope

In scope: everything under `core`, `auth`, `gcm`, `adapters/lan.rs`, `adapters/udp.rs` and its refusal of a public address in a build without the feature `internet`, the simulator and the six harnesses whose logs are the evidence, the C ABI and the four wrappers over it, as the spec promises them: confidentiality and integrity of the sealed envelope, nonce uniqueness, replay, the rate and budget bounds, key zeroing, the no-panic and no-unsafe guarantees, and any divergence between a skill's host contract and what the code does; features a product may ask for, per-device link keys, the key at rest and supersession among them, are the repository's issue #1, "Suggested features". Out of scope by design, and stated in the spec: anything a fleet-key holder can do to an unsigned fleet, traffic analysis of sizes and timing (padding and cadence are the product's: SPEC.md, Decisions), the transport beneath us, and the product's payload; so are a host clock that steps back (layer 2, "Time is an argument") and the residual costs the multi-node campaign measured and stated, each with its SPEC.md section in BACKLOG.md's Done line "The multi-node campaign, closed".

## Releases

A release is a signed tag (`git tag -s`) on `main`, built on the maintainer's machine by `./ci.sh release <tag>`. It imports the maintainer's public key from `.github/release-key.asc` into a keyring of its own, runs `git verify-tag`, and checks that the signing key's primary fingerprint equals the one pinned in `ci.sh`; it fails on an unsigned tag, on a tag signed by any other key, one added to the file included. The key is `mudRPC release key`, fingerprint `E68A B65A 32F7 C34F 7A86 0AF7 B699 5AF4 7BE0 D3B4`. It then runs the gate and the long runs on the tag's commit, and builds the library (`libmudrpc.a` and `libmudrpc.dylib` on macOS, `libmudrpc.so` on Linux), the wasm (`mudrpc.wasm`) and the header (`mudrpc.h`) twice on the pinned toolchain in `rust-toolchain.toml`, each build in its own empty target directory so every dependency is compiled twice, and fails unless the two are byte-identical. It writes their SHA-256 sums to `SHA256SUMS` beside them in `target/dist/<tag>/out1`, and a buyer checks the set with `shasum -a 256 -c SHA256SUMS` against the sums the maintainer publishes with the signed tag. Zero dependencies is the statement; the three lock files, `Cargo.lock`, `tripwire/Cargo.lock` and `qemu/Cargo.lock`, are tracked and record none, and a release builds `--locked`. CI is `ci.sh` on the maintainer's machine, run before every push on the commit being pushed, nothing on GitHub, and it uses toolchains, never a crate the code links: Node, a JDK, .NET, Swift and protobuf for the harnesses and drop-ins, the pinned targets and `llvm-tools`, QEMU, and for `./ci.sh coverage` one crate tool, `cargo-llvm-cov`, pinned by version, which nothing shipped depends on. A release runs the long counts first (the million-round fuzz, the billion-wire soak, the constant-time measurement and the scrub probes), once, as fixed seeds on unchanged code give the same answer.

## Export

mudRPC uses standard, published cryptography alone and defines none of its own: ChaCha20-Poly1305 (RFC 8439), AES-256-GCM (FIPS 197, SP 800-38D), Ed25519 (RFC 8032) and SHA-512 (FIPS 180-4), each written here from its standard rather than taken from a library. Publicly available source of standard cryptography owes no export notification; a U.S. reader's rule is 15 CFR 742.15(b). This paragraph is the one place the note lives.
