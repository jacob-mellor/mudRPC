# mudRPC — how we work

SPEC.md says what we build and why. `BACKLOG.md` says what is being built now. This file says how. Nothing else lives here.

## Rules

- **No crates, anywhere.** `no_std` and no allocation in core. One exception, the owner's on 2026-10-25: a binding's drop-in for gRPC depends on its language's gRPC API package, so Google's generated code runs over mudRPC unchanged. Recode from the primary source (RFC 8439, RFC 9171, protobuf encoding and proto3 specs) or from open code with credit at the top of the file, fewest lines.
- **Core first.** Trimmings, documentation, skills and wrappers come last; done early they take all the time. One language at a time: Rust to Rust until core is right.
- **Nothing listens on the internet unless built to.** `lan` refuses non-private addresses always, tested. A product transport binds or sends to a public address only in a build with two keys, the cargo feature `internet` and the builder's environment `MUDRPC_INTERNET=listen-on-public-addresses`, the feature alone failing the build, since any crate in a build can set a feature; no release artefact carries it, and a socket on a public address says so where no build hides it: without it the code that would do so is not compiled, so no setting, dependency, caller or write to memory can open a public port, and the default build's refusal is a test. The owner's decision, 2026-10-25; widening it further is the owner's alone.
- **Edge cases widen the principle.** Never a workaround.
- **One file, one skill.** Every adapter and wrapper is one vendored source file beside one `SKILL.md`, written so a human or an LLM with no other context can integrate from it alone; the kit is the skill, the wrapper file and the built library with its header, `bindings/c/mudrpc.h`, whose top comment is the host contract, stated once for every language. A package on a language's own manager carries that same file and the built library, built by `./ci.sh release` from the signed tag; vendoring the file is the path for those who want full control.
- **Core is four modules.** If it grows, ask why before adding one.
- **No sprawl.** Not in code, docs or comments. Comments are a short line or absent. No `.bak`, no generated docs, no second document about a thing that has one.
- **The review question for every line:** does it have value, can it be simpler, can it be more readable.
- **Time and randomness are inputs.** Core never reads a clock or an RNG.
- **Short.** Short sprints, short answers, minimum lines. Going round in circles is the failure mode this repo is built to prevent.

## The loop

Each sprint takes one feature off To do in `BACKLOG.md`:

1. Design its tests, for robustness, not minimality. The tests in SPEC.md are the floor.
2. Make them green with the fewest lines, then apply the review question to each line.
3. Harden: fuzz it, break it, try the hostile scenario against it.
4. Write the one-line lesson in the commit message. Pick the next item. Do not predict the sprints after it.

## Done means

- A feature: its tests green, fuzzed where it decodes, each line reviewed, one lesson line in `BACKLOG.md`.
- An adapter or wrapper: its example passes the four scenarios on every rung we can run with an event log byte-identical to the Rust example's, and its skill passes the skill test (a fresh session, the `SKILL.md` and a `.proto`, nothing else).

## Testing

Two layers, two files per subject, never mixed.

- **Unit, every build.** `cargo test` at the root: core unit tests, RFC vectors, the seeded fuzz, the simulator's parser and conformance set, the Peer tests. Seconds. No test needs a radio, a network or a second machine.
- **End to end, at PR and release level.** `cargo test -- --include-ignored` adds the standard scenarios on the simulator with their checked-in logs, the offline and relay runs, and partition-heal over loopback sockets. Each is marked `#[ignore]` with that command in its reason. A checked-in log is regenerated with the `scenario` example and the diff is reviewed, never accepted blind.

CI is `ci.sh`, run here before every push on the commit being pushed, never on GitHub: both layers, then core cross-built for the targets in SPEC.md with its size and branches checked and the two boards under QEMU; `./ci.sh release <tag>` adds the long counts and builds only after they and the gate are green. Bench and relay rungs run the same scenario files through `lan` on the reference rig in the `sim` skill.
