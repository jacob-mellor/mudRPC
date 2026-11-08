# C++, and gRPC C++ over mudRPC

mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The C++ binding is one header, `mud.hpp`, over the C ABI in `bindings/c/mudrpc.h`; `mud_grpc.hpp` beside it is gRPC C++'s sync API over mudRPC, and `protoc-gen-mud.cc` the protoc plugin that writes a `.proto`'s `Stub` and `Service` for it. C++17, the standard library and, for the drop-in, the protobuf the product already builds with.

The document is [SKILL.md](SKILL.md): integrate step by step, verify, test at each rung, known edges.
