# Sensor telemetry over UDP

**Sensor telemetry over UDP** with mudRPC's own API, the same program in every language: a gateway and two sensors, three peers over UDP in one loop; the sensors publish readings the gateway listens to, and the gateway calibrates one by a call, as a device with no gRPC stack would. The messages are [sensor.proto](sensor.proto), and every program prints these three lines, which the test holds it to:

```
reading sensor-1 21.50C
reading sensor-2 19.00C
calibrated sensor-2 by 0.50C
```

One folder per language, each with its own front door and commands: [rust/](rust/), [c/](c/), [cpp/](cpp/), [python/](python/), [dotnet/](dotnet/), [java/](java/), [js/](js/), [swift/](swift/), [go/](go/), [php/](php/). Part of [gRPC for the Internet of Things](../../docs/grpc-for-iot.md).
