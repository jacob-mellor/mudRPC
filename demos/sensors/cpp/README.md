# Sensor telemetry over UDP in C++

**Sensor telemetry over UDP in C++**: a gateway and two sensors, three peers over UDP on one machine, pumped by one loop in one process. Each sensor publishes a [sensor.proto](../sensor.proto) `Reading` on a topic the gateway listens to, and the gateway calibrates one sensor by a call it answers, on the binding's own API with the messages protoc generates and nothing of gRPC.

Needs a C++17 compiler, protobuf with `protoc` (found by `pkg-config`), and the library built once at the root with `cargo build --release -p mudrpc-c`. Then, from this folder:

```sh
mkdir -p gen && protoc -I.. --cpp_out=gen sensor.proto
c++ -std=c++17 -Wall -Wextra -Werror -Igen -I../../../bindings/cpp -I../../../bindings/c sensors.cc gen/sensor.pb.cc $(pkg-config --cflags --libs protobuf) -L../../../target/release -lmudrpc -Wl,-rpath,../../../target/release -o sensors
./sensors
```

To link statically, replace `-lmudrpc -Wl,-rpath,../../../target/release` with `../../../target/release/libmudrpc.a`. It prints:

```
reading sensor-1 21.50C
reading sensor-2 19.00C
calibrated sensor-2 by 0.50C
```

Part of [gRPC for the Internet of Things](../../../docs/grpc-for-iot.md); the binding is [bindings/cpp/SKILL.md](../../../bindings/cpp/SKILL.md).
