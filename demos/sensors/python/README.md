# Sensor telemetry over UDP in Python

**Sensor telemetry over UDP in Python** is `sensors.py`: a gateway and two sensors, three mudRPC peers over UDP in one loop, using the binding's own API with the messages `protoc` generates from [sensor.proto](../sensor.proto). The sensors publish a reading each on a topic the gateway listens to, and the gateway calibrates one of them by a call.

From this folder, with a Rust toolchain and Python 3:

```sh
cargo build --release -p mudrpc-c --manifest-path ../../../Cargo.toml
python3 -m venv .venv
.venv/bin/pip install --require-hashes --only-binary=:all: -r requirements.txt
mkdir -p gen
.venv/bin/python -m grpc_tools.protoc -I .. --python_out=gen sensor.proto
MUDRPC_LIB=$PWD/../../../target/release/libmudrpc.dylib PYTHONPATH=../../../bindings/python:gen .venv/bin/python sensors.py
```

On Linux the library is `libmudrpc.so`. It prints:

```
reading sensor-1 21.50C
reading sensor-2 19.00C
calibrated sensor-2 by 0.50C
```

Part of [gRPC for the Internet of Things](../../../docs/grpc-for-iot.md); the binding is [bindings/python/SKILL.md](../../../bindings/python/SKILL.md).
