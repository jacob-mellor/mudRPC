# Sensor telemetry over UDP in Go

**Sensor telemetry over UDP in Go**: a gateway and two sensors, three mudRPC peers over UDP in one loop, with the binding's own API and the protobuf messages protoc-gen-go generates from [sensor.proto](../sensor.proto), as a device would use them. The sensors publish a reading each on a topic the gateway listens to, and the gateway calibrates one of them by a call.

From this folder, with the library built by `cargo build --release -p mudrpc-c` at the repository root:

```sh
export GOTOOLCHAIN=local
go build -o bin/ google.golang.org/protobuf/cmd/protoc-gen-go
mkdir -p pb
protoc --plugin=protoc-gen-go=bin/protoc-gen-go --go_out=pb --go_opt=paths=source_relative \
  '--go_opt=Msensor.proto=mudrpc.demo/sensors/pb;pb' -I .. sensor.proto
LIB=$PWD/../../../target/release
CGO_LDFLAGS="-L$LIB" go run -ldflags="-extldflags=-Wl,-rpath,$LIB" .
```

Expected output, exit 0, nothing on stderr:

```
reading sensor-1 21.50C
reading sensor-2 19.00C
calibrated sensor-2 by 0.50C
```

Part of [gRPC for the Internet of Things](../../../docs/grpc-for-iot.md); the binding is [bindings/go/SKILL.md](../../../bindings/go/SKILL.md).
