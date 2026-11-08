# Java and Kotlin, and grpc-java over mudRPC

mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The Java binding is one file, `Mud.java`, over the C ABI through the foreign function API in `java.lang.foreign`, with no package and no JNI: copy it into your project as `mudrpc/Mud.java` (it declares `package mudrpc`) and your IDE sees every type and comment. Java 22 or later.

The document is [SKILL.md](SKILL.md): integrate step by step, verify, test at each rung, known edges.
