# gRPC without HTTP/2

**gRPC without HTTP/2** is what mudRPC is: gRPC's service model, services from a `.proto` with calls, replies, server streams, deadlines and status codes, carried with no HTTP at all, as sealed, self-contained packets of at most 512 bytes over whatever moves bytes.

gRPC's contract is the `.proto` and the code `protoc` generates from it. Its transport is HTTP/2: a live connection, framed streams, headers and trailers, a server listening on a port. Where there is no HTTP/2, there is no gRPC: a cloud or host that terminates HTTP/1.1 only, a browser that cannot speak HTTP/2 frames and needs a gRPC-Web proxy, a product that will not move to HTTP/3, and every link that is down more than it is up, where a connection cannot be kept and a request that waits for one blocks a thread.

mudRPC keeps the contract and drops the transport. Each call is one message with a nonce, a method and a payload, sealed under the fleet key to its service's topic; each reply carries the call's id, gRPC's status number and its detail, and a stream's items carry a sequence and an end flag. Nothing is connected: a reply may come hours later or expire, and a host polls, draining the peer's outgoing items into its transport and handing it what arrives. The transport is yours: [gRPC over UDP](grpc-over-udp.md) between listed peers on a private network, [gRPC over LoRa and mesh radio](grpc-over-lora-and-mesh-radio.md), a serial line, a file in a queue.

The code you have runs unchanged. Each language's gRPC library publishes an extension point, a channel or a transport, and mudRPC implements it, so the client and service `protoc` generated take a mudRPC channel where they took gRPC's; for C++, which publishes none, a protoc plugin writes the stub. The one line that changes is in [From gRPC](../README.md#from-grpc), per language, with what is not carried said once there.

A server needs no server stack, as [From gRPC](../README.md#from-grpc) says per language; [C# gRPC without IIS](csharp-grpc-without-iis.md) is the longer form for .NET. The library under all of it is [dependency-free gRPC](dependency-free-grpc.md): a `no_std` Rust core with no crates, under one C ABI and one file per language. Where it goes: [gRPC for drones](grpc-for-drones.md) and [gRPC for the Internet of Things](grpc-for-iot.md).

What is built and why, with every claim's test named: [SPEC.md](../SPEC.md). How to start, in five minutes: the [home page](../README.md).
