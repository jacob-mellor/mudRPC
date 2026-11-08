---
name: mudrpc-android
description: How to integrate, verify and test the Android binding for mudRPC: the Java binding over JNI, and grpc-java's drop-in on it.
---
mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. Android has no `java.lang.foreign`, so its binding is `Mud.java` here, the same classes and methods as `bindings/java/Mud.java` over JNI, with `mud_jni.c`, the glue the NDK builds into `libmudrpc.so`. Everything written against the desktop binding, `MudChannel.java` included, compiles against this one unchanged.

## Integrate

**1. Build the library for the device.** A Rust toolchain with the Android target, and the NDK (`brew install --cask android-ndk`, or Android Studio's):

```sh
rustup target add aarch64-linux-android
cargo rustc --release -p mudrpc-c --target aarch64-linux-android --crate-type staticlib
NDK=/opt/homebrew/share/android-ndk   # or $ANDROID_NDK_HOME
$NDK/toolchains/llvm/prebuilt/*/bin/aarch64-linux-android24-clang -shared -fPIC -o libmudrpc.so bindings/android/mud_jni.c -I bindings/c target/aarch64-linux-android/release/libmudrpc.a
```

Put `libmudrpc.so` in your app at `src/main/jniLibs/arm64-v8a/`, and copy `bindings/android/Mud.java` into it as `mudrpc/Mud.java`; `System.loadLibrary("mudrpc")` runs when the class loads. The same four lines with `x86_64-linux-android` and `x86_64-linux-android24-clang` give the emulator's `x86_64/` library.

**2. Make one call.** The program in `bindings/java/SKILL.md`'s step 2 is this binding's too, line for line; the test runs it over JNI.

**3. The loop, UDP, your `.proto`, a signed fleet.** Steps 3 to 6 of `bindings/java/SKILL.md` apply unchanged: the API is the same class, `Mud.statusName` included.

**4. The gRPC drop-in.** Copy `bindings/java/MudChannel.java` beside `Mud.java`; grpc-java's generated stubs and `BindableService` run over it as that skill says, with `grpc-api` from Maven as the one dependency. grpc-kotlin's coroutine stubs the same.

**5. On a desktop JVM.** The glue builds against the JDK's `jni.h` for tests on your machine: `cc -shared -fPIC -o libmudrpc_jni.dylib bindings/android/mud_jni.c -I bindings/c -I $JAVA_HOME/include -I $JAVA_HOME/include/darwin -L target/debug -lmudrpc` (`linux` for `darwin` there, `.so` for `.dylib`), then `MUDRPC_LIB=/path/to/libmudrpc_jni.dylib` names it and `Mud.java` loads that instead of `libmudrpc`.

## Verify

- The two bindings are one API: `MudChannel.java` and the Java harness compile against either; the test compiles both against this one.
- Every entry copies its arguments in and its result out: a `byte[]` is pinned for the call and released after it, and a result is one `byte[]` read in native byte order; nothing in C keeps a pointer past the call.
- A peer's block lives in C memory, aligned as `mud_align()` asks; `close()` zeroes it through the library and frees it, and every later call throws `MudException(-1)`, with `pending()`, `queued()`, `pressure()` and `counts()` reading 0.
- The host mistakes and their answers are the desktop skill's table: the codes come from the same library.

`cargo test -p mudrpc-c --test abi -- --include-ignored the_android_binding` builds the glue against this machine's JDK, runs the Java harness over it on every scenario and mode with logs byte-identical to the Rust ones, runs the skill's example, compiles `MudChannel.java` against it, and links `libmudrpc.so` for arm64-v8a with the NDK.

## Test at each rung

- **In-process:** the desktop JVM over JNI, as the test does.
- **Emulator and device:** your app with `libmudrpc.so` under `jniLibs`; two devices on one Wi-Fi over `Mud.Udp`, private addresses only.
- **Relay:** a box on your own LAN, never on the internet.

## Known edges

- The host contract every binding shares is the list at the top of `bindings/c/mudrpc.h`.
- `System.loadLibrary("mudrpc")` needs the `.so` for the device's ABI under `jniLibs/<abi>/`; a missing one is Android's `UnsatisfiedLinkError`, naming the library. R8 keeps the natives under its default rule `-keepclasseswithmembernames class * { native <methods>; }`; a project that drops the defaults adds it back for `mudrpc.Mud$Native`.
- A null `byte[]` is `MudException(-1)`, where the desktop binding throws `NullPointerException`: the glue hands the library a buffer it refuses.
- One thread drives a peer, as everywhere; the library's busy byte names a second thread with `MudException(-10)`, nothing done.
- `Sim` is in the binding for the harness; a product leaves it unused, and the `.so` is the same.
