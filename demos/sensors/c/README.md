# Sensor telemetry over UDP in C

**Sensor telemetry over UDP in C**: a gateway and two sensors, three peers over UDP on 127.0.0.1 in one loop, with the messages `mud gen` writes from [sensor.proto](../sensor.proto) and the C ABI's own calls. The sensors publish a reading each to a topic the gateway listens on, and the gateway calibrates one of them by a call.

From this folder, with the library and the generator built (`cargo build --release -p mudrpc-c -p mudrpc-gen` at the repository root):

```sh
../../../target/release/mud gen ../sensor.proto --lang c -o gen
cc -std=c11 -Wall -Wextra -Wpedantic -Werror -I ../../../bindings/c -I gen sensors.c ../../../target/release/libmudrpc.a -o gen/sensors
./gen/sensors
```

On Linux add `-lpthread -ldl -lm` to the compile line. Expected output, exit 0:

```
reading sensor-1 21.50C
reading sensor-2 19.00C
calibrated sensor-2 by 0.50C
```

Part of [gRPC for the Internet of Things](../../../docs/grpc-for-iot.md); the binding is [bindings/c/SKILL.md](../../../bindings/c/SKILL.md).
