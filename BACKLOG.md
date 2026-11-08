# Backlog

The idea, in one breath, so every item is judged against it: a sealed envelope with gRPC's service model inside; a nonce the host's storage makes unique; one lifetime knob; a sans-IO peer the host pumps.

## To do

11. The reference firmware for one board (RP2040 or STM32F4, under $20), no crates: UART as the radio stand-in to a host running `lan`, the block in a flash sector with the epoch written first, provisioning read once over the UART; one SKILL.md, the Rust MCU integrator's front door. Test: partition-heal over the UART shows the simulator's peer lines; a power cut between save and epoch write is `Rollback` at the ground station. Blocked on one board.

16. An HTTP/1.1 transport, for networks and hosts that pass only HTTP: a wire is a request's body between listed peers. Waits on the owner: CLAUDE.md's rule is that nothing listens on the internet, and a cloud host is the internet, so the rule widens or the transport binds private addresses as `lan` does.

19. The seams the demos found, one in each binding: a drop-in server's listen reaches its socket on the pump's next round, so a client in the same process that calls at once can race it, and the tests and demos over UDP wait 200 ms where a channel that put its listens on the socket before returning would need no wait; no binding names a gRPC status, so every demo carries its own table, where one `mud_status_name` in the ABI would serve all; `mud gen`'s Rust and C reply helpers take no detail, so a refusal with one bypasses the stub; the Rust stubs warn under `-D warnings`; a peer does not know its own name. Test: the drop-in tests' and the demos' waits removed, the demos printing their lines through the stubs alone.

Gates that need the owner, each a sprint when its input arrives: the release key and a signed tag, which `./ci.sh release <tag>` verifies against `.github/release-key.asc`; the independent audit, last, on frozen code; the packages published under the owner's accounts, `packages/build.sh`'s comment naming each command; GitBook and the articles, each naming the repository's page as its canonical original; Xcode for the iOS simulator run and the Android SDK for the emulator run; item 16's decision on the internet rule.

What is never built, with the reason already in SPEC.md: a transport, a byte-pipe stream, a second handle for the management plane, padding and cadence in core, a `supersedes` flag, a registry or broker, packages before 3.0.

## Later (not a sprint)

- Demo projects for more use cases, each owning a phrase, as `demos/README.md` maps them.
