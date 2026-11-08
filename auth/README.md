# Per-sender authentication

SHA-512 from FIPS 180-4 and Ed25519 from RFC 8032, and the `Signed` sealer behind core's one seam, under core's rules: `no_std`, no allocation, no crates, no unsafe. The constant-time measurement, `tests/ct.rs`, and the RFC vectors are here; what a signed fleet proves and costs is SPEC.md's Signed and Key paragraphs.
