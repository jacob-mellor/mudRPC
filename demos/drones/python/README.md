# gRPC drone telemetry in Python

**gRPC drone telemetry in Python** is `drones.py`: a ground station and a mock drone, two mudRPC peers over UDP on one machine, running the stub and servicer `grpc_tools.protoc` generates from [drone.proto](../drone.proto) over the drop-in channel. The station lands before arming and is refused, arms, streams three telemetry samples and lands.

From this folder, with a Rust toolchain and Python 3:

```sh
cargo build --release -p mudrpc-c --manifest-path ../../../Cargo.toml
python3 -m venv .venv
.venv/bin/pip install --require-hashes --only-binary=:all: -r requirements.txt
mkdir -p gen
.venv/bin/python -m grpc_tools.protoc -I .. --python_out=gen --grpc_python_out=gen drone.proto
MUDRPC_LIB=$PWD/../../../target/release/libmudrpc.dylib PYTHONPATH=../../../bindings/python:gen .venv/bin/python drones.py
```

On Linux the library is `libmudrpc.so`. It prints:

```
land: FAILED_PRECONDITION not armed
armed drone-1 for gcs
telemetry 0:10m 100% 1:20m 98% 2:30m 96%
landed
```

Part of [gRPC for drones](../../../docs/grpc-for-drones.md); the binding is [bindings/python/SKILL.md](../../../bindings/python/SKILL.md).
