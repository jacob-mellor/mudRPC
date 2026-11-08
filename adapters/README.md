# Rust, the peer, the simulator and loopback

mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. A Rust product drives `mudrpc-core`'s `Peer` directly; the simulator, `sim`, is where it is tested before anything touches a radio, and the loopback adapter, `lan`, is the next rung, both in the `mudrpc-adapters` crate.

The document is [SKILL.md](SKILL.md): integrate step by step, verify, test at each rung, known edges.
