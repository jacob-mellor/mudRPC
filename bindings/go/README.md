# Go, and grpc-go over mudRPC

mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The Go binding is one package over the C ABI in `bindings/c/mudrpc.h` through cgo: `mud.go`, the peer, with no dependency beyond the standard library, and `mud_grpc.go` beside it, the drop-in for gRPC, with the `google.golang.org/grpc` and `google.golang.org/protobuf` modules the product already has.

The document is [SKILL.md](SKILL.md): integrate step by step, verify, test at each rung, known edges.
