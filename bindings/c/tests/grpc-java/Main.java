// The gRPC drop-in under test: the stubs and service base grpc-java's plugin generated from drone.proto, written as
// grpc-java code is, over two mudRPC peers in this process. Building the channels is the only mudRPC code above them.
import io.grpc.CallOptions;
import io.grpc.Context;
import io.grpc.Contexts;
import io.grpc.Grpc;
import io.grpc.Metadata;
import io.grpc.MethodDescriptor;
import io.grpc.ServerCall;
import io.grpc.ServerCallHandler;
import io.grpc.ServerInterceptor;
import io.grpc.ServerInterceptors;
import io.grpc.Status;
import io.grpc.StatusRuntimeException;
import io.grpc.protobuf.ProtoUtils;
import io.grpc.stub.ClientCalls;
import io.grpc.stub.StreamObserver;
import java.net.DatagramSocket;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentLinkedQueue;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import mudrpc.Mud;
import mudrpc.MudChannel;
import test.ArmReply;
import test.ArmRequest;
import test.DroneGrpc;
import test.RoverGrpc;
import test.Sample;
import test.TelemetryRequest;

public final class Main {
    static final byte[] KEY = new byte[32];
    static { for (int i = 0; i < 32; i++) KEY[i] = (byte) (i + 1); }
    static final Context.Key<String> CALLER = Context.key("caller");
    static final java.util.concurrent.atomic.AtomicReference<CountDown> HEARD = new java.util.concurrent.atomic.AtomicReference<>();
    static volatile boolean stopped;
    static final java.util.concurrent.CountDownLatch HOLD = new java.util.concurrent.CountDownLatch(1);
    static final AtomicInteger HELD = new AtomicInteger();

    public static void main(String[] args) throws Exception {
        Mud.Peer gcsPeer = new Mud.Peer("gcs", KEY, 1, 1), dronePeer = new Mud.Peer("drone", KEY, 2, 1);
        Link link = new Link();
        MudChannel gcs = new MudChannel(gcsPeer, o -> link.send(0, o)), drone = new MudChannel(dronePeer, o -> link.send(1, o));
        link.ends = new MudChannel[] { gcs, drone };
        drone.addService(ServerInterceptors.intercept(new DroneService(), new Caller()));

        // From here, the code a grpc-java product already has.
        DroneGrpc.DroneBlockingStub client = DroneGrpc.newBlockingStub(gcs);
        ArmReply r = client.arm(ArmRequest.newBuilder().setId(7).build());
        say("arm: armed " + r.getArmed() + " id " + r.getId() + " by " + r.getBy());
        say("future: armed " + DroneGrpc.newFutureStub(gcs).arm(ArmRequest.newBuilder().setId(8).build()).get().getArmed());
        fails("refused", () -> client.arm(ArmRequest.newBuilder().setId(0).build()));
        fails("handler throws", () -> client.arm(ArmRequest.newBuilder().setId(13).build()));
        MethodDescriptor<ArmRequest, ArmReply> fly = MethodDescriptor.<ArmRequest, ArmReply>newBuilder().setType(MethodDescriptor.MethodType.UNARY).setFullMethodName("test.Drone/Fly")
            .setRequestMarshaller(ProtoUtils.marshaller(ArmRequest.getDefaultInstance())).setResponseMarshaller(ProtoUtils.marshaller(ArmReply.getDefaultInstance())).build();
        fails("unknown method", () -> ClientCalls.blockingUnaryCall(gcs, fly, CallOptions.DEFAULT, ArmRequest.newBuilder().setId(1).build()));
        MethodDescriptor<ArmRequest, ArmReply> badName = fly.toBuilder().setFullMethodName("test.Drone/\uD800").build();
        say("a method name that is not UTF-8: " + told(() -> ClientCalls.blockingUnaryCall(gcs, badName, CallOptions.DEFAULT, ArmRequest.newBuilder().setId(1).build())) + ", then armed " + client.arm(ArmRequest.newBuilder().setId(1).build()).getArmed());

        link.hold = 6; // five items and the stream's end, delivered newest first
        say("telemetry reversed: " + read(client.telemetry(TelemetryRequest.newBuilder().setCount(5).build())));
        say("telemetry empty: " + read(client.telemetry(TelemetryRequest.newBuilder().setCount(0).build())));
        fails("telemetry fails", () -> read(client.telemetry(TelemetryRequest.newBuilder().setCount(5).setFailAfter(2).build()), "telemetry fails"));
        CompletableFuture<String> upload = new CompletableFuture<>();
        DroneGrpc.newStub(gcs).upload(observer(upload)).onNext(Sample.newBuilder().setSeq(1).build());
        say("upload: " + upload.get(5, TimeUnit.SECONDS));
        fails("too large", () -> client.arm(ArmRequest.newBuilder().setId(1).setNote(com.google.protobuf.ByteString.copyFrom(new byte[1000])).build()));

        link.cut = true;
        long t0 = System.nanoTime();
        fails("deadline", () -> client.withDeadlineAfter(2, TimeUnit.SECONDS).arm(ArmRequest.newBuilder().setId(9).build()));
        double took = (System.nanoTime() - t0) / 1e9;
        say("deadline in time: " + (took >= 2.0 && took < 3.5));
        Context.CancellableContext cancel = Context.current().withCancellation();
        new Thread(() -> { sleep(300); cancel.cancel(null); }).start();
        fails("cancelled", () -> cancel.call(() -> client.arm(ArmRequest.newBuilder().setId(10).build())));
        var future = DroneGrpc.newFutureStub(gcs).arm(ArmRequest.newBuilder().setId(12).build());
        future.cancel(true);
        fails("future cancelled", () -> { try { future.get(); return null; } catch (java.util.concurrent.CancellationException e) { throw Status.CANCELLED.withDescription("the future").asRuntimeException(); } });
        link.cut = false;
        say("after the cut: armed " + client.arm(ArmRequest.newBuilder().setId(11).build()).getArmed());

        fails("past the deadline", () -> client.withDeadline(io.grpc.Deadline.after(-1, TimeUnit.SECONDS)).arm(ArmRequest.newBuilder().setId(1).build()));
        MethodDescriptor<ArmRequest, ArmReply> bad = fly.toBuilder().setFullMethodName("test.Drone/Arm").setRequestMarshaller(new Junk()).build();
        fails("undecodable request", () -> ClientCalls.blockingUnaryCall(gcs, bad, CallOptions.DEFAULT, ArmRequest.getDefaultInstance()));

        int before = gcs.use(Mud.Peer::pending);
        Context.CancellableContext gone = Context.current().withCancellation();
        gone.cancel(null);
        for (int i = 0; i < 16; i++) quiet(() -> gone.call(() -> client.arm(ArmRequest.newBuilder().setId(1).build())));
        say("cancelled before sending, 16 times: pending " + (gcs.use(Mud.Peer::pending) - before) + " more, then armed " + client.arm(ArmRequest.newBuilder().setId(15).build()).getArmed());

        int finished = 0;
        for (int round = 0; round < 30; round++) {
            Context.CancellableContext c = Context.current().withCancellation();
            var f = c.call(() -> DroneGrpc.newFutureStub(gcs).arm(ArmRequest.newBuilder().setId(16).build()));
            Thread canceller = new Thread(() -> c.cancel(null));
            gcs.use(p -> { sleep(30); canceller.start(); sleep(20); return null; }); // the reply waits in the inbox while the cancel waits for the pump
            try { f.get(3, TimeUnit.SECONDS); finished++; } catch (ExecutionException | java.util.concurrent.CancellationException e) { finished++; } catch (java.util.concurrent.TimeoutException e) { }
            canceller.join();
        }
        say("cancel racing a reply: " + finished + " of 30 finished");

        AtomicInteger answered = new AtomicInteger();
        Thread calls = new Thread(() -> { for (int i = 0; i < 50; i++) { if (client.arm(ArmRequest.newBuilder().setId(1).build()).getArmed()) answered.incrementAndGet(); sleep(40); } }); // a client keeps under 32 calls a second
        calls.start();
        int saves = 0;
        while (calls.isAlive()) { gcs.use(Mud.Peer::save); saves++; sleep(1); }
        say("saved while calling: " + answered.get() + " calls answered, saves " + (saves > 0 ? "made" : "none"));

        say("two full streams back to back: " + read(client.telemetry(TelemetryRequest.newBuilder().setCount(63).build())).split(" ").length + " " + read(client.telemetry(TelemetryRequest.newBuilder().setCount(63).build())).split(" ").length);
        fails("a 64th item", () -> read(client.telemetry(TelemetryRequest.newBuilder().setCount(64).build())));
        List<String> seen = new ArrayList<>();
        String big = told(() -> { client.telemetry(TelemetryRequest.newBuilder().setCount(40).setUnknownFields(field(3, f -> f.addVarint(2))).build()).forEachRemaining(m -> seen.add(Integer.toString(m.getSeq()))); return null; });
        sleep(100);
        say("an item past the wire mid-stream: " + String.join(" ", seen) + ", " + big + ", its handler stopped: " + stopped);

        Mud.Peer lonePeer = new Mud.Peer("lone", KEY, 9, 1);
        try (MudChannel lone = new MudChannel(lonePeer, o -> { })) {
            lonePeer.close();
            sleep(200); // the pump's next round finds the peer gone and closes the channel
            fails("the peer closed first", () -> DroneGrpc.newBlockingStub(lone).withDeadlineAfter(2, TimeUnit.SECONDS).arm(ArmRequest.newBuilder().setId(1).build()));
        }

        int free = 16 - gcs.use(Mud.Peer::pending); // a call cancelled after sending keeps its slot until its ttl
        List<CompletableFuture<String>> unknown = new ArrayList<>();
        for (int i = 0; i < free; i++) {
            CompletableFuture<String> u = new CompletableFuture<>();
            ClientCalls.asyncUnaryCall(gcs.newCall(fly, CallOptions.DEFAULT.withDeadlineAfter(5, TimeUnit.SECONDS)), ArmRequest.newBuilder().setId(1).build(), observer(u));
            unknown.add(u);
        }
        boolean all = true;
        for (CompletableFuture<String> u : unknown) all &= u.get(10, TimeUnit.SECONDS).startsWith("UNIMPLEMENTED");
        say("unknown method, every free slot at once: " + (all ? "all UNIMPLEMENTED" : "not all"));
        fails("null description", () -> client.arm(ArmRequest.newBuilder().setId(22).build()));
        say("a null wire: " + thrown(() -> gcs.deliver("gcs", null)));
        link.cut = true;
        DroneGrpc.newFutureStub(gcs).withDeadlineAfter(8, TimeUnit.DAYS).arm(ArmRequest.newBuilder().setId(1).build());
        sleep(200);
        link.cut = false;
        say("a deadline eight days out: ttl " + link.lastTtl);
        gcs.use(p -> { for (int i = 0; i < 5000; i++) gcs.deliver("gcs", new byte[40]); return null; });
        say("five thousand junk wires: then armed " + client.arm(ArmRequest.newBuilder().setId(1).build()).getArmed());

        Link link3 = new Link();
        link3.refuseListen = true;
        AtomicInteger faults = new AtomicInteger();
        MudChannel gcs3 = new MudChannel(new Mud.Peer("gcs3", KEY, 7, 1), o -> link3.send(0, o), 60, e -> faults.incrementAndGet());
        MudChannel drone3 = new MudChannel(new Mud.Peer("drone3", KEY, 8, 1), o -> link3.send(1, o));
        link3.ends = new MudChannel[] { gcs3, drone3 };
        drone3.addService(ServerInterceptors.intercept(new DroneService(), new Caller()));
        say("a transport refusing a listen: armed " + DroneGrpc.newBlockingStub(gcs3).withDeadlineAfter(10, TimeUnit.SECONDS).arm(ArmRequest.newBuilder().setId(1).build()).getArmed() + ", faults heard " + (faults.get() > 0 ? "yes" : "no"));

        MudChannel[] selfish = new MudChannel[1];
        selfish[0] = new MudChannel(new Mud.Peer("selfish", KEY, 10, 1), o -> selfish[0].close());
        Thread stopper = new Thread(() -> { sleep(300); selfish[0].close(); });
        stopper.start();
        stopper.join(3000);
        say("closed from its own send: " + (stopper.isAlive() ? "hangs" : "returns"));

        Mud.Udp[] pair = udpPair();
        Mud.Udp gu = pair[0], du = pair[1];
        int pd = du.port();
        MudChannel gcsU = new MudChannel(new Mud.Peer("gcsu", KEY, 11, 1), gu), droneU = new MudChannel(new Mud.Peer("droneu", KEY, 12, 1), du);
        droneU.addService(ServerInterceptors.intercept(new DroneService(), new Caller()));
        DroneGrpc.DroneBlockingStub overUdp = DroneGrpc.newBlockingStub(gcsU).withDeadlineAfter(5, TimeUnit.SECONDS);
        say("over UDP: armed " + overUdp.arm(ArmRequest.newBuilder().setId(1).build()).getArmed() + ", telemetry " + read(DroneGrpc.newBlockingStub(gcsU).telemetry(TelemetryRequest.newBuilder().setCount(5).build())));
        int atOnce = 0;
        for (int i = 0; i < 20; i++) { // a server called the moment its service is added: its socket already listens
            Mud.Udp[] fresh = udpPair();
            Mud.Peer cp = new Mud.Peer("gcsf", KEY, 24, 1), sp = new Mud.Peer("dronef", KEY, 25, 1);
            try (MudChannel c = new MudChannel(cp, fresh[0]); MudChannel s = new MudChannel(sp, fresh[1])) {
                s.addService(ServerInterceptors.intercept(new DroneService(), new Caller()));
                if (DroneGrpc.newBlockingStub(c).withDeadlineAfter(5, TimeUnit.SECONDS).arm(ArmRequest.newBuilder().setId(1).build()).getArmed()) atOnce++;
            } catch (StatusRuntimeException e) { }
            cp.close(); sp.close(); fresh[0].close(); fresh[1].close();
        }
        say("twenty servers called at once: " + atOnce + " of 20 armed");
        Mud.Udp lostSocket = new Mud.Udp("127.0.0.1:0", "127.0.0.1:" + pd);
        try (MudChannel lost = new MudChannel(new Mud.Peer("lost", KEY, 13, 1), lostSocket)) {
            lostSocket.close();
            sleep(100);
            fails("its socket closed", () -> DroneGrpc.newBlockingStub(lost).withDeadlineAfter(2, TimeUnit.SECONDS).arm(ArmRequest.newBuilder().setId(1).build()));
        }
        say("a public bind: " + thrown(() -> new Mud.Udp("0.0.0.0:0", "127.0.0.1:9")) + ", a public peer: " + thrown(() -> new Mud.Udp("127.0.0.1:0", "8.8.8.8:53")) + ", this library public: " + Mud.Udp.internet());

        fails("a reply past the wire", () -> client.arm(ArmRequest.newBuilder().setId(23).build()));
        say("request(-1): " + thrown(() -> gcs.newCall(fly, CallOptions.DEFAULT).request(-1)));
        var early = gcs.newCall(fly, CallOptions.DEFAULT);
        early.cancel("early", null);
        say("start after cancel: " + thrown(() -> early.start(new io.grpc.ClientCall.Listener<>() { }, new Metadata())));
        CompletableFuture<String> throwing = new CompletableFuture<>();
        var call = gcs.newCall(DroneGrpc.getArmMethod(), CallOptions.DEFAULT);
        call.start(new io.grpc.ClientCall.Listener<ArmReply>() {
            @Override public void onMessage(ArmReply m) { throw new IllegalStateException("the listener's own bug"); }
            @Override public void onClose(Status s, Metadata t) { throwing.complete(s.getCode() + " " + s.getDescription()); }
        }, new Metadata());
        call.request(1); call.sendMessage(ArmRequest.newBuilder().setId(1).build()); call.halfClose();
        say("a listener that throws: " + throwing.get(5, TimeUnit.SECONDS));
        MudChannel shut = new MudChannel(new Mud.Peer("shut", KEY, 14, 1), o -> { });
        shut.close();
        fails("a call after close", () -> DroneGrpc.newBlockingStub(shut).arm(ArmRequest.newBuilder().setId(1).build()));
        MudChannel inner = new MudChannel(new Mud.Peer("inner", KEY, 15, 1), o -> { });
        CountDown closedInside = new CountDown();
        ClientCalls.asyncClientStreamingCall(inner.newCall(DroneGrpc.getUploadMethod(), CallOptions.DEFAULT.withExecutor(Runnable::run)), new StreamObserver<ArmReply>() {
            @Override public void onNext(ArmReply v) { }
            @Override public void onError(Throwable t) { inner.close(); closedInside.done(); }
            @Override public void onCompleted() { }
        });
        say("closed inside its own callback: " + (closedInside.await() ? "returns" : "hangs"));

        link.cut = true;
        CountDown directClosed = new CountDown();
        MudChannel direct = new MudChannel(new Mud.Peer("direct", KEY, 16, 1), o -> { });
        DroneGrpc.newStub(direct).withExecutor(Runnable::run).withDeadlineAfter(500, TimeUnit.MILLISECONDS).arm(ArmRequest.newBuilder().setId(1).build(), new StreamObserver<ArmReply>() {
            @Override public void onNext(ArmReply v) { }
            @Override public void onError(Throwable t) { direct.close(); directClosed.done(); }
            @Override public void onCompleted() { }
        });
        say("closed from a direct executor's callback: " + (directClosed.await() ? "returns" : "hangs"));
        link.cut = false;

        CompletableFuture<String> manual = new CompletableFuture<>();
        var flowCall = gcs.newCall(DroneGrpc.getTelemetryMethod(), CallOptions.DEFAULT);
        AtomicInteger got = new AtomicInteger();
        flowCall.start(new io.grpc.ClientCall.Listener<Sample>() {
            @Override public void onMessage(Sample m) { got.incrementAndGet(); }
            @Override public void onClose(Status s, Metadata t) { manual.complete(s.getCode() + " after " + got.get()); }
        }, new Metadata());
        flowCall.request(1); flowCall.sendMessage(TelemetryRequest.newBuilder().setCount(5).build()); flowCall.halfClose();
        sleep(500); // every item has landed; one has been asked for
        flowCall.cancel("enough", null);
        say("cancelled with items unread: " + manual.get(5, TimeUnit.SECONDS));
        long slow = System.nanoTime();
        fails("a slow reader past its deadline", () -> { Iterator<Sample> it = client.withDeadlineAfter(1, TimeUnit.SECONDS).telemetry(TelemetryRequest.newBuilder().setCount(5).build()); while (it.hasNext()) { it.next(); sleep(600); } return null; });
        say("the slow reader's deadline in time: " + ((System.nanoTime() - slow) / 1e9 < 2.0));

        var dead = java.util.concurrent.Executors.newSingleThreadExecutor();
        dead.shutdown();
        CompletableFuture<String> rejected = new CompletableFuture<>();
        DroneGrpc.newStub(gcs).withExecutor(dead).arm(ArmRequest.newBuilder().setId(1).build(), observer(rejected));
        say("an executor that refuses: " + rejected.get(5, TimeUnit.SECONDS) + ", then armed " + client.arm(ArmRequest.newBuilder().setId(1).build()).getArmed());

        var twice = gcs.newCall(DroneGrpc.getArmMethod(), CallOptions.DEFAULT);
        twice.start(new io.grpc.ClientCall.Listener<ArmReply>() { }, new Metadata());
        twice.request(1); twice.sendMessage(ArmRequest.newBuilder().setId(1).build()); twice.halfClose();
        say("a second halfClose: " + thrown(twice::halfClose));

        Link slowLink = new Link();
        slowLink.refuseFor = System.nanoTime() + 300_000_000L;
        MudChannel gcs4 = new MudChannel(new Mud.Peer("gcs4", KEY, 17, 1), o -> slowLink.send(0, o)), drone4 = new MudChannel(new Mud.Peer("drone4", KEY, 18, 1), o -> slowLink.send(1, o));
        slowLink.ends = new MudChannel[] { gcs4, drone4 };
        drone4.addService(ServerInterceptors.intercept(new DroneService(), new Caller()));
        say("a transport refusing for 300 ms: armed " + DroneGrpc.newBlockingStub(gcs4).withDeadlineAfter(5, TimeUnit.SECONDS).arm(ArmRequest.newBuilder().setId(1).build()).getArmed());

        HEARD.set(new CountDown());
        fails("a server that never answers", () -> client.withDeadlineAfter(1, TimeUnit.SECONDS).arm(ArmRequest.newBuilder().setId(24).build()));
        say("the server heard its ttl pass: " + HEARD.get().await5());

        Mud.Udp[] three = null;
        for (int i = 0; three == null; i++) {
            int[] ps = { freePort(), freePort(), freePort() };
            try { three = java.util.stream.IntStream.of(ps).mapToObj(p -> new Mud.Udp("127.0.0.1:" + p, java.util.stream.IntStream.of(ps).filter(q -> q != p).mapToObj(q -> "127.0.0.1:" + q).toArray(String[]::new))).toArray(Mud.Udp[]::new); }
            catch (Mud.MudException e) { if (e.code != -2 || i >= 50) throw e; }
        }
        MudChannel gT = new MudChannel(new Mud.Peer("gcst", KEY, 21, 1), three[0]), dT = new MudChannel(new Mud.Peer("dronet", KEY, 22, 1), three[1]), iT = new MudChannel(new Mud.Peer("idlet", KEY, 23, 1), three[2]);
        dT.addService(ServerInterceptors.intercept(new DroneService(), new Caller()));
        int armedT = 0;
        for (int i = 0; i < 10; i++) if (DroneGrpc.newBlockingStub(gT).withDeadlineAfter(5, TimeUnit.SECONDS).arm(ArmRequest.newBuilder().setId(1).build()).getArmed()) armedT++;
        say("three on UDP, one serving nothing: " + armedT + " of 10 armed");
        for (MudChannel c : new MudChannel[] { gT, dT, iT }) c.close();

        for (int i = 0; i < 5; i++) ask(drone, "held" + i, 200 + i, 13, "Arm", ArmRequest.newBuilder().setId(27).build().toByteArray()); // 65 handlers that wait on a latch
        sleep(300);
        int held = HELD.get();
        HOLD.countDown();
        for (int i = 0; i < 100 && HELD.get() < 65; i++) sleep(20);
        say("65 calls on 64 handlers: " + held + " running, then " + HELD.get());

        // A native peer answers the Rover service with the plain API: a stream ends on its last item, and a refusal has a detail.
        Native nat = new Native(new Mud.Peer("rover", KEY, 3, 1));
        Link link2 = new Link();
        MudChannel gcs2 = new MudChannel(new Mud.Peer("gcs2", KEY, 4, 1), o -> link2.send(0, o));
        link2.ends = new MudChannel[] { gcs2 };
        link2.nat = nat;
        nat.back = gcs2;
        RoverGrpc.RoverBlockingStub rover = RoverGrpc.newBlockingStub(gcs2);
        say("native stream: " + read(rover.telemetry(TelemetryRequest.newBuilder().setCount(3).build())));
        fails("native refusal", () -> rover.arm(ArmRequest.newBuilder().setId(1).build()));
        nat.stop();

        for (int i = 0; i < 48; i++) ask(drone, "crowd" + i, 100 + i, 1, "Telemetry", TelemetryRequest.newBuilder().setCount(20).build().toByteArray()); // 48 callers owed a stream each, past the 32 a second: the next caller waits its turn, not behind them all
        sleep(300);
        boolean armed;
        try { armed = client.withDeadlineAfter(3, TimeUnit.SECONDS).arm(ArmRequest.newBuilder().setId(1).build()).getArmed(); } catch (StatusRuntimeException e) { armed = false; }
        say("a caller behind 48 owed a stream each: armed " + armed);
        for (MudChannel c : new MudChannel[] { gcs, drone, gcs2, gcs3, drone3, gcsU, droneU }) c.close();
        gu.close(); du.close();
        System.exit(0);
    }

    static void say(String s) { System.out.println(s); }
    static void sleep(long ms) { try { Thread.sleep(ms); } catch (InterruptedException e) { Thread.currentThread().interrupt(); } }
    /** Two sockets on loopback that list each other: ports chosen free, chosen again while another test takes one between the choosing and the binding. */
    static Mud.Udp[] udpPair() throws Exception {
        for (int i = 0; ; i++) {
            int a = freePort(), b = freePort();
            Mud.Udp x = null;
            try { x = new Mud.Udp("127.0.0.1:" + a, "127.0.0.1:" + b); return new Mud.Udp[] { x, new Mud.Udp("127.0.0.1:" + b, "127.0.0.1:" + a) }; }
            catch (Mud.MudException e) { if (x != null) x.close(); if (e.code != -2 || i >= 50) throw e; }
        }
    }

    static int freePort() throws Exception { try (DatagramSocket s = new DatagramSocket(0, java.net.InetAddress.getLoopbackAddress())) { return s.getLocalPort(); } }

    interface Call { Object run() throws Exception; }
    static void fails(String what, Call f) { say(what + ": " + told(f)); }
    static String told(Call f) {
        try { f.run(); return "no error"; }
        catch (StatusRuntimeException e) { return e.getStatus().getCode() + " " + e.getStatus().getDescription(); }
        catch (Exception e) { return e.getClass().getSimpleName(); }
    }
    /** A message's field this drone.proto leaves out, as the C++ test's proto has it. */
    static com.google.protobuf.UnknownFieldSet field(int n, java.util.function.Consumer<com.google.protobuf.UnknownFieldSet.Field.Builder> f) {
        var b = com.google.protobuf.UnknownFieldSet.Field.newBuilder();
        f.accept(b);
        return com.google.protobuf.UnknownFieldSet.newBuilder().addField(n, b.build()).build();
    }
    /** A native caller's calls on test.Drone, its wires handed straight to the server. */
    static void ask(MudChannel to, String name, int sender, int calls, String method, byte[] request) {
        Mud.Peer p = new Mud.Peer(name, KEY, sender, 1);
        for (int i = 0; i < calls; i++) p.call(0, "test.Drone", method, request, 5);
        for (Mud.Outgoing o; (o = p.outgoing(0)) != null; p.sent()) if (!o.listen()) to.deliver(o.topic(), o.wire());
    }
    static void quiet(Call f) { try { f.run(); } catch (Exception e) { } }
    static String thrown(Runnable f) { try { f.run(); return "no error"; } catch (Exception e) { return e.getClass().getSimpleName(); } }

    static String read(Iterator<Sample> it) { return read(it, null); }
    static String read(Iterator<Sample> it, String partial) {
        List<String> seen = new ArrayList<>();
        try { while (it.hasNext()) seen.add(Integer.toString(it.next().getSeq())); }
        catch (StatusRuntimeException e) { if (partial != null) say(partial + " after: " + String.join(" ", seen)); throw e; }
        return String.join(" ", seen);
    }

    static <T> StreamObserver<T> observer(CompletableFuture<String> done) {
        return new StreamObserver<>() {
            @Override public void onNext(T v) { }
            @Override public void onError(Throwable t) { Status s = Status.fromThrowable(t); done.complete(s.getCode() + " " + s.getDescription()); }
            @Override public void onCompleted() { done.complete("OK"); }
        };
    }

    static final class CountDown {
        final java.util.concurrent.CountDownLatch latch = new java.util.concurrent.CountDownLatch(1);
        void done() { latch.countDown(); }
        boolean await() throws InterruptedException { return latch.await(3, TimeUnit.SECONDS); }
        boolean await5() throws InterruptedException { return latch.await(5, TimeUnit.SECONDS); }
    }

    /** A request marshaller that writes bytes no ArmRequest can be. */
    static final class Junk implements MethodDescriptor.Marshaller<ArmRequest> {
        @Override public java.io.InputStream stream(ArmRequest v) { return new java.io.ByteArrayInputStream(new byte[] { (byte) 0xff }); }
        @Override public ArmRequest parse(java.io.InputStream in) { throw new UnsupportedOperationException(); }
    }

    /** What a grpc-java service reads of its caller: the transport's remote address, through an interceptor. */
    static final class Caller implements ServerInterceptor {
        @Override public <Q, A> ServerCall.Listener<Q> interceptCall(ServerCall<Q, A> call, Metadata headers, ServerCallHandler<Q, A> next) {
            return Contexts.interceptCall(Context.current().withValue(CALLER, String.valueOf(call.getAttributes().get(Grpc.TRANSPORT_ATTR_REMOTE_ADDR))), call, headers, next);
        }
    }

    static final class DroneService extends DroneGrpc.DroneImplBase {
        @Override public void arm(ArmRequest r, StreamObserver<ArmReply> out) {
            switch (r.getId()) {
                case 0 -> out.onError(Status.FAILED_PRECONDITION.withDescription("battery low").asRuntimeException());
                case 13 -> throw new IllegalStateException("thirteen");
                case 22 -> out.onError(Status.INTERNAL.asRuntimeException());
                case 23 -> { out.onNext(ArmReply.newBuilder().setArmed(true).setBy("x".repeat(1000)).build()); out.onCompleted(); }
                case 24 -> ((io.grpc.stub.ServerCallStreamObserver<ArmReply>) out).setOnCancelHandler(() -> HEARD.get().done());
                case 27 -> { HELD.incrementAndGet(); try { HOLD.await(); } catch (InterruptedException e) { } out.onNext(ArmReply.getDefaultInstance()); out.onCompleted(); }
                default -> { out.onNext(ArmReply.newBuilder().setArmed(true).setId(r.getId()).setBy(CALLER.get()).build()); out.onCompleted(); }
            }
        }

        @Override public void telemetry(TelemetryRequest r, StreamObserver<Sample> out) {
            List<Long> bigAt = r.getUnknownFields().getField(3).getVarintList(); // big_at: the item padded past the wire
            for (int i = 0; i < r.getCount(); i++) {
                if (r.getFailAfter() > 0 && i == r.getFailAfter()) { out.onError(Status.ABORTED.withDescription("lost the camera").asRuntimeException()); return; }
                Sample.Builder s = Sample.newBuilder().setSeq(i);
                if (!bigAt.isEmpty()) {
                    if (i == bigAt.get(0)) s.setUnknownFields(field(2, f -> f.addLengthDelimited(com.google.protobuf.ByteString.copyFrom(new byte[1000]))));
                    sleep(30);
                    if (((io.grpc.stub.ServerCallStreamObserver<Sample>) out).isCancelled()) { stopped = true; return; }
                }
                out.onNext(s.build());
            }
            out.onCompleted();
        }
    }

    /** The radio between the channels: drops nothing unless cut, and can hold the drone's wires to deliver them reversed. */
    static final class Link {
        MudChannel[] ends = new MudChannel[0];
        Native nat;
        volatile boolean cut, refuseListen;
        volatile int hold, lastTtl;
        volatile long refuseFor;
        final List<Mud.Outgoing> held = new ArrayList<>();
        void send(int from, Mud.Outgoing o) {
            if (o.listen()) { if (refuseListen) throw new UnsupportedOperationException("this radio has no listen"); return; }
            if (System.nanoTime() < refuseFor) throw new IllegalStateException("not yet");
            lastTtl = o.ttlSecs();
            if (cut) return;
            if (nat != null) { nat.inbox.add(o); return; }
            MudChannel to = ends[1 - from];
            if (from == 1 && hold > 0) {
                synchronized (held) {
                    held.add(o);
                    if (held.size() < hold) return;
                    hold = 0;
                    for (int i = held.size() - 1; i >= 0; i--) to.deliver(held.get(i).topic(), held.get(i).wire());
                    held.clear();
                }
                return;
            }
            to.deliver(o.topic(), o.wire());
        }
    }

    /** A mudRPC server with no gRPC: Rover's Telemetry sends count items, 2 down to 0, end on the last; Arm refuses. */
    static final class Native {
        final Mud.Peer peer;
        final ConcurrentLinkedQueue<Mud.Outgoing> inbox = new ConcurrentLinkedQueue<>();
        final Thread loop;
        volatile boolean stop;
        volatile MudChannel back;
        Native(Mud.Peer peer) {
            this.peer = peer;
            peer.listen("test.Rover");
            loop = new Thread(this::run);
            loop.setDaemon(true);
            loop.start();
        }
        void stop() throws InterruptedException { stop = true; loop.join(); }
        void run() {
            long start = System.nanoTime();
            while (!stop) {
                int now = (int) ((System.nanoTime() - start) / 1_000_000_000L);
                for (Mud.Outgoing w; (w = inbox.poll()) != null;) {
                    Mud.Incoming c = peer.deliver(now, w.topic(), w.wire());
                    if (c.kind() != Mud.Kind.CALL) continue;
                    if (c.method().equals("Arm")) { peer.reply(now, c, 9, new byte[0], true, 0, "native says no"); continue; }
                    int n;
                    try { n = TelemetryRequest.parseFrom(c.bytes()).getCount(); } catch (Exception e) { continue; }
                    for (int i = 0; i < n; i++) peer.reply(now, c, 0, Sample.newBuilder().setSeq(n - 1 - i).build().toByteArray(), i == n - 1, i, "");
                }
                for (Mud.Outgoing o; (o = peer.outgoing(now)) != null; peer.sent()) if (!o.listen() && back != null) back.deliver(o.topic(), o.wire());
                sleep(10);
            }
        }
    }
}
