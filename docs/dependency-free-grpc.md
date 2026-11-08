# Dependency-free gRPC

**Dependency-free gRPC** is the library under mudRPC: a `no_std` Rust core with no crates, no allocation and no unsafe code, under one C ABI, with one file per language over it, so a product adds nothing but that file and its own gRPC API package.

gRPC's own implementation is large by nature: an HTTP/2 stack, TLS, a connection manager, load balancing and name resolution, and in most languages a native library beside the API. mudRPC has none of those to carry, so its core is four modules, the envelope, the codec, the sealer and the peer, auditable in a day; the two sealers beside it, AES-256-GCM from FIPS 197 and SP 800-38D and Ed25519 from RFC 8032 with SHA-512 from FIPS 180-4, are written from the primary sources, with no cryptography crate between them and the standard. Time and randomness are inputs: core never reads a clock or an RNG, which is what lets it run the same on a flight controller, a phone and a server, and lets every test be exact.

The supply chain is the point. A product that ships this ships what it can read: `cargo tree` shows one tree with nothing in it but the workspace's own crates, and the gate fails if a crate appears. The C ABI, `mudrpc.h`, is the one native surface; its top comment is the host contract every language shares. Each binding is one vendored source file, Python's over `ctypes`, .NET's over P/Invoke, Java's over the foreign function API, Go's over cgo, JavaScript's over WebAssembly, Swift's, PHP's over FFI, C++'s a header; copying the file is the integration, and the packages at 3.0 carry the same file with the built library.

A gRPC drop-in adds its language's gRPC API package and nothing of gRPC's transport, said exactly, language by language, in [From gRPC](../README.md#from-grpc). The library is also small in the field: the gate measures core, the sealers and the ABI as a Cortex-M firmware links them and fails on growth, and runs them on two boards under QEMU; the numbers are in [ci.sh](../ci.sh) and [SPEC.md](../SPEC.md).

This page belongs to [gRPC without HTTP/2](grpc-without-http2.md); the [home page](../README.md) has the five-minute start.
