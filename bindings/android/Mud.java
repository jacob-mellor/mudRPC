// mudRPC for Android: the Java binding over the C ABI through JNI, since Android has no java.lang.foreign; the same classes
// and methods as bindings/java/Mud.java, so MudChannel.java and a product written against either compile against both. Copy
// it into your project as mudrpc/Mud.java with mud_jni.c built by the NDK into libmudrpc.so (see SKILL.md); on a desktop JVM,
// point MUDRPC_LIB at that library.
//
// A call is a message; a reply may come hours later or expire; nothing blocks. You poll: drain outgoing() into your
// transport and call sent(), feed what arrives to deliver(), call tick(). `now` is your uptime in seconds.
package mudrpc;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.ArrayList;
import java.util.List;

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

    /** The natives mud_jni.c registers: pointers as long, buffers as byte[], results packed in native byte order. */
    static final class Native {
        static {
            String env = System.getenv("MUDRPC_LIB");
            if (env != null && !env.isEmpty()) System.load(env); else System.loadLibrary("mudrpc");
        }
        static native long size(); static native long hubSize(); static native long alloc(long size); static native void free(long mem);
        static native int newPeer(long mem, long len, byte[] name, byte[] key, int sender, int epoch, byte[] seed, int budget, int hub, int cipher);
        static native byte[] save(long mem, long len); static native int resume(long mem, long len, byte[] saved, int epoch); static native int renew(long mem, long len, int epoch); static native int drop(long mem, long len);
        static native int stage(long mem, long len, byte[] key); static native int rotate(long mem, long len); static native int retire(long mem, long len); static native int roster(long mem, long len, int sender, byte[] key, byte[] next); static native int revoke(long mem, long len, int sender); static native int opened(long mem, long len); static native byte[] publicKey(byte[] seed);
        static native byte[] call(long mem, long len, int now, byte[] service, byte[] method, byte[] req, int ttl); static native int publish(long mem, long len, int now, byte[] topic, byte[] payload, int ttl); static native int listen(long mem, long len, byte[] topic);
        static native int reply(long mem, long len, int now, byte[] id, byte[] topic, byte[] method, byte[] replyTo, byte[] bytes, int ttl, int seqIn, int endIn, int seq, int status, byte[] detail, byte[] payload, int end);
        static native byte[] deliver(long mem, long len, int now, byte[] topic, byte[] wire); static native byte[] tick(long mem, long len, int now); static native byte[] outgoing(long mem, long len, int now); static native int sent(long mem, long len);
        static native int pressure(long mem, long len); static native int pending(long mem, long len); static native int queued(long mem, long len); static native int count(long mem, long len, int which);
        static native String statusName(int status); static native int internet(); static native long udpNew(byte[] bind, byte[] peers, int[] code); static native void udpFree(long udp); static native int udpPort(long udp); static native int udpSend(long udp, byte[] topic, byte[] wire); static native int udpListen(long udp, byte[] topic); static native byte[] udpPoll(long udp);
        static native long simNew(byte[] text, int[] line); static native void simFree(long sim); static native int simPeers(long sim); static native long simLastEventMs(long sim); static native long simNowMs(long sim); static native void simStep(long sim, long dt); static native void simNote(long sim, byte[] text); static native void simSilent(long sim, int radio, int on);
        static native int simSend(long sim, int radio, byte[] topic, byte[] wire, int ttl); static native int simListen(long sim, int radio, byte[] topic); static native byte[] simPoll(long sim, int radio); static native byte[] simLog(long sim); static native int simEvents(long sim); static native byte[] simEvent(long sim, int i);
    }

    static int check(int code) { if (code < 0) throw new MudException(code); return code; }
    static byte[] text(String s) { return s.getBytes(UTF_8); }
    /** A packed result: its leading code checked, the rest read in native order. */
    static ByteBuffer read(byte[] packed) { ByteBuffer b = ByteBuffer.wrap(packed).order(ByteOrder.nativeOrder()); check(b.getInt()); return b; }
    static byte[] take(ByteBuffer b, int n) { byte[] out = new byte[n]; b.get(out); return out; }
    static byte[] bytes(ByteBuffer b) { return take(b, b.getInt()); }
    static String str(ByteBuffer b) { return new String(bytes(b), UTF_8); }

    /** gRPC's name for a status number, {@code "FAILED_PRECONDITION"} for 9, {@code ""} past 16. */
    public static String statusName(int status) { return Native.statusName(status); }

    static Incoming incoming(ByteBuffer b) {
        Kind kind = Kind.values()[b.getInt()];
        int ttl = b.getInt(), seq = b.getInt(), status = b.getInt(), end = b.getInt(), dropped = b.getInt(), sender = b.getInt();
        byte[] id = take(b, 12);
        return new Incoming(kind, id, str(b), str(b), str(b), str(b), bytes(b), ttl, seq, status, end != 0, Dropped.values()[dropped], sender);
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
        static final long SIZE = Native.size(), HUB_SIZE = Native.hubSize();
        private final long len;
        private long mem;   // 0 after close, which every entry refuses as -1

        private Peer(long size) { len = size; mem = Native.alloc(size); if (mem == 0) throw new MudException(-5); }
        private long mem() { if (mem == 0) throw new MudException(-1); return mem; }

        public Peer(String name, byte[] key, int sender, int epoch) { this(name, key, sender, epoch, null, 1000, false); }
        public Peer(String name, byte[] key, int sender, int epoch, byte[] seed, int budget) { this(name, key, sender, epoch, seed, budget, false); }

        public Peer(String name, byte[] key, int sender, int epoch, byte[] seed, int budget, boolean hub) { this(name, key, sender, epoch, seed, budget, hub, CIPHER_DEFAULT); }

        /** Unsigned under the fleet key alone, or signed with this sender's 32-byte Ed25519 seed and a verification budget per second; {@code hub} for the hub shape; {@code cipher} {@link #CIPHER_DEFAULT} from the shorter constructors. */
        public Peer(String name, byte[] key, int sender, int epoch, byte[] seed, int budget, boolean hub, int cipher) {
            this(hub ? HUB_SIZE : SIZE);
            if (key.length != 32) { close(); throw new IllegalArgumentException("the key is 32 bytes"); }
            if (seed != null && seed.length != 32) { close(); throw new IllegalArgumentException("the seed is 32 bytes"); }
            try { check(Native.newPeer(mem, len, text(name), key, sender, epoch, seed == null ? new byte[0] : seed, budget, hub ? 1 : 0, cipher)); } catch (RuntimeException e) { close(); throw e; }
        }

        /** A peer from {@link #save} bytes of this same build of the library, with a higher epoch from the host. */
        public static Peer restore(byte[] saved, int epoch) {
            if (saved.length != SIZE && saved.length != HUB_SIZE) throw new MudException(-4);
            Peer p = new Peer(saved.length);
            try { check(Native.resume(p.mem, p.len, saved, epoch)); } catch (RuntimeException e) { p.close(); throw e; }
            return p;
        }

        /** Signed peers: a roster line for sender, its public key and the next one during a rotation (null otherwise). */
        public void roster(int sender, byte[] key, byte[] next) { check(Native.roster(mem(), len, sender, key, next == null ? new byte[0] : next)); }
        public void revoke(int sender) { check(Native.revoke(mem(), len, sender)); }
        /** The 32-byte public key of a seed, for rosters. */
        public static byte[] publicKey(byte[] seed) { return take(read(Native.publicKey(seed)), 32); }

        /** The peer's bytes, to write to flash. They contain the key, and cannot send until restore has resumed them. */
        public byte[] save() { ByteBuffer b = read(Native.save(mem(), len)); return take(b, b.remaining()); }
        /** A higher epoch for a live peer; clears "counter exhausted". */
        public void renew(int epoch) { check(Native.renew(mem(), len, epoch)); }
        /** The next fleet key, before the flag day. */
        public void stage(byte[] key) {
            if (key.length != 32) throw new IllegalArgumentException("the key is 32 bytes");
            check(Native.stage(mem(), len, key));
        }
        /** Previous ← current ← next. */
        public void rotate() { check(Native.rotate(mem(), len)); }
        /** Drop the previous key; late wires under it are refused from now on. */
        public void retire() { check(Native.retire(mem(), len)); }
        /** Which key slot opened the last accepted wire. */
        public Slot opened() { return Slot.values()[check(Native.opened(mem(), len))]; }
        /** Live table entries evicted, or senders refused a window, for want of room; rising means replay protection is degraded. */
        public int pressure() { return mem == 0 ? 0 : Native.pressure(mem, len); }
        /** Calls in flight, of 16: at 16 a call is full. */
        public int pending() { return mem == 0 ? 0 : Native.pending(mem, len); }
        /** Items queued for the transport, of 16: at 16 every producer is full. */
        public int queued() { return mem == 0 ? 0 : Native.queued(mem, len); }
        /** The operator's counters, kept across restores: managed (sealer taken for a change), rollbacks, floods, dropped. */
        public Counts counts() { return mem == 0 ? new Counts(0, 0, 0, 0) : new Counts(Native.count(mem, len, 0), Native.count(mem, len, 1), Native.count(mem, len, 2), Native.count(mem, len, 3)); }
        public record Counts(int managed, int rollbacks, int floods, int dropped) {}

        /** Queues a call. Returns its id; the reply carries the same id, or {@link #tick} reports it expired. */
        public byte[] call(int now, String service, String method, byte[] request, int ttlSecs) { return take(read(Native.call(mem(), len, now, text(service), text(method), request, ttlSecs)), 12); }

        /** Queues an event on a topic; no reply. */
        public void publish(int now, String topic, byte[] payload, int ttlSecs) { check(Native.publish(mem(), len, now, text(topic), payload, ttlSecs)); }

        /** Announce and subscribe, one act. */
        public void listen(String topic) { check(Native.listen(mem(), len, text(topic))); }

        /** Answers a CALL exactly as {@link #deliver} returned it, in one piece. */
        public void reply(int now, Incoming call, int status, byte[] payload) { reply(now, call, status, payload, true, 0, ""); }

        /** Stream: seq 0..n, end on the last. */
        public void reply(int now, Incoming call, int status, byte[] payload, boolean end, int seq) { reply(now, call, status, payload, end, seq, ""); }

        /** With the status's detail, its UTF-8 text, empty for none. */
        public void reply(int now, Incoming call, int status, byte[] payload, boolean end, int seq, String detail) {
            if (call.kind() != Kind.CALL || call.id().length != 12) throw new MudException(-1);
            check(Native.reply(mem(), len, now, call.id(), text(call.topic()), text(call.method()), text(call.replyTo()), call.bytes(), call.ttlSecs(), call.seq(), call.end() ? 1 : 0, seq, status, text(detail), payload, end ? 1 : 0));
        }

        /** A wire from the transport. Every wire gets a verdict; a DROPPED kind names why. */
        public Incoming deliver(int now, String topic, byte[] wire) { return incoming(read(Native.deliver(mem(), len, now, text(topic), wire))); }

        /** Time passes: the next expired call, or null. Call until null. */
        public Incoming tick(int now) { ByteBuffer b = read(Native.tick(mem(), len, now)); return b.hasRemaining() ? incoming(b) : null; }

        /** Next item for the transport, left in place until {@link #sent}. Null when the outbox is empty. */
        public Outgoing outgoing(int now) {
            byte[] packed = Native.outgoing(mem(), len, now);
            ByteBuffer b = read(packed);
            int kind = b.getInt(0);
            if (kind == 0) return null;
            int ttl = b.getInt();
            return new Outgoing(kind == 2, str(b), bytes(b), ttl);
        }

        /** The transport took the item {@link #outgoing} showed. */
        public void sent() { check(Native.sent(mem(), len)); }

        /** Ends the peer: its block is zeroed, keys included, then freed. Every later call throws {@code MudException(-1)}. */
        @Override public void close() { if (mem != 0) { Native.drop(mem, len); Native.free(mem); mem = 0; } }
    }

    /**
     * A product's UDP: each wire to every listed peer, on private addresses alone unless the library was built with the
     * cargo feature {@code internet}, which no release is. A channel's pump and the owner's {@link #close} never meet
     * inside the library.
     */
    public static final class Udp implements AutoCloseable {
        /** A wire from a listed peer. */
        public record Datagram(String topic, byte[] wire) {}
        private long udp;

        /** Whether this library may bind or send to a public address. */
        public static boolean internet() { return Native.internet() == 1; }

        /** Binds {@code "ip:port"} and sends to each peer's, at least one; a bad or public address, or no peer, is {@code MudException} -1 ({@code 0.0.0.0} is public), a bind the system refuses -2. */
        public Udp(String bind, String... peers) {
            int[] code = new int[1];
            udp = Native.udpNew(text(bind), text(String.join(" ", peers)), code);
            if (udp == 0) throw new MudException(code[0]);
        }

        private long live() { if (udp == 0) throw new IllegalStateException("the socket is closed"); return udp; }

        /** The bound port, the one chosen for port 0. */
        public synchronized int port() { return check(Native.udpPort(live())); }

        /** One outgoing item: a listen keeps wires on its topic from then, every other topic dropped unread; a send throws when no peer took the wire, so the item stays queued. */
        public synchronized void send(Outgoing o) { check(o.listen() ? Native.udpListen(live(), text(o.topic())) : Native.udpSend(live(), text(o.topic()), o.wire())); }

        /** The next wire from a listed peer, or null. */
        public synchronized Datagram poll() {
            byte[] packed = Native.udpPoll(live());
            ByteBuffer b = read(packed);
            return b.getInt(0) == 1 ? new Datagram(str(b), bytes(b)) : null;
        }

        @Override public synchronized void close() { if (udp != 0) { Native.udpFree(udp); udp = 0; } }
    }

    /** The simulator: one world from scenario text, radios 0..peers()-1. A development tool. */
    public static final class Sim implements AutoCloseable {
        /** What a radio handed up: kind 1 a message with its topic and wire, 2 on-mesh, 3 off-mesh. */
        public record Rx(int kind, String topic, byte[] wire) {}
        static final String[] EVENT_KINDS = { "", "partition", "heal", "jam", "unjam", "malformed", "replay", "flood", "bandwidth", "peer_down", "peer_up", "clone", "spam", "stage", "rotate", "retire", "warp", "hostile", "noise", "stale_reboot", "torn_save", "revoke", "telemetry", "hold" };
        private long sim;
        private final int peers;
        private final long lastEventMs;

        public Sim(String scenario) {
            int[] line = new int[1];
            sim = Native.simNew(text(scenario), line);
            if (sim == 0) throw new IllegalArgumentException("scenario error at line " + line[0]);
            peers = Native.simPeers(sim);
            lastEventMs = Native.simLastEventMs(sim);
        }

        private long live() { if (sim == 0) throw new IllegalStateException("the simulator is closed"); return sim; }
        public int peers() { return peers; }
        public long lastEventMs() { return lastEventMs; }
        public long nowMs() { return Native.simNowMs(live()); }
        public void step() { step(100); }
        public void step(long dtMs) { Native.simStep(live(), dtMs); }
        public void note(String text) { Native.simNote(live(), text(text)); }
        public void silent(int radio, boolean on) { Native.simSilent(live(), radio, on ? 1 : 0); }

        /** False when the radio is down; the item stays with the peer. */
        public boolean send(int radio, String topic, byte[] wire, int ttlSecs) { return up(Native.simSend(live(), radio, text(topic), wire, ttlSecs)); }

        public boolean listen(int radio, String topic) { return up(Native.simListen(live(), radio, text(topic))); }

        static boolean up(int code) { if (code == -1) throw new MudException(code); return code == 0; }

        /** Null when idle. */
        public Rx poll(int radio) {
            byte[] packed = Native.simPoll(live(), radio);
            ByteBuffer b = read(packed);
            int k = b.getInt(0);
            return k == 0 ? null : new Rx(k, k == 1 ? str(b) : "", k == 1 ? bytes(b) : new byte[0]);
        }

        public List<SimEvent> events() {
            List<SimEvent> list = new ArrayList<>();
            for (int i = 0; i < Native.simEvents(live()); i++) {
                ByteBuffer b = read(Native.simEvent(sim, i));
                list.add(new SimEvent(b.getLong(), EVENT_KINDS[b.getInt()], b.getLong(), b.getLong(), str(b)));
            }
            return list;
        }

        public String log() { return new String(Native.simLog(live()), UTF_8); }

        @Override public void close() { if (sim != 0) { Native.simFree(sim); sim = 0; } }
    }
}
