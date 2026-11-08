// mudRPC for Java: one file over the C ABI through the foreign function API (java.lang.foreign, Java 22 or later).
// No package to install, no dependencies, no JNI. Copy it into your project as mudrpc/Mud.java, run with
// --enable-native-access=ALL-UNNAMED, and point MUDRPC_LIB at libmudrpc or put the library next to your classes or jar.
//
// A call is a message; a reply may come hours later or expire; nothing blocks. You poll: drain outgoing() into your
// transport and call sent(), feed what arrives to deliver(), call tick(). `now` is your uptime in seconds.
package mudrpc;

import java.lang.foreign.*;
import java.lang.invoke.MethodHandle;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;

import static java.lang.foreign.ValueLayout.*;
import static java.nio.charset.StandardCharsets.UTF_8;

public final class Mud {
    private Mud() {}

    /** What a delivered wire was, or why it was dropped. */
    public enum Kind { NONE, CALL, REPLY, EVENT, EXPIRED, DROPPED, FLOOD, ROLLBACK }

    /** Why a wire never reached your code. Log every one in development; count them in the field. */
    public enum Dropped { NONE, VERSION, OPEN, DECODE, DUPLICATE, NO_PENDING, TOPIC, SEQ, ID, REPLAY, RATE }

    /** Which key slot opened the last accepted wire. */
    public enum Slot { CURRENT, PREVIOUS, NEXT }

    /** A failure code from the library. {@link #code} is the C constant: -2 full, -3 too large, -4 corrupt, -6 epoch, -7 unprovisioned, -8 exhausted, -9 rate, -10 busy. */
    public static final class MudException extends RuntimeException {
        public final int code;
        MudException(int code) { super("mudRPC: " + name(code)); this.code = code; }
        public boolean isFull() { return code == -2; }
        static String name(int c) {
            return switch (c) { case -1 -> "invalid argument"; case -2 -> "full"; case -3 -> "too large"; case -4 -> "corrupt"; case -5 -> "bad memory"; case -6 -> "epoch not higher"; case -7 -> "unprovisioned (sender 0)"; case -8 -> "counter exhausted"; case -9 -> "this second's sends spent"; case -10 -> "busy: another thread holds this peer"; case -99 -> "panic"; default -> Integer.toString(c); };
        }
    }

    /** One delivered wire. For a CALL, pass it back to {@link Peer#reply}. For FLOOD and ROLLBACK, {@code sender} is the number concerned. */
    public record Incoming(Kind kind, byte[] id, String topic, String method, String replyTo, String detail, byte[] bytes, int ttlSecs, int seq, int status, boolean end, Dropped dropped, int sender) {}

    /** What the transport must do next: listen on a topic, or send a wire with its lifetime. */
    public record Outgoing(boolean listen, String topic, byte[] wire, int ttlSecs) {}

    /** A scenario event from the simulator; clone, spam, stage, rotate, retire, warp, hostile, stale_reboot, torn_save, revoke and telemetry are the host's to perform. */
    public record SimEvent(long atMs, String kind, long a, long b, String text) {}

    // The C ABI, field for field from mudrpc.h. mud_buf is {pointer, size_t}; mud_id is 12 bytes with alignment 1, so it
    // follows kind at offset 4 and nothing is padded after it; where C pads, the padding is written out.
    static final StructLayout BUF = MemoryLayout.structLayout(ADDRESS.withName("ptr"), JAVA_LONG.withName("len"));
    static final StructLayout CONFIG = MemoryLayout.structLayout(BUF.withName("name"), BUF.withName("key"), JAVA_INT.withName("sender"), JAVA_INT.withName("epoch"), BUF.withName("seed"), JAVA_INT.withName("budget"), JAVA_INT.withName("hub"), JAVA_INT.withName("cipher"), MemoryLayout.paddingLayout(4));
    static final StructLayout IN = MemoryLayout.structLayout(JAVA_INT.withName("kind"), MemoryLayout.sequenceLayout(12, JAVA_BYTE).withName("id"),
        BUF.withName("topic"), BUF.withName("method"), BUF.withName("reply_to"), BUF.withName("detail"), BUF.withName("bytes"),
        JAVA_INT.withName("ttl_secs"), JAVA_INT.withName("seq"), JAVA_INT.withName("status"), JAVA_INT.withName("end"), JAVA_INT.withName("dropped"), JAVA_INT.withName("sender"));
    static final StructLayout OUT = MemoryLayout.structLayout(JAVA_INT.withName("kind"), MemoryLayout.paddingLayout(4), BUF.withName("topic"), BUF.withName("wire"), JAVA_INT.withName("ttl_secs"), MemoryLayout.paddingLayout(4));
    static final StructLayout EVENT = MemoryLayout.structLayout(JAVA_LONG.withName("at_ms"), JAVA_INT.withName("kind"), MemoryLayout.paddingLayout(4), JAVA_LONG.withName("a"), JAVA_LONG.withName("b"), BUF.withName("text"));

    static final Linker LINKER = Linker.nativeLinker();
    static final SymbolLookup LIB = load();
    static final MethodHandle
        mud_size = fn("mud_size", JAVA_LONG), mud_hub_size = fn("mud_hub_size", JAVA_LONG), mud_align = fn("mud_align", JAVA_LONG),
        mud_new = fn("mud_new", JAVA_INT, ADDRESS, JAVA_LONG, ADDRESS),
        mud_save = fn("mud_save", JAVA_INT, ADDRESS, JAVA_LONG, ADDRESS, JAVA_LONG), mud_resume = fn("mud_resume", JAVA_INT, ADDRESS, JAVA_LONG, JAVA_INT), mud_renew = fn("mud_renew", JAVA_INT, ADDRESS, JAVA_LONG, JAVA_INT), mud_drop = fn("mud_drop", JAVA_INT, ADDRESS, JAVA_LONG), mud_roster = fn("mud_roster", JAVA_INT, ADDRESS, JAVA_LONG, JAVA_INT, BUF, BUF), mud_revoke = fn("mud_revoke", JAVA_INT, ADDRESS, JAVA_LONG, JAVA_INT), mud_public_key = fn("mud_public_key", JAVA_INT, BUF, ADDRESS),
        mud_stage = fn("mud_stage", JAVA_INT, ADDRESS, JAVA_LONG, BUF), mud_rotate = fn("mud_rotate", JAVA_INT, ADDRESS, JAVA_LONG), mud_retire = fn("mud_retire", JAVA_INT, ADDRESS, JAVA_LONG), mud_opened = fn("mud_opened", JAVA_INT, ADDRESS, JAVA_LONG),
        mud_call = fn("mud_call", JAVA_INT, ADDRESS, JAVA_LONG, JAVA_INT, BUF, BUF, BUF, JAVA_INT, ADDRESS),
        mud_publish = fn("mud_publish", JAVA_INT, ADDRESS, JAVA_LONG, JAVA_INT, BUF, BUF, JAVA_INT),
        mud_listen = fn("mud_listen", JAVA_INT, ADDRESS, JAVA_LONG, BUF),
        mud_reply = fn("mud_reply", JAVA_INT, ADDRESS, JAVA_LONG, JAVA_INT, ADDRESS, JAVA_INT, JAVA_INT, BUF, BUF, JAVA_INT),
        mud_deliver = fn("mud_deliver", JAVA_INT, ADDRESS, JAVA_LONG, JAVA_INT, BUF, BUF, BUF, ADDRESS),
        mud_tick = fn("mud_tick", JAVA_INT, ADDRESS, JAVA_LONG, JAVA_INT, ADDRESS),
        mud_outgoing = fn("mud_outgoing", JAVA_INT, ADDRESS, JAVA_LONG, JAVA_INT, BUF, ADDRESS),
        mud_sent = fn("mud_sent", JAVA_INT, ADDRESS, JAVA_LONG), mud_pressure = fn("mud_pressure", JAVA_INT, ADDRESS, JAVA_LONG), mud_pending = fn("mud_pending", JAVA_INT, ADDRESS, JAVA_LONG), mud_queued = fn("mud_queued", JAVA_INT, ADDRESS, JAVA_LONG), mud_count = fn("mud_count", JAVA_INT, ADDRESS, JAVA_LONG, JAVA_INT),
        mud_status_name = fn("mud_status_name", ADDRESS, JAVA_INT), mud_internet = fn("mud_internet", JAVA_INT), mud_udp_new = fn("mud_udp_new", ADDRESS, BUF, BUF, ADDRESS), mud_udp_free = fn("mud_udp_free", null, ADDRESS),
        mud_udp_port = fn("mud_udp_port", JAVA_INT, ADDRESS), mud_udp_send = fn("mud_udp_send", JAVA_INT, ADDRESS, BUF, BUF), mud_udp_listen = fn("mud_udp_listen", JAVA_INT, ADDRESS, BUF), mud_udp_poll = fn("mud_udp_poll", JAVA_INT, ADDRESS, BUF, ADDRESS, ADDRESS),
        mud_sim_new = fn("mud_sim_new", ADDRESS, BUF, ADDRESS), mud_sim_free = fn("mud_sim_free", null, ADDRESS),
        mud_sim_peers = fn("mud_sim_peers", JAVA_INT, ADDRESS), mud_sim_last_event_ms = fn("mud_sim_last_event_ms", JAVA_LONG, ADDRESS), mud_sim_now_ms = fn("mud_sim_now_ms", JAVA_LONG, ADDRESS),
        mud_sim_step = fn("mud_sim_step", null, ADDRESS, JAVA_LONG), mud_sim_note = fn("mud_sim_note", null, ADDRESS, BUF),
        mud_sim_send = fn("mud_sim_send", JAVA_INT, ADDRESS, JAVA_INT, BUF, BUF, JAVA_INT), mud_sim_listen = fn("mud_sim_listen", JAVA_INT, ADDRESS, JAVA_INT, BUF),
        mud_sim_poll = fn("mud_sim_poll", JAVA_INT, ADDRESS, JAVA_INT, BUF, ADDRESS, ADDRESS), mud_sim_silent = fn("mud_sim_silent", null, ADDRESS, JAVA_INT, JAVA_INT),
        mud_sim_log = fn("mud_sim_log", JAVA_INT, ADDRESS, BUF), mud_sim_events = fn("mud_sim_events", JAVA_INT, ADDRESS), mud_sim_event = fn("mud_sim_event", JAVA_INT, ADDRESS, JAVA_INT, ADDRESS);

    static SymbolLookup load() {
        String env = System.getenv("MUDRPC_LIB");
        Path lib = env == null || env.isEmpty() ? beside() : Path.of(env);
        if (!Files.exists(lib)) lib = unpacked(lib.getFileName().toString(), lib);
        return SymbolLookup.libraryLookup(lib, Arena.global());
    }

    /** The library inside the jar, as the Maven artefact carries it, copied out for this run; without one, the error names the path tried. */
    static Path unpacked(String file, Path tried) {
        try (java.io.InputStream in = Mud.class.getResourceAsStream("/" + file)) {
            if (in == null) throw new UnsatisfiedLinkError("libmudrpc not found at " + tried + ": build with `cargo build --release -p mudrpc-c`, then set MUDRPC_LIB or put it beside the classes");
            Path tmp = Files.createTempFile("mudrpc-", file);
            tmp.toFile().deleteOnExit();
            Files.copy(in, tmp, java.nio.file.StandardCopyOption.REPLACE_EXISTING);
            return tmp;
        } catch (java.io.IOException e) { throw new UnsatisfiedLinkError("cannot unpack " + file + ": " + e); }
    }

    /** libmudrpc next to the classes directory or the jar this class came from. */
    static Path beside() {
        String os = System.getProperty("os.name").toLowerCase();
        String file = os.contains("win") ? "mudrpc.dll" : os.contains("mac") ? "libmudrpc.dylib" : "libmudrpc.so";
        try {
            Path here = Path.of(Mud.class.getProtectionDomain().getCodeSource().getLocation().toURI());
            return (Files.isDirectory(here) ? here : here.getParent()).resolve(file);
        } catch (Exception e) { throw new UnsatisfiedLinkError("cannot tell where Mud.class lives: set MUDRPC_LIB"); }
    }

    static MethodHandle fn(String name, MemoryLayout ret, MemoryLayout... args) {
        MemorySegment sym = LIB.find(name).orElseThrow(() -> new UnsatisfiedLinkError(name + " missing: libmudrpc is from another build"));
        return LINKER.downcallHandle(sym, ret == null ? FunctionDescriptor.ofVoid(args) : FunctionDescriptor.of(ret, args));
    }

    /** One downcall. The handles match the header, so the only Throwable left is a bug. */
    static Object invoke(MethodHandle h, Object... args) {
        try { return h.invokeWithArguments(args); } catch (Throwable t) { throw new IllegalStateException(t); }
    }
    static int check(int code) { if (code < 0) throw new MudException(code); return code; }
    static long at(StructLayout l, String field) { return l.byteOffset(MemoryLayout.PathElement.groupElement(field)); }

    /** A mud_buf over `data`, for one call. */
    static MemorySegment buf(Arena a, MemorySegment data) { MemorySegment b = a.allocate(BUF); b.set(ADDRESS, 0, data); b.set(JAVA_LONG, 8, data.byteSize()); return b; }
    static MemorySegment buf(Arena a, byte[] data) { return buf(a, data.length == 0 ? MemorySegment.NULL : a.allocateFrom(JAVA_BYTE, data)); }
    static MemorySegment text(Arena a, String s) { return buf(a, s.getBytes(UTF_8)); }
    /** The bytes a mud_buf at `off` in `s` points to, copied out. */
    static byte[] bytes(MemorySegment s, long off) { long n = s.get(JAVA_LONG, off + 8); return n == 0 ? new byte[0] : s.get(ADDRESS, off).reinterpret(n).toArray(JAVA_BYTE); }
    static String str(MemorySegment s, long off) { return new String(bytes(s, off), UTF_8); }

    /** gRPC's name for a status number, {@code "FAILED_PRECONDITION"} for 9, {@code ""} past 16. */
    public static String statusName(int status) { return ((MemorySegment) invoke(mud_status_name, status)).reinterpret(Long.MAX_VALUE).getString(0); }

    static Incoming wrap(MemorySegment r) {
        return new Incoming(Kind.values()[r.get(JAVA_INT, at(IN, "kind"))], r.asSlice(at(IN, "id"), 12).toArray(JAVA_BYTE), str(r, at(IN, "topic")), str(r, at(IN, "method")), str(r, at(IN, "reply_to")), str(r, at(IN, "detail")), bytes(r, at(IN, "bytes")),
            r.get(JAVA_INT, at(IN, "ttl_secs")), r.get(JAVA_INT, at(IN, "seq")), r.get(JAVA_INT, at(IN, "status")), r.get(JAVA_INT, at(IN, "end")) != 0, Dropped.values()[r.get(JAVA_INT, at(IN, "dropped"))], r.get(JAVA_INT, at(IN, "sender")));
    }

    /**
     * A mudRPC peer. {@code name} is its inbox topic; {@code key} the 32-byte fleet key; {@code sender} the roster number
     * (0 cannot send); {@code epoch} a number the host persists beside the key and raises on every boot and restore,
     * written to storage before the peer is created. A signed peer believes 16 senders; {@code hub} is the hub shape, 256,
     * for the host that believes a fleet; the line past either throws {@link MudException#isFull}. Not thread-safe: one
     * thread drives a peer. {@link #save} bytes contain the key; store them as you store the key.
     */
    public static final class Peer implements AutoCloseable {
        public static final int MAX_TOPIC = 64, MSG = 512;
        /** The ciphers, as {@code mudrpc.h} names them: AES the default, the NIST algorithm buyers ask for; ChaCha20-Poly1305 smaller and faster without an AES instruction. */
        public static final int CIPHER_CHACHA20_POLY1305 = 0, CIPHER_AES_256_GCM = 1, CIPHER_DEFAULT = CIPHER_AES_256_GCM;
        static final long SIZE = (long) invoke(mud_size), HUB_SIZE = (long) invoke(mud_hub_size), ALIGN = (long) invoke(mud_align);
        private final Arena arena = Arena.ofShared();
        private final MemorySegment scratch = arena.allocate(MAX_TOPIC + MSG), plain = arena.allocate(MSG);
        private MemorySegment mem;   // NULL after close, which every entry refuses as -1

        private Peer(long size) { mem = arena.allocate(size, ALIGN); }

        public Peer(String name, byte[] key, int sender, int epoch) { this(name, key, sender, epoch, null, 1000, false); }
        public Peer(String name, byte[] key, int sender, int epoch, byte[] seed, int budget) { this(name, key, sender, epoch, seed, budget, false); }

        public Peer(String name, byte[] key, int sender, int epoch, byte[] seed, int budget, boolean hub) { this(name, key, sender, epoch, seed, budget, hub, CIPHER_DEFAULT); }

        /** Unsigned under the fleet key alone, or signed with this sender's 32-byte Ed25519 seed and a verification budget per second; {@code hub} for the hub shape; {@code cipher} {@link #CIPHER_DEFAULT} from the shorter constructors. */
        public Peer(String name, byte[] key, int sender, int epoch, byte[] seed, int budget, boolean hub, int cipher) {
            this(hub ? HUB_SIZE : SIZE);
            if (key.length != 32) throw new IllegalArgumentException("the key is 32 bytes");
            if (seed != null && seed.length != 32) throw new IllegalArgumentException("the seed is 32 bytes");
            try (Arena a = Arena.ofConfined()) {
                MemorySegment cfg = a.allocate(CONFIG);
                cfg.copyFrom(text(a, name));
                cfg.asSlice(at(CONFIG, "key"), BUF.byteSize()).copyFrom(buf(a, key));
                cfg.set(JAVA_INT, at(CONFIG, "sender"), sender);
                cfg.set(JAVA_INT, at(CONFIG, "epoch"), epoch);
                cfg.asSlice(at(CONFIG, "seed"), BUF.byteSize()).copyFrom(buf(a, seed == null ? new byte[0] : seed));
                cfg.set(JAVA_INT, at(CONFIG, "budget"), budget);
                cfg.set(JAVA_INT, at(CONFIG, "hub"), hub ? 1 : 0);
                cfg.set(JAVA_INT, at(CONFIG, "cipher"), cipher);
                check((int) invoke(mud_new, mem, mem.byteSize(), cfg));
            } catch (RuntimeException e) { close(); throw e; }
        }

        /** A peer from {@link #save} bytes of this same build of the library, with a higher epoch from the host. */
        public static Peer restore(byte[] saved, int epoch) {
            if (saved.length != SIZE && saved.length != HUB_SIZE) throw new MudException(-4);
            Peer p = new Peer(saved.length);
            p.mem.copyFrom(MemorySegment.ofArray(saved));
            try { check((int) invoke(mud_resume, p.mem, p.mem.byteSize(), epoch)); } catch (RuntimeException e) { p.close(); throw e; }
            return p;
        }

        /** Signed peers: a roster line for sender, its public key and the next one during a rotation (null otherwise). */
        public void roster(int sender, byte[] key, byte[] next) {
            try (Arena a = Arena.ofConfined()) { check((int) invoke(mud_roster, mem, mem.byteSize(), sender, buf(a, key), buf(a, next == null ? new byte[0] : next))); }
        }
        public void revoke(int sender) { check((int) invoke(mud_revoke, mem, mem.byteSize(), sender)); }
        /** The 32-byte public key of a seed, for rosters. */
        public static byte[] publicKey(byte[] seed) {
            try (Arena a = Arena.ofConfined()) { MemorySegment out = a.allocate(32); check((int) invoke(mud_public_key, buf(a, seed), out)); return out.toArray(JAVA_BYTE); }
        }

        /** The peer's bytes, to write to flash. They contain the key, and cannot send until restore has resumed them. */
        public byte[] save() { if (mem.equals(MemorySegment.NULL)) throw new MudException(-1); try (Arena a = Arena.ofConfined()) { MemorySegment out = a.allocate(mem.byteSize(), ALIGN); check((int) invoke(mud_save, mem, mem.byteSize(), out, out.byteSize())); return out.toArray(JAVA_BYTE); } }
        /** A higher epoch for a live peer; clears "counter exhausted". */
        public void renew(int epoch) { check((int) invoke(mud_renew, mem, mem.byteSize(), epoch)); }
        /** The next fleet key, before the flag day. */
        public void stage(byte[] key) {
            if (key.length != 32) throw new IllegalArgumentException("the key is 32 bytes");
            try (Arena a = Arena.ofConfined()) { check((int) invoke(mud_stage, mem, mem.byteSize(), buf(a, key))); }
        }
        /** Previous ← current ← next. */
        public void rotate() { check((int) invoke(mud_rotate, mem, mem.byteSize())); }
        /** Drop the previous key; late wires under it are refused from now on. */
        public void retire() { check((int) invoke(mud_retire, mem, mem.byteSize())); }
        /** Which key slot opened the last accepted wire. */
        public Slot opened() { return Slot.values()[check((int) invoke(mud_opened, mem, mem.byteSize()))]; }
        /** Live table entries evicted, or senders refused a window, for want of room; rising means replay protection is degraded. */
        public int pressure() { return (int) invoke(mud_pressure, mem, mem.byteSize()); }
        /** Calls in flight, of 16: at 16 a call is full. */
        public int pending() { return (int) invoke(mud_pending, mem, mem.byteSize()); }
        /** Items queued for the transport, of 16: at 16 every producer is full. */
        public int queued() { return (int) invoke(mud_queued, mem, mem.byteSize()); }
        /** The operator's counters, kept across restores: managed (sealer taken for a change), rollbacks, floods, dropped. */
        public Counts counts() { return new Counts((int) invoke(mud_count, mem, mem.byteSize(), 0), (int) invoke(mud_count, mem, mem.byteSize(), 1), (int) invoke(mud_count, mem, mem.byteSize(), 2), (int) invoke(mud_count, mem, mem.byteSize(), 3)); }
        public record Counts(int managed, int rollbacks, int floods, int dropped) {}

        /** Queues a call. Returns its id; the reply carries the same id, or {@link #tick} reports it expired. */
        public byte[] call(int now, String service, String method, byte[] request, int ttlSecs) {
            try (Arena a = Arena.ofConfined()) {
                MemorySegment id = a.allocate(12);
                check((int) invoke(mud_call, mem, mem.byteSize(), now, text(a, service), text(a, method), buf(a, request), ttlSecs, id));
                return id.toArray(JAVA_BYTE);
            }
        }

        /** Queues an event on a topic; no reply. */
        public void publish(int now, String topic, byte[] payload, int ttlSecs) {
            try (Arena a = Arena.ofConfined()) { check((int) invoke(mud_publish, mem, mem.byteSize(), now, text(a, topic), buf(a, payload), ttlSecs)); }
        }

        /** Announce and subscribe, one act. */
        public void listen(String topic) { try (Arena a = Arena.ofConfined()) { check((int) invoke(mud_listen, mem, mem.byteSize(), text(a, topic))); } }

        /** Answers a CALL exactly as {@link #deliver} returned it, in one piece. */
        public void reply(int now, Incoming call, int status, byte[] payload) { reply(now, call, status, payload, true, 0, ""); }

        /** Stream: seq 0..n, end on the last. */
        public void reply(int now, Incoming call, int status, byte[] payload, boolean end, int seq) { reply(now, call, status, payload, end, seq, ""); }

        /** With the status's detail, its UTF-8 text, empty for none. */
        public void reply(int now, Incoming call, int status, byte[] payload, boolean end, int seq, String detail) {
            if (call.kind() != Kind.CALL || call.id().length != 12) throw new MudException(-1);
            try (Arena a = Arena.ofConfined()) {
                MemorySegment c = a.allocate(IN);
                c.set(JAVA_INT, at(IN, "kind"), 1);
                c.asSlice(at(IN, "id"), 12).copyFrom(MemorySegment.ofArray(call.id()));
                c.asSlice(at(IN, "topic"), 16).copyFrom(text(a, call.topic()));
                c.asSlice(at(IN, "method"), 16).copyFrom(text(a, call.method()));
                c.asSlice(at(IN, "reply_to"), 16).copyFrom(text(a, call.replyTo()));
                c.asSlice(at(IN, "bytes"), 16).copyFrom(buf(a, call.bytes()));
                c.set(JAVA_INT, at(IN, "ttl_secs"), call.ttlSecs());
                c.set(JAVA_INT, at(IN, "seq"), call.seq());
                c.set(JAVA_INT, at(IN, "end"), call.end() ? 1 : 0);
                check((int) invoke(mud_reply, mem, mem.byteSize(), now, c, seq, status, text(a, detail), buf(a, payload), end ? 1 : 0));
            }
        }

        /** A wire from the transport. Every wire gets a verdict; a DROPPED kind names why. */
        public Incoming deliver(int now, String topic, byte[] wire) {
            try (Arena a = Arena.ofConfined()) {
                MemorySegment in = a.allocate(IN);
                check((int) invoke(mud_deliver, mem, mem.byteSize(), now, text(a, topic), buf(a, wire), buf(a, plain), in));
                return wrap(in);
            }
        }

        /** Time passes: the next expired call, or null. Call until null. */
        public Incoming tick(int now) {
            try (Arena a = Arena.ofConfined()) {
                MemorySegment in = a.allocate(IN);
                return check((int) invoke(mud_tick, mem, mem.byteSize(), now, in)) == 0 ? null : wrap(in);
            }
        }

        /** Next item for the transport, left in place until {@link #sent}. Null when the outbox is empty. */
        public Outgoing outgoing(int now) {
            try (Arena a = Arena.ofConfined()) {
                MemorySegment o = a.allocate(OUT);
                int kind = check((int) invoke(mud_outgoing, mem, mem.byteSize(), now, buf(a, scratch), o));
                return kind == 0 ? null : new Outgoing(kind == 2, str(o, at(OUT, "topic")), bytes(o, at(OUT, "wire")), o.get(JAVA_INT, at(OUT, "ttl_secs")));
            }
        }

        /** The transport took the item {@link #outgoing} showed. */
        public void sent() { check((int) invoke(mud_sent, mem, mem.byteSize())); }

        /** Ends the peer: its block is zeroed, keys included, then freed. Every later call throws {@code MudException(-1)}. */
        @Override public void close() { if (arena.scope().isAlive()) { invoke(mud_drop, mem, mem.byteSize()); arena.close(); mem = MemorySegment.NULL; } }
    }

    /**
     * A product's UDP: each wire to every listed peer, on private addresses alone unless the library was built with the
     * cargo feature {@code internet}, which no release is. A channel's pump and the owner's {@link #close} never meet
     * inside the library.
     */
    public static final class Udp implements AutoCloseable {
        /** A wire from a listed peer. */
        public record Datagram(String topic, byte[] wire) {}
        private final Arena arena = Arena.ofShared();
        private final MemorySegment scratch = arena.allocate(Peer.MAX_TOPIC + Peer.MSG);
        private MemorySegment udp;

        /** Whether this library may bind or send to a public address. */
        public static boolean internet() { return (int) invoke(mud_internet) == 1; }

        /** Binds {@code "ip:port"} and sends to each peer's, at least one; a bad or public address, or no peer, is {@code MudException} -1 ({@code 0.0.0.0} is public), a bind the system refuses -2. */
        public Udp(String bind, String... peers) {
            try (Arena a = Arena.ofConfined()) {
                MemorySegment code = a.allocate(JAVA_INT);
                MemorySegment u = (MemorySegment) invoke(mud_udp_new, text(a, bind), text(a, String.join(" ", peers)), code);
                if (u.address() == 0) { arena.close(); throw new MudException(code.get(JAVA_INT, 0)); }
                udp = u;
            }
        }

        private MemorySegment live() { if (udp == null) throw new IllegalStateException("the socket is closed"); return udp; }

        /** The bound port, the one chosen for port 0. */
        public synchronized int port() { return check((int) invoke(mud_udp_port, live())); }

        /** One outgoing item: a listen keeps wires on its topic from then, every other topic dropped unread; a send throws when no peer took the wire, so the item stays queued. */
        public synchronized void send(Outgoing o) {
            try (Arena a = Arena.ofConfined()) { check((int) (o.listen() ? invoke(mud_udp_listen, live(), text(a, o.topic())) : invoke(mud_udp_send, live(), text(a, o.topic()), buf(a, o.wire())))); }
        }

        /** The next wire from a listed peer, or null. */
        public synchronized Datagram poll() {
            try (Arena a = Arena.ofConfined()) {
                MemorySegment topic = a.allocate(BUF), wire = a.allocate(BUF);
                return check((int) invoke(mud_udp_poll, live(), buf(a, scratch), topic, wire)) == 1 ? new Datagram(str(topic, 0), bytes(wire, 0)) : null;
            }
        }

        @Override public synchronized void close() { if (udp != null) { invoke(mud_udp_free, udp); udp = null; arena.close(); } }
    }

    /** The simulator: one world from scenario text, radios 0..peers()-1. A development tool. */
    public static final class Sim implements AutoCloseable {
        /** What a radio handed up: kind 1 a message with its topic and wire, 2 on-mesh, 3 off-mesh. */
        public record Rx(int kind, String topic, byte[] wire) {}
        static final String[] EVENT_KINDS = { "", "partition", "heal", "jam", "unjam", "malformed", "replay", "flood", "bandwidth", "peer_down", "peer_up", "clone", "spam", "stage", "rotate", "retire", "warp", "hostile", "noise", "stale_reboot", "torn_save", "revoke", "telemetry", "hold" };
        private final Arena arena = Arena.ofShared();
        private final MemorySegment scratch = arena.allocate(Peer.MAX_TOPIC + Peer.MSG);
        private final MemorySegment sim;
        private final int peers;
        private final long lastEventMs;

        public Sim(String scenario) {
            try (Arena a = Arena.ofConfined()) {
                MemorySegment line = a.allocate(JAVA_INT);
                sim = (MemorySegment) invoke(mud_sim_new, text(a, scenario), line);
                if (sim.address() == 0) { arena.close(); throw new IllegalArgumentException("scenario error at line " + line.get(JAVA_INT, 0)); }
            }
            peers = (int) invoke(mud_sim_peers, sim);
            lastEventMs = (long) invoke(mud_sim_last_event_ms, sim);
        }

        public int peers() { return peers; }
        public long lastEventMs() { return lastEventMs; }
        public long nowMs() { return (long) invoke(mud_sim_now_ms, sim); }
        public void step() { step(100); }
        public void step(long dtMs) { invoke(mud_sim_step, sim, dtMs); }
        public void note(String text) { try (Arena a = Arena.ofConfined()) { invoke(mud_sim_note, sim, text(a, text)); } }
        public void silent(int radio, boolean on) { invoke(mud_sim_silent, sim, radio, on ? 1 : 0); }

        /** False when the radio is down; the item stays with the peer. */
        public boolean send(int radio, String topic, byte[] wire, int ttlSecs) {
            try (Arena a = Arena.ofConfined()) { return up((int) invoke(mud_sim_send, sim, radio, text(a, topic), buf(a, wire), ttlSecs)); }
        }

        public boolean listen(int radio, String topic) {
            try (Arena a = Arena.ofConfined()) { return up((int) invoke(mud_sim_listen, sim, radio, text(a, topic))); }
        }

        static boolean up(int code) { if (code == -1) throw new MudException(code); return code == 0; }

        /** Null when idle. */
        public Rx poll(int radio) {
            try (Arena a = Arena.ofConfined()) {
                MemorySegment topic = a.allocate(BUF), wire = a.allocate(BUF);
                int k = check((int) invoke(mud_sim_poll, sim, radio, buf(a, scratch), topic, wire));
                return k == 0 ? null : new Rx(k, k == 1 ? str(topic, 0) : "", k == 1 ? bytes(wire, 0) : new byte[0]);
            }
        }

        public List<SimEvent> events() {
            List<SimEvent> list = new ArrayList<>();
            try (Arena a = Arena.ofConfined()) {
                MemorySegment e = a.allocate(EVENT);
                for (int i = 0; i < (int) invoke(mud_sim_events, sim); i++) {
                    check((int) invoke(mud_sim_event, sim, i, e));
                    list.add(new SimEvent(e.get(JAVA_LONG, at(EVENT, "at_ms")), EVENT_KINDS[e.get(JAVA_INT, at(EVENT, "kind"))], e.get(JAVA_LONG, at(EVENT, "a")), e.get(JAVA_LONG, at(EVENT, "b")), str(e, at(EVENT, "text"))));
                }
            }
            return list;
        }

        public String log() {
            try (Arena a = Arena.ofConfined()) {
                MemorySegment out = a.allocate((int) invoke(mud_sim_log, sim, a.allocate(BUF)));
                invoke(mud_sim_log, sim, buf(a, out));
                return new String(out.toArray(JAVA_BYTE), UTF_8);
            }
        }

        @Override public void close() { if (arena.scope().isAlive()) { invoke(mud_sim_free, sim); arena.close(); } }
    }
}
