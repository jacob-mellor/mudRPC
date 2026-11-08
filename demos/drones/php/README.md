# gRPC drone telemetry in PHP

**gRPC drone telemetry in PHP** is the client `grpc_php_plugin` generates from [drone.proto](../drone.proto), written as gRPC PHP code is, running over mudRPC with no HTTP/2 and no connection to keep. A ground station lands, arms, streams three telemetry samples and lands again against a mock drone in the same process, which answers with the binding's own `Mud\Peer::reply`.

From this folder, with PHP 8.2+ and its FFI extension, composer, protoc and `grpc_php_plugin` installed:

```sh
cargo build --release -p mudrpc-c --manifest-path ../../../Cargo.toml
composer install
mkdir -p gen && protoc -I .. --php_out=gen --grpc_out=gen --plugin=protoc-gen-grpc="$(which grpc_php_plugin)" drone.proto
MUDRPC_LIB=../../../target/release/libmudrpc.dylib php drones.php
```

On Linux the library is `libmudrpc.so`. It prints:

```
land: FAILED_PRECONDITION not armed
armed drone-1 for gcs
telemetry 0:10m 100% 1:20m 98% 2:30m 96%
landed
```

Part of [gRPC for drones](../../../docs/grpc-for-drones.md); the binding is [bindings/php/SKILL.md](../../../bindings/php/SKILL.md).
