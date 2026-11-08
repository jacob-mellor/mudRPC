# The demos, and the phrase each owns

Two use cases, each one `.proto` and one program per language, every program printing the same lines, which the test holds them to. A folder's `README.md` is its front door: the phrase it owns in bold, what it runs, how, and the page it belongs to.

| Use case | What runs | Parent page |
| --- | --- | --- |
| [drones/](drones/) | a ground station and a mock drone, two peers over UDP on one machine: the drone is armed, three telemetry samples stream back, a landing is refused before arming and granted after; the gRPC drop-in where the language has one | [gRPC for drones](../docs/grpc-for-drones.md) |
| [sensors/](sensors/) | a gateway and two sensors, three peers over UDP in one loop: the sensors publish readings the gateway listens to, and the gateway calibrates one by a call; the binding's own API with protobuf messages, as a device would | [gRPC for the Internet of Things](../docs/grpc-for-iot.md) |

The phrases: `gRPC drone telemetry in <Language>` and `sensor telemetry over UDP in <Language>`, one folder each under `drones/` and `sensors/`, for Rust, C, C++, Python, .NET, Java, JavaScript, Swift, Go and PHP.
