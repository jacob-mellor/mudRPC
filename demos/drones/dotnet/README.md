# gRPC drone telemetry in .NET

**gRPC drone telemetry in .NET**: a ground station and a mock drone, two mudRPC peers over UDP on one machine, driven by the client and service base Grpc.Tools generates from [drone.proto](../drone.proto). The station lands before arming and is refused, arms the drone, streams three telemetry samples and lands it; `MudChannel` replaces the HTTP/2 channel and nothing else changes.

Build the library once at the repository root with `cargo build --release -p mudrpc-c`, then from this folder:

```sh
MUDRPC_LIB=$(cd ../../.. && pwd)/target/release/libmudrpc.dylib dotnet run -c Release -nologo -v q
```

Expected output, exit 0, nothing on standard error:

```
land: FAILED_PRECONDITION not armed
armed drone-1 for gcs
telemetry 0:10m 100% 1:20m 98% 2:30m 96%
landed
```

Part of [gRPC for drones](../../../docs/grpc-for-drones.md); the binding is [bindings/dotnet/SKILL.md](../../../bindings/dotnet/SKILL.md).
