# gRPC drone telemetry in Java

**gRPC drone telemetry in Java** is the stub and service base grpc-java generates from [drone.proto](../drone.proto), run unchanged over two mudRPC peers on UDP in one process: a ground station lands, arms, reads three telemetry samples from and lands a mock drone. `Drones.java` is the whole program; building its two `MudChannel`s is the only mudRPC code in it, and the mock drone is the generated `DroneImplBase` added with `addService`.

A product declares the same packages and no grpc-netty, since `MudChannel` is the transport:

```kotlin
implementation("io.grpc:grpc-protobuf:1.85.1")
implementation("io.grpc:grpc-stub:1.85.1")
implementation("com.google.protobuf:protobuf-java:4.36.2")
```

From a clone, at the root, `cargo build --release -p mudrpc-c` builds the library and `cargo test -p mudrpc-c --test abi -- --include-ignored the_java_and_kotlin` fetches those jars, `protoc` and grpc-java's plugin into `target/maven`, each pinned by its SHA-256. Then, in this folder, with a JDK 22 or later on the path (Homebrew's is `/opt/homebrew/opt/openjdk/bin`):

```sh
M=../../../target/maven
CP=$M/grpc-api-1.85.1.jar:$M/grpc-stub-1.85.1.jar:$M/grpc-protobuf-1.85.1.jar:$M/grpc-protobuf-lite-1.85.1.jar:$M/protobuf-java-4.36.2.jar:$M/guava-33.6.0-android.jar:$M/failureaccess-1.0.3.jar
mkdir -p gen && $M/protoc-4.36.2-osx-aarch_64.exe --plugin=protoc-gen-grpc-java=$M/protoc-gen-grpc-java-1.85.1-osx-aarch_64.exe --java_out=gen --grpc-java_out=@generated=omit:gen -I .. drone.proto
javac -d out -cp $CP gen/demo/drones/*.java ../../../bindings/java/Mud.java ../../../bindings/java/MudChannel.java Drones.java
MUDRPC_LIB=$PWD/../../../target/release/libmudrpc.dylib java --enable-native-access=ALL-UNNAMED -cp out:$CP Drones
```

On Linux the two tools end in `-linux-x86_64.exe` and the library is `libmudrpc.so`. `gen/` and `out/` are made by these commands and not kept. Expected output, exit 0, nothing on standard error:

```
land: FAILED_PRECONDITION not armed
armed drone-1 for gcs
telemetry 0:10m 100% 1:20m 98% 2:30m 96%
landed
```

Part of [gRPC for drones](../../../docs/grpc-for-drones.md); the binding is [bindings/java/SKILL.md](../../../bindings/java/SKILL.md).
