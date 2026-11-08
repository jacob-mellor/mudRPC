# Python, and grpcio over mudRPC

mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The Python binding is one file, `mud.py`, over the C ABI through `ctypes`, with no pip package.

The document is [SKILL.md](SKILL.md): integrate step by step, verify, test at each rung, known edges.
