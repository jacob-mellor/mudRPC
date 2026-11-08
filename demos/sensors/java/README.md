# Sensor telemetry over UDP in Java

**Sensor telemetry over UDP in Java** is three mudRPC peers in one loop, a gateway and two sensors on UDP in one process, speaking the messages protobuf-java generates from [sensor.proto](../sensor.proto) through the binding's own API, with nothing of gRPC's: as a device would. Each sensor publishes a reading on a topic the gateway listens to, and the gateway calibrates one sensor by a call it answers.

A product declares protobuf-java alone, `implementation("com.google.protobuf:protobuf-java:4.36.2")`. From a clone, at the root, `cargo build --release -p mudrpc-c` builds the library and `cargo test -p mudrpc-c --test abi -- --include-ignored the_java_and_kotlin` fetches that jar and `protoc` into `target/maven`, each pinned by its SHA-256. Then, in this folder, with a JDK 22 or later on the path (Homebrew's is `/opt/homebrew/opt/openjdk/bin`):

```sh
M=../../../target/maven
CP=$M/protobuf-java-4.36.2.jar
mkdir -p gen && $M/protoc-4.36.2-osx-aarch_64.exe --java_out=gen -I .. sensor.proto
javac -d out -cp $CP gen/demo/sensors/*.java ../../../bindings/java/Mud.java Sensors.java
MUDRPC_LIB=$PWD/../../../target/release/libmudrpc.dylib java --enable-native-access=ALL-UNNAMED -cp out:$CP Sensors
```

On Linux `protoc` ends in `-linux-x86_64.exe` and the library is `libmudrpc.so`. `gen/` and `out/` are made by these commands and not kept. Expected output, exit 0, nothing on standard error:

```
reading sensor-1 21.50C
reading sensor-2 19.00C
calibrated sensor-2 by 0.50C
```

Part of [gRPC for the Internet of Things](../../../docs/grpc-for-iot.md); the binding is [bindings/java/SKILL.md](../../../bindings/java/SKILL.md).
