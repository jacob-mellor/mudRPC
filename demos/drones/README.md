# gRPC drone telemetry

**gRPC drone telemetry** over mudRPC, the same program in every language: a ground station and a mock drone, two peers over UDP on one machine; the drone is armed, three telemetry samples stream back, a landing is refused before arming and granted after. The service is [drone.proto](drone.proto), and every program prints these four lines, which the test holds it to:

```
land: FAILED_PRECONDITION not armed
armed drone-1 for gcs
telemetry 0:10m 100% 1:20m 98% 2:30m 96%
landed
```

One folder per language, each with its own front door and commands: [rust/](rust/), [c/](c/), [cpp/](cpp/), [python/](python/), [dotnet/](dotnet/), [java/](java/), [js/](js/), [swift/](swift/), [go/](go/), [php/](php/). Part of [gRPC for drones](../../docs/grpc-for-drones.md).
