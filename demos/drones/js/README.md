# gRPC drone telemetry in JavaScript

**gRPC drone telemetry in JavaScript** is the Connect client and service `protoc-gen-es` generates from [drone.proto](../drone.proto), run unchanged over two mudRPC peers on UDP in one Node process: the ground station lands before arming and is refused, arms the drone, streams three telemetry samples and lands. The only mudRPC code is the two transports; `createClient(Drone, transport)` and `transport.service(Drone, impl)` are Connect's.

From this folder, with Node 22 or later, `protoc` and a Rust toolchain:

```sh
(cd ../../.. && cargo build --release -p mudrpc-c --target wasm32-unknown-unknown)
npm ci
mkdir -p vendor gen && cp ../../../bindings/js/mud.js ../../../bindings/js/mud-connect.js vendor/
protoc --plugin=protoc-gen-es=node_modules/.bin/protoc-gen-es --es_out=gen --es_opt=target=js,import_extension=js -I .. drone.proto
MUDRPC_WASM=$PWD/../../../target/wasm32-unknown-unknown/release/mudrpc.wasm node drones.js
```

`vendor/` holds the binding beside the program, as a product vendors it, so `mud-connect.js` finds `@bufbuild/protobuf` and `@connectrpc/connect` in this folder's `node_modules`; `vendor/`, `gen/` and `node_modules/` are made here and not committed. Expected output, exit 0:

```
land: FAILED_PRECONDITION not armed
armed drone-1 for gcs
telemetry 0:10m 100% 1:20m 98% 2:30m 96%
landed
```

Part of [gRPC for drones](../../../docs/grpc-for-drones.md); the binding is [bindings/js/SKILL.md](../../../bindings/js/SKILL.md).
