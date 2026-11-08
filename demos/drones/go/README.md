# gRPC drone telemetry in Go

**gRPC drone telemetry in Go**: a ground station and a mock drone, two mudRPC peers over UDP on one machine, the client and server protoc-gen-go-grpc generates from [drone.proto](../drone.proto) run unchanged over the drop-in `mud.NewChannel`. The station is refused a landing before arming, arms the drone, streams three telemetry samples back and lands it.

From this folder, with the library built by `cargo build --release -p mudrpc-c` at the repository root:

```sh
export GOTOOLCHAIN=local
go build -o bin/ google.golang.org/grpc/cmd/protoc-gen-go-grpc google.golang.org/protobuf/cmd/protoc-gen-go
mkdir -p pb
protoc --plugin=protoc-gen-go=bin/protoc-gen-go --plugin=protoc-gen-go-grpc=bin/protoc-gen-go-grpc \
  --go_out=pb --go_opt=paths=source_relative '--go_opt=Mdrone.proto=mudrpc.demo/drones/pb;pb' \
  --go-grpc_out=pb --go-grpc_opt=paths=source_relative '--go-grpc_opt=Mdrone.proto=mudrpc.demo/drones/pb;pb' \
  -I .. drone.proto
LIB=$PWD/../../../target/release
CGO_LDFLAGS="-L$LIB" go run -ldflags="-extldflags=-Wl,-rpath,$LIB" .
```

Expected output, exit 0, nothing on stderr:

```
land: FAILED_PRECONDITION not armed
armed drone-1 for gcs
telemetry 0:10m 100% 1:20m 98% 2:30m 96%
landed
```

Part of [gRPC for drones](../../../docs/grpc-for-drones.md); the binding is [bindings/go/SKILL.md](../../../bindings/go/SKILL.md).
