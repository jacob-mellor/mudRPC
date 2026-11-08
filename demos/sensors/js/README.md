# Sensor telemetry over UDP in JavaScript

**Sensor telemetry over UDP in JavaScript** is a gateway and two sensors, three mudRPC peers on UDP driven by one loop in one Node process: each sensor publishes a `Reading` on the topic `sensors.Readings`, and the gateway, once both have arrived, prints them and calibrates `sensor-2` by a `Calibrate` call it answers with an `Ack`. The messages are the descriptors `protoc-gen-es` generates from [sensor.proto](../sensor.proto); the calls are the binding's own API, as a device without gRPC would make them.

From this folder, with Node 22 or later, `protoc` and a Rust toolchain:

```sh
(cd ../../.. && cargo build --release -p mudrpc-c --target wasm32-unknown-unknown)
npm ci
mkdir -p gen && protoc --plugin=protoc-gen-es=node_modules/.bin/protoc-gen-es --es_out=gen --es_opt=target=js,import_extension=js -I .. sensor.proto
MUDRPC_WASM=$PWD/../../../target/wasm32-unknown-unknown/release/mudrpc.wasm node sensors.js
```

`gen/` and `node_modules/` are made here and not committed. Expected output, exit 0:

```
reading sensor-1 21.50C
reading sensor-2 19.00C
calibrated sensor-2 by 0.50C
```

Part of [gRPC for the Internet of Things](../../../docs/grpc-for-iot.md); the binding is [bindings/js/SKILL.md](../../../bindings/js/SKILL.md).
