# Sensor telemetry over UDP in PHP

**Sensor telemetry over UDP in PHP** is the binding's own API, `Mud\Peer` and `Mud\Udp`, with the messages protoc generates from [sensor.proto](../sensor.proto) and no gRPC: a gateway and two sensors, three peers on 127.0.0.1 pumped by one loop, as a device would. Each sensor publishes a reading on a topic the gateway listens to, and with both in hand the gateway calibrates one sensor by a call it answers.

From this folder, with PHP 8.2+ and its FFI extension, composer and protoc installed:

```sh
cargo build --release -p mudrpc-c --manifest-path ../../../Cargo.toml
composer install
mkdir -p gen && protoc -I .. --php_out=gen sensor.proto
MUDRPC_LIB=../../../target/release/libmudrpc.dylib php sensors.php
```

On Linux the library is `libmudrpc.so`. It prints:

```
reading sensor-1 21.50C
reading sensor-2 19.00C
calibrated sensor-2 by 0.50C
```

Part of [gRPC for the Internet of Things](../../../docs/grpc-for-iot.md); the binding is [bindings/php/SKILL.md](../../../bindings/php/SKILL.md).
