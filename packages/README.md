# Mud RPC — gRPC for unstable networks

gRPC's service model, services from a `.proto` with calls, replies, streams, deadlines and status codes, carried as sealed, self-contained packets over mesh networks, radios, UDP and delay-tolerant links: gRPC without HTTP/2 and with no server stack, for the Internet of Things, drones, field robots and anything that goes out of range. A `no_std` Rust core with no dependencies under one C ABI, one file in your language over it, and a drop-in for your language's gRPC client and server, so the code `protoc` generates runs unchanged.

Start with `SKILL.md` in this package: one call step by step, the gRPC drop-in, and what each host mistake answers; copy it to your project's `.claude/skills/mudrpc/SKILL.md` for an agent to read. Source, specification and the other languages: the repository `jacob-mellor/mudRPC` on GitHub, which the package's metadata links.
