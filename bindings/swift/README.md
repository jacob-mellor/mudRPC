# Swift, and grpc-swift over mudRPC

mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The Swift binding is one file, `Mud.swift`, over the C ABI, which Swift imports from `bindings/c/mudrpc.h` through `bindings/c/module.modulemap` as the module `CMudRPC`; `MudGrpc.swift` beside it is the drop-in for grpc-swift. Swift 6.1 or later, on macOS 15 or later and on Linux; iOS's library is built in the gate, its simulator run waiting on Xcode.

The document is [SKILL.md](SKILL.md): integrate step by step, verify, test at each rung, known edges.
