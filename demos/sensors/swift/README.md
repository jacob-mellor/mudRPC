# Sensor telemetry over UDP in Swift

**Sensor telemetry over UDP in Swift** is the binding's own API, [bindings/swift/Mud.swift](../../../bindings/swift/Mud.swift), carrying the SwiftProtobuf messages protoc-gen-swift generates from [sensor.proto](../sensor.proto), with no gRPC. A gateway and two sensors, three peers on loopback pumped by one loop in one process: each sensor publishes a reading on a topic the gateway listens to, and the gateway calibrates one sensor by a call it answers.

The binding and the C header are symlinks into the repository; `Sources/Sensors/gen/` and `.build/` are generated and not committed. From this folder, with a Rust toolchain and `protoc` installed:

```sh
(cd ../../.. && cargo build --release -p mudrpc-c)
LIB=$(cd ../../../target/release && pwd)
swift build -c release --product protoc-gen-swift
mkdir -p Sources/Sensors/gen
protoc --plugin=protoc-gen-swift=.build/release/protoc-gen-swift --swift_out=Sources/Sensors/gen -I .. sensor.proto
swift build -Xlinker -L$LIB -Xlinker -rpath -Xlinker $LIB
.build/debug/Sensors
```

It prints:

```
reading sensor-1 21.50C
reading sensor-2 19.00C
calibrated sensor-2 by 0.50C
```

Part of [gRPC for the Internet of Things](../../../docs/grpc-for-iot.md); the binding is [bindings/swift/SKILL.md](../../../bindings/swift/SKILL.md).
