# Android, the Java binding over JNI

mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. Android has no `java.lang.foreign`, so its binding is `Mud.java` here, the same classes and methods as `bindings/java/Mud.java` over JNI, with `mud_jni.c`, the glue the NDK builds into `libmudrpc.so`. Everything written against the desktop binding, `MudChannel.java` included, compiles against this one unchanged.

The document is [SKILL.md](SKILL.md): integrate step by step, verify, test at each rung, known edges.
