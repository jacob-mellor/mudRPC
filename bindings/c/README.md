# C, the ABI every other language sits on

mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The C ABI, `mudrpc.h`, is the one native surface every other language sits on: you own all memory, and the library never allocates, never keeps your pointers, never calls you back.

The document is [SKILL.md](SKILL.md): integrate step by step, verify, test at each rung, known edges.
