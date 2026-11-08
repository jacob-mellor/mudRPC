# gRPC drone telemetry in C++

**gRPC drone telemetry in C++**: a ground station and a mock drone, two peers over UDP on one machine, in one process. The drone serves [drone.proto](../drone.proto) through the `Stub` and `Service` that `protoc-gen-mud` writes, the gRPC C++ sync API with `mud::` for `grpc::`; the station lands, arms, streams three samples and lands again.

Needs a C++17 compiler, protobuf with `protoc` and `libprotoc` (found by `pkg-config`), and the library built once at the root with `cargo build --release -p mudrpc-c`. Then, from this folder:

```sh
c++ -std=c++17 ../../../bindings/cpp/protoc-gen-mud.cc $(pkg-config --cflags --libs protobuf) -lprotoc -o protoc-gen-mud
mkdir -p gen && protoc --plugin=protoc-gen-mud=./protoc-gen-mud -I.. --cpp_out=gen --mud_out=gen drone.proto
c++ -std=c++17 -Wall -Wextra -Werror -Igen -I../../../bindings/cpp -I../../../bindings/c drones.cc gen/drone.pb.cc $(pkg-config --cflags --libs protobuf) -L../../../target/release -lmudrpc -Wl,-rpath,../../../target/release -o drones
./drones
```

To link statically, replace `-lmudrpc -Wl,-rpath,../../../target/release` with `../../../target/release/libmudrpc.a`. It prints:

```
land: FAILED_PRECONDITION not armed
armed drone-1 for gcs
telemetry 0:10m 100% 1:20m 98% 2:30m 96%
landed
```

Part of [gRPC for drones](../../../docs/grpc-for-drones.md); the binding is [bindings/cpp/SKILL.md](../../../bindings/cpp/SKILL.md).
