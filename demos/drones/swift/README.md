# gRPC drone telemetry in Swift

**gRPC drone telemetry in Swift** is grpc-swift 2's generated client and service for [drone.proto](../drone.proto), running over mudRPC on UDP with the drop-in in [bindings/swift/MudGrpc.swift](../../../bindings/swift/MudGrpc.swift). A ground station and a mock drone, two peers on loopback in one process: the drone refuses to land before it is armed, is armed, streams three telemetry samples and lands.

The binding, the drop-in and the C header are symlinks into the repository; `Sources/Drones/gen/` and `.build/` are generated and not committed. From this folder, with a Rust toolchain and `protoc` installed:

```sh
(cd ../../.. && cargo build --release -p mudrpc-c)
LIB=$(cd ../../../target/release && pwd)
swift build -c release --product protoc-gen-swift
swift build -c release --product protoc-gen-grpc-swift-2
mkdir -p Sources/Drones/gen
protoc --plugin=protoc-gen-swift=.build/release/protoc-gen-swift --plugin=protoc-gen-grpc-swift-2=.build/release/protoc-gen-grpc-swift-2 --swift_out=Sources/Drones/gen --grpc-swift-2_out=Sources/Drones/gen -I .. drone.proto
swift build -Xlinker -L$LIB -Xlinker -rpath -Xlinker $LIB
.build/debug/Drones
```

It prints:

```
land: FAILED_PRECONDITION not armed
armed drone-1 for gcs
telemetry 0:10m 100% 1:20m 98% 2:30m 96%
landed
```

Part of [gRPC for drones](../../../docs/grpc-for-drones.md); the binding is [bindings/swift/SKILL.md](../../../bindings/swift/SKILL.md).
