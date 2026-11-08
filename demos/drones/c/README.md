# gRPC drone telemetry in C

**gRPC drone telemetry in C**: a ground station and a mock drone, two peers over UDP on 127.0.0.1 in one process, calling through the stubs `mud gen` writes from [drone.proto](../drone.proto) over the C ABI. The drone refuses to land before it is armed, is armed, streams three samples and lands.

From this folder, with the library and the generator built (`cargo build --release -p mudrpc-c -p mudrpc-gen` at the repository root):

```sh
../../../target/release/mud gen ../drone.proto --lang c -o gen
cc -std=c11 -Wall -Wextra -Wpedantic -Werror -I ../../../bindings/c -I gen drones.c ../../../target/release/libmudrpc.a -o gen/drones
./gen/drones
```

On Linux add `-lpthread -ldl -lm` to the compile line. Expected output, exit 0:

```
land: FAILED_PRECONDITION not armed
armed drone-1 for gcs
telemetry 0:10m 100% 1:20m 98% 2:30m 96%
landed
```

Part of [gRPC for drones](../../../docs/grpc-for-drones.md); the binding is [bindings/c/SKILL.md](../../../bindings/c/SKILL.md).
