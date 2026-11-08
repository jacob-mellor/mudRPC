# Sensor telemetry over UDP in Rust

**Sensor telemetry over UDP in Rust** is a gateway and two sensors in one process, three peers over UDP on 127.0.0.1 pumped in one loop, with the protobuf messages `mud gen` writes from [sensor.proto](../sensor.proto) at build time. Each sensor publishes one reading on a topic the gateway listens to, and once both have arrived the gateway prints them and calibrates the second by a call it answers.

From this folder:

```sh
cargo run --release --locked
```

prints

```
reading sensor-1 21.50C
reading sensor-2 19.00C
calibrated sensor-2 by 0.50C
```

Part of [gRPC for the Internet of Things](../../../docs/grpc-for-iot.md); the binding is [adapters/SKILL.md](../../../adapters/SKILL.md).
