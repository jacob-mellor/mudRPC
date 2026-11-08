# The core: four modules

`codec.rs`, `envelope.rs`, `seal.rs` and `rpc.rs`, the peer, the wire and the cipher seam: `no_std`, no allocation, no unsafe, no crates; time and randomness are inputs. Its unit tests, the seeded fuzz and the property test are in `tests/`. What it builds and why, with every claim's test named, is [SPEC.md](../SPEC.md).
