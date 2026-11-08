# Sensor telemetry over UDP in .NET

**Sensor telemetry over UDP in .NET**: two sensors and a gateway, three mudRPC peers over UDP on one machine, pumped by one loop through the binding's own API with the messages Grpc.Tools generates from [sensor.proto](../sensor.proto). Each sensor publishes a reading on a topic the gateway listens to, and once both have arrived the gateway calibrates sensor-2 with a call it answers.

Build the library once at the repository root with `cargo build --release -p mudrpc-c`, then from this folder:

```sh
MUDRPC_LIB=$(cd ../../.. && pwd)/target/release/libmudrpc.dylib dotnet run -c Release -nologo -v q
```

Expected output, exit 0, nothing on standard error:

```
reading sensor-1 21.50C
reading sensor-2 19.00C
calibrated sensor-2 by 0.50C
```

Part of [gRPC for the Internet of Things](../../../docs/grpc-for-iot.md); the binding is [bindings/dotnet/SKILL.md](../../../bindings/dotnet/SKILL.md).
