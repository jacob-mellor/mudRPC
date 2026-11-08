# .NET, and gRPC for .NET over mudRPC

mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The .NET binding is one file, `Mud.cs`, over the C ABI through P/Invoke, with no package and no unsafe code: copy it into your project and your IDE sees every type and comment. .NET 8 or later.

The document is [SKILL.md](SKILL.md): integrate step by step, verify, test at each rung, known edges.
