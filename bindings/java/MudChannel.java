// mudRPC as gRPC's transport on Java and Kotlin: the stubs and service bases grpc-java's protoc plugin generates from a
// .proto, and grpc-kotlin's, run over mudRPC unchanged; building the channel is the one line that differs. Beside
// Mud.java, with grpc-api and grpc-stub, whose Channel, ClientCall and ServerCall this implements (io.grpc, Copyright
// The gRPC Authors, Apache License 2.0).
//
// Not a drop-in where gRPC needs a connection: a message of about 460 bytes, no metadata, unary and server-streaming
// calls only, at most 63 items a stream, the first reply wins, and 16 calls in flight. A server sends 32 replies a
// second, half what a receiver takes from one sender, its callers in turn, and a client keeps under 32 calls a second.
// A deadline is the call's ttl, a week at most; a call cancelled after it was sent keeps its place among the 16 until
// its ttl, since nothing reaches the server to say so.
package mudrpc;

import io.grpc.Attributes;
import io.grpc.BindableService;
import io.grpc.CallOptions;
import io.grpc.Channel;
import io.grpc.ClientCall;
import io.grpc.Context;
import io.grpc.Deadline;
import io.grpc.Grpc;
import io.grpc.Metadata;
import io.grpc.MethodDescriptor;
import io.grpc.ServerCall;
import io.grpc.ServerMethodDefinition;
import io.grpc.ServerServiceDefinition;
import io.grpc.Status;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.net.SocketAddress;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.HashMap;
import java.util.HashSet;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.ConcurrentLinkedQueue;
import java.util.concurrent.Executor;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.ScheduledThreadPoolExecutor;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import static java.nio.charset.StandardCharsets.UTF_8;
import java.util.function.Consumer;
import java.util.function.Function;

/** A gRPC channel over one peer, and the server for the services added to it: it pumps on its own thread. */
public final class MudChannel extends Channel implements AutoCloseable {
    /** The detail of the reply that ends a server stream and carries no item. A reply with end, OK and no detail is the stream's last item, as the native API sends it. */
    public static final String STREAM_END = "end";
    static final int WEEK = 604800, MAX_ITEMS = 63, INBOX = 1024; // a message's longest life, a stream's items, wires waiting for the pump
    static final int PACE = 32; // replies a second: half what a receiver takes from one sender, so no edge of its second drops one
    static final long PATIENCE = 1_000_000_000L; // how long an item the transport refuses is tried before it is let go
    static final Status EXPIRED = Status.DEADLINE_EXCEEDED.withDescription("deadline exceeded");

    private final Mud.Peer peer;
    private final Consumer<Mud.Outgoing> send;
    private final Consumer<Throwable> fault;
    private final Mud.Udp udp;
    private final int ttl;
    private final Object gate = new Object(); // one thread drives the peer at a time; no gRPC callback runs while it is held, the transport's send alone does
    private final long start = System.nanoTime();
    private final ConcurrentLinkedQueue<Mud.Udp.Datagram> inbox = new ConcurrentLinkedQueue<>();
    private final AtomicInteger waiting = new AtomicInteger();
    private final Map<String, Exchange> pending = new HashMap<>();
    private final Set<Call<?, ?>> open = new HashSet<>();
    private final Map<String, ServerMethodDefinition<?, ?>> methods = new HashMap<>();
    private final Set<String> services = new HashSet<>();
    private final ArrayDeque<String> toListen = new ArrayDeque<>();   // names the peer's full outbox refused, listened on a later round
    private final LinkedHashMap<Integer, ArrayDeque<Answer>> answers = new LinkedHashMap<>(); // by caller, each once, the next to send first
    private final ConcurrentLinkedQueue<Runnable> later = new ConcurrentLinkedQueue<>(); // found under the gate, started after it with no lock held
    private final ExecutorService executor = Executors.newCachedThreadPool(r -> { Thread t = new Thread(r, "mudrpc-call"); t.setDaemon(true); return t; });
    private final ThreadPoolExecutor handlers = new ThreadPoolExecutor(64, 64, 60, TimeUnit.SECONDS, new ArrayBlockingQueue<>(INBOX), r -> { Thread t = new Thread(r, "mudrpc-handler"); t.setDaemon(true); return t; }); // the client's callbacks stay on the executor, so a handler that calls out never waits on itself
    private final ScheduledThreadPoolExecutor timers = new ScheduledThreadPoolExecutor(1, r -> { Thread t = new Thread(r, "mudrpc-timer"); t.setDaemon(true); return t; });
    private final Semaphore wake = new Semaphore(0);
    private final Thread pump;
    private volatile boolean closed;
    private long paceSec = -1, refusedSince;
    private int paced;

    /** Over a {@link Mud.Udp}, which the pump sends through and reads in its own round, and which listens on the peer's name before this returns, and on a service before {@link #addService} does; the socket stays the caller's to close, after the channel. */
    public MudChannel(Mud.Peer peer, Mud.Udp udp) { this(peer, udp, 60, e -> { }); }
    public MudChannel(Mud.Peer peer, Mud.Udp udp, int ttlSecs, Consumer<Throwable> fault) { this(peer, udp::send, udp, ttlSecs, fault); }

    /** Over any transport: {@code send} takes each outgoing item, and what arrives goes to {@link #deliver}. */
    public MudChannel(Mud.Peer peer, Consumer<Mud.Outgoing> send) { this(peer, send, 60, e -> { }); }

    /**
     * The peer is the channel's while it lives: reach it only through {@link #use}, and close the channel before the peer.
     * {@code ttlSecs} is the life of a call made without a deadline; {@code fault} hears every exception the transport
     * throws, and the item it threw on is tried for a second, then let go.
     */
    public MudChannel(Mud.Peer peer, Consumer<Mud.Outgoing> send, int ttlSecs, Consumer<Throwable> fault) { this(peer, send, null, ttlSecs, fault); }

    private MudChannel(Mud.Peer peer, Consumer<Mud.Outgoing> send, Mud.Udp udp, int ttlSecs, Consumer<Throwable> fault) {
        this.peer = peer; this.send = send; this.udp = udp; this.fault = fault; ttl = Math.max(1, Math.min(ttlSecs, WEEK));
        timers.setRemoveOnCancelPolicy(true);
        handlers.allowCoreThreadTimeOut(true);
        pump = new Thread(this::run, "mudrpc");
        pump.setDaemon(true);
        listenNow();
        pump.start();
    }

    /** Under the gate, over the library's socket: the listens at the head of the outbox reach it before the caller goes on; a wire, or a refusal, is the pump's. */
    private void listenNow() {
        if (udp == null) return;
        try { for (Mud.Outgoing o; (o = peer.outgoing(now())) != null && o.listen(); peer.sent()) udp.send(o); } catch (RuntimeException ignored) { }
    }

    /** A wire from the transport, from any thread. Past 1024 waiting for the pump, a wire is dropped, as core would drop a flood. */
    public void deliver(String topic, byte[] wire) {
        if (topic == null || wire == null) throw new NullPointerException("a wire has a topic and bytes");
        if (closed || waiting.incrementAndGet() > INBOX) { waiting.decrementAndGet(); return; }
        inbox.add(new Mud.Udp.Datagram(topic, wire));
        wake.release();
    }

    /** The peer, between rounds of the pump: {@code save}, {@code stage}, {@code rotate}, {@code roster}, {@code counts}. */
    public <T> T use(Function<Mud.Peer, T> act) { synchronized (gate) { try { return act.apply(peer); } finally { wake.release(); } } }

    /** The generated {@code ...ImplBase} or {@code bindService()} result: its methods answer calls through this channel. */
    public MudChannel addService(BindableService service) { return addService(service.bindService()); }

    public MudChannel addService(ServerServiceDefinition service) {
        String name = service.getServiceDescriptor().getName();
        synchronized (gate) {
            for (ServerMethodDefinition<?, ?> m : service.getMethods()) methods.put(name + "/" + m.getMethodDescriptor().getBareMethodName(), m);
            if (services.add(name)) { try { peer.listen(name); listenNow(); } catch (Mud.MudException x) { if (x.code != -2) throw x; toListen.add(name); } }
        }
        wake.release();
        return this;
    }

    @Override public String authority() { return "mudrpc"; }

    @Override public <ReqT, RespT> ClientCall<ReqT, RespT> newCall(MethodDescriptor<ReqT, RespT> method, CallOptions options) { return new Call<>(method, options); }

    /** Stops the pump and ends every call in flight with {@code UNAVAILABLE}; from any thread, a callback or the transport's send included. */
    @Override public void close() {
        synchronized (gate) { shut(Status.UNAVAILABLE.withDescription("the channel is closed")); }
        hand();
        wake.release();
        if (Thread.currentThread() != pump && !Thread.holdsLock(gate)) { try { pump.join(); } catch (InterruptedException e) { Thread.currentThread().interrupt(); } }
        timers.shutdown(); // the call pool stays: a call made after the close is still told it failed, and its idle threads end themselves
    }

    int now() { return (int) ((System.nanoTime() - start) / 1_000_000_000L); }

    /** Under the gate. */
    private void shut(Status s) {
        if (closed) return;
        closed = true;
        for (Call<?, ?> c : new ArrayList<>(open)) c.abort(s);
        answers.clear();
    }

    /** Starts what was found under the gate: each call's queue, already in order, and the tidying it left. No lock is held. */
    private void hand() { for (Runnable r; (r = later.poll()) != null;) r.run(); }

    private void run() {
        while (true) {
            try { wake.tryAcquire(udp == null ? 50 : 10, TimeUnit.MILLISECONDS); } catch (InterruptedException e) { return; }
            wake.drainPermits();
            boolean stop = round();
            hand();
            if (stop) return;
        }
    }

    /** One round under the gate; true when the channel has closed. */
    private boolean round() {
        synchronized (gate) {
            if (closed) return true;
            int now = now();
            try {
                while (!toListen.isEmpty()) { peer.listen(toListen.peek()); toListen.poll(); }
                for (Mud.Udp.Datagram w; (w = inbox.poll()) != null;) {
                    waiting.decrementAndGet();
                    Mud.Incoming e;
                    try { e = peer.deliver(now, w.topic(), w.wire()); } catch (Mud.MudException x) { continue; } // a wire the transport mangled: no verdict to act on
                    land(e);
                }
                if (udp != null) {
                    try { for (int i = 0; i < INBOX; i++) { Mud.Udp.Datagram w = udp.poll(); if (w == null) break; try { land(peer.deliver(now, w.topic(), w.wire())); } catch (Mud.MudException x) { } } }
                    catch (IllegalStateException x) { shut(Status.UNAVAILABLE.withDescription("the socket is closed")); return true; }
                    catch (Mud.MudException x) { tell(x); }
                }
                for (Mud.Incoming e; (e = peer.tick(now)) != null;) land(e);
                answer(now);
                for (Mud.Outgoing o; (o = peer.outgoing(now)) != null;) {
                    try { send.accept(o); }
                    catch (RuntimeException x) {
                        if (udp != null && x instanceof IllegalStateException) { shut(Status.UNAVAILABLE.withDescription("the socket is closed")); return true; }
                        tell(x);
                        long t = System.nanoTime();
                        if (refusedSince == 0) refusedSince = t;
                        if (t - refusedSince < PATIENCE) break; // the next round tries it again; past a second it is let go
                    }
                    refusedSince = 0;
                    peer.sent();
                }
            } catch (Mud.MudException x) {
                if (x.code == -1 || x.code == -4 || x.code == -5) { shut(Status.UNAVAILABLE.withDescription("the peer is closed: " + x.getMessage())); return true; }
            }
            return false;
        }
    }

    private void tell(Throwable t) { try { fault.accept(t); } catch (RuntimeException ignored) { } }

    private void land(Mud.Incoming e) {
        if (e.kind() == Mud.Kind.REPLY || e.kind() == Mud.Kind.EXPIRED) { Exchange x = pending.get(HexFormat.of().formatHex(e.id())); if (x != null) x.land(e); }
        else if (e.kind() == Mud.Kind.CALL) {
            ServerMethodDefinition<?, ?> m = methods.get(e.topic() + "/" + e.method());
            long until = System.nanoTime() + e.ttlSecs() * 1_000_000_000L; // its ttl counts from its arrival
            if (m != null) {
                try { handlers.execute(() -> serve(m, e, until)); } // 64 run at once, 1024 wait
                catch (RejectedExecutionException x) { owe(new Answer(e, 0, Status.Code.RESOURCE_EXHAUSTED.value(), "the server is behind", new byte[0], true, until, null)); }
            }
            else if (services.contains(e.topic())) owe(new Answer(e, 0, Status.Code.UNIMPLEMENTED.value(), "no method " + e.topic() + "/" + e.method(), new byte[0], true, until, null)); // a service this channel does not serve is not its to answer
        }
    }

    /** Under the gate: a reply the server owes, in its caller's line, 1024 at most, sixteen full streams; past 4096 in all the longest line gives way, so one flooding sender cannot crowd out the rest. */
    private void owe(Answer a) {
        ArrayDeque<Answer> line = answers.computeIfAbsent(a.call.sender(), k -> new ArrayDeque<>());
        if (line.size() >= INBOX) return; // a flooding sender: its caller waits out its ttl
        if (answers.values().stream().mapToInt(ArrayDeque::size).sum() >= 4 * INBOX) {
            ArrayDeque<Answer> longest = Collections.max(answers.values(), Comparator.comparingInt(ArrayDeque::size));
            if (longest == line) return;
            longest.pollLast();
        }
        line.add(a);
    }

    /** Under the gate: the server's replies, 32 a second, a turn each: a caller sends one and goes to the back; a full outbox or a spent second leaves the rest for the next round, each until its call's ttl. */
    private void answer(int now) {
        if (now != paceSec) { paceSec = now; paced = 0; }
        long t = System.nanoTime();
        for (Map.Entry<Integer, ArrayDeque<Answer>> e; paced < PACE && (e = answers.pollFirstEntry()) != null;) {
            ArrayDeque<Answer> line = e.getValue();
            Answer a = line.poll();
            if (a != null && t <= a.until) {
                try { peer.reply(now, a.call, a.status, a.payload, a.end, a.seq, clip(a.detail)); paced++; }
                catch (Mud.MudException x) {
                    if (x.code == -2 || x.code == -9) { line.addFirst(a); answers.putFirst(e.getKey(), line); return; }
                    if (x.code == -3 && a.payload.length > 0) { // the call is over: its caller hears why, at once, and its handler is cancelled
                        a.from.killed = true;
                        later.add(() -> a.from.context.cancel(null));
                        line.removeIf(b -> b.from == a.from);
                        line.addFirst(new Answer(a.call, a.seq, Status.Code.RESOURCE_EXHAUSTED.value(), "the reply is larger than a mudRPC wire", new byte[0], true, a.until, a.from));
                    } else tell(x);
                }
            }
            if (!line.isEmpty()) answers.putLast(e.getKey(), line);
        }
    }

    static String clip(String detail) { return detail == null ? "" : detail.length() > 128 ? detail.substring(0, 128) : detail; }

    static Status refused(Mud.MudException e) {
        Status.Code c = e.code == -2 || e.code == -3 || e.code == -9 ? Status.Code.RESOURCE_EXHAUSTED : e.code == -1 ? Status.Code.INVALID_ARGUMENT : Status.Code.INTERNAL;
        return c.toStatus().withDescription(e.getMessage());
    }

    static byte[] bytes(InputStream in) { try (in) { return in.readAllBytes(); } catch (IOException e) { throw new IllegalStateException(e); } }

    /** One reply a server owes, its call's life, from its arrival, the time it may wait for the pace, and the handler's call it is from, if any. */
    private record Answer(Mud.Incoming call, int seq, int status, String detail, byte[] payload, boolean end, long until, Serve<?, ?> from) { }

    /**
     * Runs a call's callbacks one at a time, in the order they were posted, on the executor its options give, or the
     * channel's when that one refuses. {@link #post} only queues, so it is safe under the gate; {@link #kick} starts the
     * queue, outside every lock.
     */
    final class Serial implements Executor {
        private final Executor under;
        private final ArrayDeque<Runnable> queue = new ArrayDeque<>();
        private boolean running;
        Serial(Executor under) { this.under = under; }
        synchronized void post(Runnable r) { queue.add(r); }
        void kick() {
            synchronized (this) { if (running || queue.isEmpty()) return; running = true; }
            try { under.execute(this::drain); } catch (RejectedExecutionException e) { executor.execute(this::drain); }
        }
        @Override public void execute(Runnable r) { post(r); kick(); }
        /** Drains on this thread, a handler's, so the handler runs within the bound. */
        void here() { synchronized (this) { if (running) return; running = true; } drain(); }
        private void drain() {
            while (true) {
                Runnable r;
                synchronized (this) { r = queue.poll(); if (r == null) { running = false; return; } }
                try { r.run(); }
                catch (RuntimeException ignored) { } // each callback below catches its listener's own
                catch (Error e) { synchronized (this) { running = false; } kick(); throw e; }
            }
        }
    }

    /** Under the gate: a callback for a call, in its order, started once the gate is let go. */
    private void post(Serial s, Runnable r) { s.post(r); later.add(s::kick); }

    /** One call in flight: replies in any order, items given in seq order, the end its status. Every method runs under the gate. */
    private abstract static class Exchange {
        private final TreeMap<Integer, Mud.Incoming> early = new TreeMap<>();
        private int next;
        boolean done;
        Runnable forget = () -> { };
        abstract void item(byte[] bytes);
        /** {@code sent}: the server's status, which comes after the items before it; otherwise the channel's own, which cuts them off. */
        abstract void end(Status s, boolean sent);

        void land(Mud.Incoming e) {
            if (e.kind() == Mud.Kind.EXPIRED) { finish(EXPIRED); return; }
            if (e.seq() >= next) early.putIfAbsent(e.seq(), e);
            for (Mud.Incoming r; !done && (r = early.remove(next)) != null;) {
                next++;
                if (r.status() != 0) close(Status.fromCodeValue(r.status()).withDescription(r.detail()), true);
                else if (r.end() && STREAM_END.equals(r.detail())) close(Status.OK, true);
                else { item(r.bytes()); if (r.end()) close(Status.OK, true); }
            }
        }

        void finish(Status s) { close(s, false); }

        void close(Status s, boolean sent) {
            if (done) return;
            done = true;
            early.clear();
            forget.run();
            end(s, sent);
        }
    }

    /**
     * A client call: grpc-java's stubs start it, request, send one message and half-close; the listener hears it on the
     * call's executor. The exchange ends when the server's last reply lands; the call ends when the listener has been
     * told, so a cancel, a deadline or a close still ends it while items wait to be read.
     */
    private final class Call<ReqT, RespT> extends ClientCall<ReqT, RespT> {
        private final MethodDescriptor<ReqT, RespT> method;
        private final CallOptions options;
        private final Serial exec;
        private final ArrayDeque<RespT> ready = new ArrayDeque<>();
        private final List<Runnable> release = new ArrayList<>(); // timers and listeners, let go when the call ends
        private Listener<RespT> listener;
        private byte[] request;
        private int requested;
        private boolean headers, closedOut, started, cancelled, begun;
        private volatile Status failed; // set by a listener's own exception, on the call's executor, before the close it replaces
        private Status end;
        private Context context;
        private final Exchange x = new Exchange() {
            @Override void item(byte[] bytes) {
                RespT m;
                try { m = method.parseResponse(new ByteArrayInputStream(bytes)); } catch (RuntimeException e) { abort(Status.INTERNAL.withDescription("Failed to read message.").withCause(e)); return; }
                ready.add(m);
                flow();
            }
            @Override void end(Status s, boolean sent) { if (end == null) end = s; if (!sent) ready.clear(); flow(); }
        };

        Call(MethodDescriptor<ReqT, RespT> method, CallOptions options) {
            this.method = method; this.options = options;
            exec = new Serial(options.getExecutor() != null ? options.getExecutor() : executor);
        }

        /** Under the gate: the call ends now, whatever the exchange: what waits unread is cut off. */
        void abort(Status s) {
            if (closedOut) return;
            if (!x.done) { x.finish(s); return; }
            end = s;
            ready.clear();
            flow();
        }

        /** Under the gate: hands the listener what it asked for, in order, on its executor, after the gate. */
        private void flow() {
            if (listener == null || closedOut) return;
            Listener<RespT> l = listener;
            if (!headers && (!ready.isEmpty() || end != null && end.isOk())) { headers = true; post(exec, () -> { try { l.onHeaders(new Metadata()); } catch (RuntimeException e) { fail("Failed to read headers", e); } }); }
            while (requested > 0 && !ready.isEmpty()) { RespT m = ready.poll(); requested--; post(exec, () -> { try { l.onMessage(m); } catch (RuntimeException e) { fail("Failed to read message.", e); } }); }
            if (end != null && ready.isEmpty()) {
                closedOut = true;
                open.remove(this);
                for (Runnable r : release) r.run();
                Status s = end;
                post(exec, () -> l.onClose(failed != null ? failed : s, new Metadata()));
            }
        }

        @Override public void start(Listener<RespT> listener, Metadata headers) {
            synchronized (gate) {
                if (started) throw new IllegalStateException("Already started");
                if (cancelled) throw new IllegalStateException("call was cancelled");
                started = true;
                this.listener = listener;
                context = Context.current();
                if (closed) { abort(Status.UNAVAILABLE.withDescription("the channel is closed")); }
                else if (method.getType() != MethodDescriptor.MethodType.UNARY && method.getType() != MethodDescriptor.MethodType.SERVER_STREAMING) abort(Status.UNIMPLEMENTED.withDescription("mudRPC carries no client stream"));
                else if (context.isCancelled()) abort(Contexts.cancelled(context));
                else {
                    open.add(this);
                    Context.CancellationListener cl = c -> { synchronized (gate) { abort(Contexts.cancelled(c)); } hand(); };
                    context.addListener(cl, Runnable::run);
                    Context ctx = context;
                    release.add(() -> later.add(() -> ctx.removeListener(cl)));
                }
            }
            hand();
        }

        /** On the call's executor: the listener threw, so the call ends cancelled with its exception, as grpc-java's does. */
        private void fail(String why, RuntimeException e) {
            if (failed == null) failed = Status.CANCELLED.withDescription(why).withCause(e);
            cancel(why, e);
        }

        @Override public void request(int n) {
            if (n < 0) throw new IllegalArgumentException("Number requested must be non-negative");
            synchronized (gate) { requested = (int) Math.min((long) requested + n, Integer.MAX_VALUE); flow(); }
            hand();
        }

        @Override public void sendMessage(ReqT message) {
            synchronized (gate) {
                if (begun) throw new IllegalStateException("call already half-closed");
                if (request != null) throw new IllegalStateException("one request a call: mudRPC carries no client stream");
                if (!closedOut) request = bytes(method.streamRequest(message));
            }
        }

        @Override public void halfClose() {
            synchronized (gate) {
                if (begun) throw new IllegalStateException("call already half-closed");
                begun = true;
                if (!closedOut) begin();
            }
            hand();
            wake.release();
        }

        /** Under the gate: sends the request and files the exchange; a refusal, a passed deadline or a closed channel ends it, sending nothing. */
        private void begin() {
            if (request == null) { abort(Status.INTERNAL.withDescription("no request")); return; }
            Deadline d = earliest(options.getDeadline(), context.getDeadline());
            long left = d == null ? Long.MAX_VALUE : d.timeRemaining(TimeUnit.NANOSECONDS);
            if (left <= 0) { abort(EXPIRED); return; }
            String service = method.getServiceName();
            if (!utf8(service) || service.isEmpty() || service.getBytes(UTF_8).length > 64 || !utf8(method.getBareMethodName())) { abort(refused(new Mud.MudException(-1))); return; }
            if (closed) { abort(Status.UNAVAILABLE.withDescription("the channel is closed")); return; }
            int life = d == null ? ttl : (int) Math.min((left + 999_999_999L) / 1_000_000_000L + 1, WEEK); // a second over: the clock is whole seconds, the timer below is the deadline
            String key;
            try { key = HexFormat.of().formatHex(peer.call(now(), method.getServiceName(), method.getBareMethodName(), request, life)); }
            catch (Mud.MudException e) { abort(refused(e)); return; }
            pending.put(key, x);
            x.forget = () -> pending.remove(key);
            if (d != null && left <= TimeUnit.SECONDS.toNanos(WEEK)) {
                ScheduledFuture<?> f = timers.schedule(() -> { synchronized (gate) { abort(EXPIRED); } hand(); }, left, TimeUnit.NANOSECONDS);
                release.add(() -> f.cancel(false));
            }
        }

        @Override public void cancel(String message, Throwable cause) {
            synchronized (gate) {
                Status s = Status.CANCELLED;
                if (message != null) s = s.withDescription(message);
                if (cause != null) s = s.withCause(cause);
                if (message == null && cause == null) s = s.withDescription("Cancelled without a message or cause");
                cancelled = true;
                abort(s);
            }
            hand();
        }

        @Override public boolean isReady() { return !closedOut; }
        @Override public Attributes getAttributes() { return Attributes.EMPTY; }
    }

    /** UTF-8, which a lone surrogate is not. */
    static boolean utf8(String s) { return s != null && UTF_8.newEncoder().canEncode(s); }

    static Deadline earliest(Deadline a, Deadline b) { return a == null ? b : b == null ? a : a.minimum(b); }

    /** The caller's roster number, as a server sees it in {@code Grpc.TRANSPORT_ATTR_REMOTE_ADDR}: proven per device under a signed fleet. */
    public static final class Sender extends SocketAddress {
        private static final long serialVersionUID = 1L;
        public final int number;
        Sender(int number) { this.number = number; }
        @Override public String toString() { return "mudrpc:" + number; }
    }

    /** Runs a call's handler as grpc-java's server would: start it, give it the request, half-close; it answers through the call. */
    private <ReqT, RespT> void serve(ServerMethodDefinition<ReqT, RespT> m, Mud.Incoming in, long until) {
        long left = until - System.nanoTime();
        if (left <= 0) return; // its caller has given up: a server behind sheds the call, not the next one
        MethodDescriptor<ReqT, RespT> d = m.getMethodDescriptor();
        Serve<ReqT, RespT> call = new Serve<>(d, in, until);
        if (d.getType() != MethodDescriptor.MethodType.UNARY && d.getType() != MethodDescriptor.MethodType.SERVER_STREAMING) { call.close(Status.UNIMPLEMENTED.withDescription("mudRPC carries no client stream"), new Metadata()); return; }
        ReqT req;
        try { req = d.parseRequest(new ByteArrayInputStream(in.bytes())); }
        catch (RuntimeException e) { call.close(Status.INTERNAL.withDescription("Failed to deserialize request message."), new Metadata()); return; }
        Context.CancellableContext ctx = Context.current().withDeadlineAfter(left, TimeUnit.NANOSECONDS, timers);
        call.context = ctx;
        call.exec.post(ctx.wrap(() -> {
            try {
                ServerCall.Listener<ReqT> l = m.getServerCallHandler().startCall(call, new Metadata());
                call.listener = l;
                ctx.addListener(c -> call.ended(), Runnable::run);
                l.onReady();
                l.onMessage(req);
                l.onHalfClose();
            } catch (RuntimeException e) { call.close(Status.UNKNOWN.withDescription("Application error processing RPC").withCause(e), new Metadata()); }
        }));
        call.exec.here();
    }

    /** The server's side of one call: what the handler sends becomes the replies the pump paces out. */
    private final class Serve<ReqT, RespT> extends ServerCall<ReqT, RespT> {
        private final MethodDescriptor<ReqT, RespT> method;
        private final Mud.Incoming in;
        private final long until;
        final Serial exec = new Serial(executor);
        Context.CancellableContext context;
        Listener<ReqT> listener;
        private int seq;
        private byte[] unary;
        private boolean closedCall;
        boolean killed; // its reply was too large for the wire: the call is over, its writes go nowhere

        Serve(MethodDescriptor<ReqT, RespT> method, Mud.Incoming in, long until) { this.method = method; this.in = in; this.until = until; }

        @Override public void request(int n) { }
        @Override public void sendHeaders(Metadata headers) { }

        @Override public void sendMessage(RespT message) {
            byte[] b = bytes(method.streamResponse(message));
            synchronized (gate) {
                if (closedCall) throw new IllegalStateException("call is closed");
                if (closed || killed) return;
                if (method.getType() == MethodDescriptor.MethodType.UNARY) { unary = b; return; }
                if (seq >= MAX_ITEMS) throw Status.RESOURCE_EXHAUSTED.withDescription("a stream carries at most " + MAX_ITEMS + " items").asRuntimeException();
                owe(new Answer(in, seq++, 0, "", b, false, until, this));
            }
            wake.release();
        }

        @Override public void close(Status status, Metadata trailers) {
            synchronized (gate) {
                if (closedCall) throw new IllegalStateException("call already closed");
                closedCall = true;
                if (!closed && !killed) {
                    if (!status.isOk()) owe(new Answer(in, seq, status.getCode().value(), status.getDescription(), new byte[0], true, until, this));
                    else if (method.getType() == MethodDescriptor.MethodType.UNARY) owe(unary != null ? new Answer(in, 0, 0, "", unary, true, until, this) : new Answer(in, 0, Status.Code.INTERNAL.value(), "No value received for unary call", new byte[0], true, until, this));
                    else owe(new Answer(in, seq, 0, STREAM_END, new byte[0], true, until, this));
                }
            }
            wake.release();
            Listener<ReqT> l = listener;
            if (l != null && !killed) exec.execute(l::onComplete);
            if (context != null) context.cancel(null); // grpc-java's server cancels a call's context when the call is done
        }

        /** The context ended before the handler closed the call: its ttl passed, or the channel closed. */
        void ended() {
            boolean early;
            synchronized (gate) { early = !closedCall; }
            Listener<ReqT> l = listener;
            if (early && l != null) exec.execute(l::onCancel);
        }

        @Override public boolean isCancelled() { synchronized (gate) { return !closedCall && (closed || killed || context != null && context.isCancelled()); } }
        @Override public boolean isReady() { return !closedCall; }
        @Override public MethodDescriptor<ReqT, RespT> getMethodDescriptor() { return method; }
        @Override public String getAuthority() { return "mudrpc"; }
        @Override public Attributes getAttributes() { return Attributes.newBuilder().set(Grpc.TRANSPORT_ATTR_REMOTE_ADDR, new Sender(in.sender())).build(); }
    }

    /** grpc-java's status for a cancelled context: its cause if it has one. */
    static final class Contexts {
        static Status cancelled(Context c) {
            Throwable t = c.cancellationCause();
            if (t == null) return Status.CANCELLED.withDescription("io.grpc.Context was cancelled without error");
            if (t instanceof java.util.concurrent.TimeoutException) return Status.DEADLINE_EXCEEDED.withDescription(t.getMessage()).withCause(t);
            Status s = Status.fromThrowable(t);
            return s.getCode() == Status.Code.UNKNOWN && s.getCause() == t ? Status.CANCELLED.withDescription("Context cancelled").withCause(t) : s.withCause(t);
        }
    }
}
