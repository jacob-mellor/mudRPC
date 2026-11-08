# gRPC drone telemetry in Rust

**gRPC drone telemetry in Rust** is a ground station and a mock drone in one process, two peers over UDP on 127.0.0.1, calling through the stubs `mud gen` writes from [drone.proto](../drone.proto) at build time. The station is refused a landing before arming, arms the drone, streams three telemetry samples back and lands it, each call with a ttl of ten seconds; Rust drives the peer directly, having no gRPC drop-in.

From this folder:

```sh
cargo run --release --locked
```

prints

```
land: FAILED_PRECONDITION not armed
armed drone-1 for gcs
telemetry 0:10m 100% 1:20m 98% 2:30m 96%
landed
```

Part of [gRPC for drones](../../../docs/grpc-for-drones.md); the binding is [adapters/SKILL.md](../../../adapters/SKILL.md).
