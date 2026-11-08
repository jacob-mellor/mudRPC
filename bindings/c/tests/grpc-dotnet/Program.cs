// The gRPC drop-in under test: a client and a service base Grpc.Tools generated from drone.proto, written as gRPC code
// is, over two mudRPC peers in this process. Building the two channels is the only mudRPC code above the services.
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Google.Protobuf;
using Grpc.Core;
using MudRpc;
using Test;

var key = Enumerable.Range(1, 32).Select(i => (byte)i).ToArray();
using var gcsPeer = new Peer("gcs", key, 1, 1);
using var dronePeer = new Peer("drone", key, 2, 1);
var link = new Link();
using var gcs = new MudChannel(gcsPeer, o => link.Send(0, o));
using var drone = new MudChannel(dronePeer, o => link.Send(1, o));
link.Ends = new[] { gcs, drone };
Drone.BindService(drone.Binder, new DroneService());

// From here, the code a gRPC product already has.
var client = new Drone.DroneClient(gcs);
var r = await client.ArmAsync(new ArmRequest { Id = 7 });
Console.WriteLine($"arm: armed {r.Armed} id {r.Id} by {r.By}");
Console.WriteLine($"blocking: armed {client.Arm(new ArmRequest { Id = 8 }).Armed}");
await Fails("refused", async () => await client.ArmAsync(new ArmRequest { Id = 0 }));
await Fails("handler throws", async () => await client.ArmAsync(new ArmRequest { Id = 13 }));
var fly = new Method<ArmRequest, ArmReply>(MethodType.Unary, "test.Drone", "Fly", Marshallers.Create((ArmRequest m) => m.ToByteArray(), ArmRequest.Parser.ParseFrom), Marshallers.Create((ArmReply m) => m.ToByteArray(), ArmReply.Parser.ParseFrom));
await Fails("unknown method", async () => await gcs.CreateCallInvoker().AsyncUnaryCall(fly, null, new CallOptions(), new ArmRequest { Id = 1 }));
var surrogate = new Method<ArmRequest, ArmReply>(MethodType.Unary, "test.Drone", "Arm\uD800", fly.RequestMarshaller, fly.ResponseMarshaller);
await Fails("a method name with a lone surrogate", async () => await gcs.CreateCallInvoker().AsyncUnaryCall(surrogate, null, new CallOptions(), new ArmRequest { Id = 1 }));
var longService = new Method<ArmRequest, ArmReply>(MethodType.Unary, new string('s', 65), "Arm", fly.RequestMarshaller, fly.ResponseMarshaller);
await Fails("a service name of 65 bytes", async () => await gcs.CreateCallInvoker().AsyncUnaryCall(longService, null, new CallOptions(), new ArmRequest { Id = 1 }));

link.Hold = 6;   // five items and the stream's end, delivered newest first
Console.WriteLine($"telemetry reversed: {await Read(client.Telemetry(new TelemetryRequest { Count = 5 }))}");
Console.WriteLine($"telemetry empty: {await Read(client.Telemetry(new TelemetryRequest { Count = 0 }))}");
await Fails("telemetry fails", async () => await Read(client.Telemetry(new TelemetryRequest { Count = 5, FailAfter = 2 }), "telemetry fails"));
await Fails("upload", () => { client.Upload(); return Task.CompletedTask; });
await Fails("too large", async () => await client.ArmAsync(new ArmRequest { Id = 1, Note = ByteString.CopyFrom(new byte[1000]) }));

link.Cut = true;
var watch = Stopwatch.StartNew();
await Fails("deadline", async () => await client.ArmAsync(new ArmRequest { Id = 9 }, deadline: DateTime.UtcNow.AddSeconds(2)));
Console.WriteLine($"deadline in time: {watch.Elapsed.TotalSeconds is >= 2.0 and < 3.5}");
using (var cts = new CancellationTokenSource(TimeSpan.FromMilliseconds(300))) await Fails("cancelled", async () => await client.ArmAsync(new ArmRequest { Id = 10 }, cancellationToken: cts.Token));
var unseen = client.ArmAsync(new ArmRequest { Id = 12 }, deadline: DateTime.UtcNow.AddSeconds(1));
Console.WriteLine($"status before the end: {Throws(() => unseen.GetStatus())}, trailers {Throws(() => unseen.GetTrailers())}");
await Fails("unseen", async () => await unseen);
using (var cts = new CancellationTokenSource(TimeSpan.FromMilliseconds(200)))
{
    var stream = client.Telemetry(new TelemetryRequest { Count = 3 });
    await Fails("reading cancelled", async () => await stream.ResponseStream.MoveNext(cts.Token));
}
link.Cut = false;
Console.WriteLine($"after the cut: armed {(await client.ArmAsync(new ArmRequest { Id = 11 })).Armed}");

Console.WriteLine($"local deadline: {Throws(() => client.ArmAsync(new ArmRequest { Id = 1 }, deadline: DateTime.Now.AddSeconds(5)))}");
await Fails("status set", async () => await client.ArmAsync(new ArmRequest { Id = 21 }));
var bad = new Method<ArmRequest, ArmReply>(MethodType.Unary, "test.Drone", "Arm", Marshallers.Create((ArmRequest m) => new byte[] { 0xff }, ArmRequest.Parser.ParseFrom), Marshallers.Create((ArmReply m) => m.ToByteArray(), ArmReply.Parser.ParseFrom));
await Fails("undecodable request", async () => await gcs.CreateCallInvoker().AsyncUnaryCall(bad, null, new CallOptions(), new ArmRequest()));
var shown = client.ArmAsync(new ArmRequest { Id = 14 });
await shown;
Console.WriteLine($"headers after the reply: {(await shown.ResponseHeadersAsync).Count}, status {shown.GetStatus().StatusCode}");

var before = gcs.Use(p => p.Pending);
using (var gone = new CancellationTokenSource())
{
    gone.Cancel();
    for (int i = 0; i < 16; i++) await Fails("", async () => await client.ArmAsync(new ArmRequest { Id = 1 }, cancellationToken: gone.Token), quiet: true);
}
Console.WriteLine($"cancelled before sending, 16 times: pending {gcs.Use(p => p.Pending) - before} more, then armed {(await client.ArmAsync(new ArmRequest { Id = 15 })).Armed}");

int finished = 0;
for (int round = 0; round < 30; round++)
{
    var cts = new CancellationTokenSource();
    var t = client.ArmAsync(new ArmRequest { Id = 16 }, cancellationToken: cts.Token).ResponseAsync;
    var canceller = new Thread(() => cts.Cancel());
    gcs.Use(_ => { Thread.Sleep(30); canceller.Start(); Thread.Sleep(20); });   // the reply waits in the inbox while the cancel waits for the pump
    if (t.ContinueWith(x => _ = x.Exception).Wait(3000)) finished++;   // the call's outcome observed: cancelled or answered
    canceller.Join();
}
Console.WriteLine($"cancel racing a reply: {finished} of 30 finished");

var calls = Task.Run(async () => { int n = 0; for (int i = 0; i < 50; i++) { if ((await client.ArmAsync(new ArmRequest { Id = 1 })).Armed) n++; await Task.Delay(40); } return n; });   // a client keeps under 32 calls a second
int saves = 0;
while (!calls.IsCompleted) { gcs.Use(p => p.Save()); saves++; await Task.Delay(1); }
Console.WriteLine($"saved while calling: {await calls} calls answered, saves {(saves > 0 ? "made" : "none")}");

Console.WriteLine($"two full streams back to back: {(await Read(client.Telemetry(new TelemetryRequest { Count = 63 }))).Split(' ').Length} {(await Read(client.Telemetry(new TelemetryRequest { Count = 63 }))).Split(' ').Length}");
await Fails("a 64th item", async () => await Read(client.Telemetry(new TelemetryRequest { Count = 64 })));
var big = new Method<TelemetryRequest, Sample>(MethodType.ServerStreaming, "test.Drone", "Big", Marshallers.Create((TelemetryRequest m) => m.ToByteArray(), TelemetryRequest.Parser.ParseFrom), Marshallers.Create((Sample m) => m.Seq == 2 ? new byte[1000] : m.ToByteArray(), Sample.Parser.ParseFrom));
bool bigStopped = false;
drone.Binder.AddMethod(big, async (TelemetryRequest r, IServerStreamWriter<Sample> w, ServerCallContext ctx) =>
{
    try { for (uint i = 0; i < r.Count; i++) { await w.WriteAsync(new Sample { Seq = i }); await Task.Delay(30); } }
    catch (RpcException) { bigStopped = true; throw; }
});
var bigSeen = new List<uint>();
var bigCall = gcs.CreateCallInvoker().AsyncServerStreamingCall(big, null, new CallOptions(), new TelemetryRequest { Count = 40 });
string bigEnd = "OK";
try { while (await bigCall.ResponseStream.MoveNext(CancellationToken.None)) bigSeen.Add(bigCall.ResponseStream.Current.Seq); } catch (RpcException e) { bigEnd = $"{e.StatusCode} {e.Status.Detail}"; }
await Task.Delay(100);
Console.WriteLine($"an item past the wire mid-stream: {string.Join(" ", bigSeen)}, {bigEnd}, its handler stopped: {bigStopped}");

var lonePeer = new Peer("lone", key, 9, 1);
using (var lone = new MudChannel(lonePeer, _ => { }))
{
    lonePeer.Dispose();
    Thread.Sleep(200);   // the pump's next round finds the peer gone and closes the channel
    await Fails("the peer disposed first", async () => await new Drone.DroneClient(lone).ArmAsync(new ArmRequest { Id = 1 }, deadline: DateTime.UtcNow.AddSeconds(2)));
}

var free = 16 - (int)gcs.Use(p => p.Pending);   // a call cancelled after sending keeps its slot until its ttl
var unknown = await Task.WhenAll(Enumerable.Range(0, free).Select(async i => { try { await gcs.CreateCallInvoker().AsyncUnaryCall(fly, null, new CallOptions(deadline: DateTime.UtcNow.AddSeconds(5)), new ArmRequest { Id = 1 }); return "OK"; } catch (RpcException e) { return e.StatusCode.ToString(); } }));
Console.WriteLine($"unknown method, every free slot at once: {(unknown.All(s => s == "Unimplemented") ? "all Unimplemented" : string.Join(", ", unknown))}");
await Fails("null detail", async () => await client.ArmAsync(new ArmRequest { Id = 22 }));
Console.WriteLine($"a null wire: {Throws(() => gcs.Deliver("gcs", null!))}");
link.Cut = true;
_ = client.ArmAsync(new ArmRequest { Id = 1 }, deadline: DateTime.UtcNow.AddDays(8));
Thread.Sleep(200);
link.Cut = false;
Console.WriteLine($"a deadline eight days out: ttl {link.LastTtl}");
GC.Collect(); GC.WaitForPendingFinalizers();   // what earlier cases left is not counted
int unobserved = 0;
TaskScheduler.UnobservedTaskException += (_, e) => { Interlocked.Increment(ref unobserved); e.SetObserved(); };
for (int i = 0; i < 50; i++) { await Fails("", async () => await client.ArmAsync(new ArmRequest { Id = 0 }), quiet: true); await Task.Delay(40); }
GC.Collect(); GC.WaitForPendingFinalizers(); GC.Collect();
Console.WriteLine($"fifty failed calls, unobserved exceptions: {unobserved}");
gcs.Use(_ => { for (int i = 0; i < 5000; i++) gcs.Deliver("gcs", new byte[40]); });
Console.WriteLine($"five thousand junk wires: then armed {(await client.ArmAsync(new ArmRequest { Id = 1 })).Armed}");

var link3 = new Link { RefuseListen = true };
int faults = 0;
using var gcs3 = new MudChannel(new Peer("gcs3", key, 7, 1), o => link3.Send(0, o), fault: _ => Interlocked.Increment(ref faults));
using var drone3 = new MudChannel(new Peer("drone3", key, 8, 1), o => link3.Send(1, o));
link3.Ends = new[] { gcs3, drone3 };
Drone.BindService(drone3.Binder, new DroneService());
Console.WriteLine($"a transport refusing a listen: armed {(await new Drone.DroneClient(gcs3).ArmAsync(new ArmRequest { Id = 1 }, deadline: DateTime.UtcNow.AddSeconds(10))).Armed}, faults heard {(faults > 0 ? "yes" : "no")}");

var selfish = default(MudChannel);
selfish = new MudChannel(new Peer("selfish", key, 10, 1), _ => selfish!.Dispose());
var stopped = Task.Run(() => { Thread.Sleep(300); selfish.Dispose(); });
Console.WriteLine($"disposed from its own send: {(stopped.Wait(3000) ? "returns" : "hangs")}");

using (var stream = client.Telemetry(new TelemetryRequest { Count = 5 }))
{
    Thread.Sleep(500);   // every item has landed
    await stream.ResponseStream.MoveNext(CancellationToken.None);
    stream.Dispose();
    await Fails("cancelled with items unread", async () => await stream.ResponseStream.MoveNext(CancellationToken.None));
}
var slow = Stopwatch.StartNew();
await Fails("a slow reader past its deadline", async () => { var st = client.Telemetry(new TelemetryRequest { Count = 5 }, deadline: DateTime.UtcNow.AddSeconds(1)); while (await st.ResponseStream.MoveNext(CancellationToken.None)) Thread.Sleep(600); });
Console.WriteLine($"the slow reader's deadline in time: {slow.Elapsed.TotalSeconds < 2.0}");
{
    var linkC = new Link();
    var gcsC = new MudChannel(new Peer("gcsc", key, 19, 1), o => linkC.Send(0, o));
    using var droneC = new MudChannel(new Peer("dronec", key, 20, 1), o => linkC.Send(1, o));
    linkC.Ends = new[] { gcsC, droneC };
    Drone.BindService(droneC.Binder, new DroneService());
    var st = new Drone.DroneClient(gcsC).Telemetry(new TelemetryRequest { Count = 5 });
    Thread.Sleep(500);
    await st.ResponseStream.MoveNext(CancellationToken.None);
    gcsC.Dispose();
    await Fails("closed with items unread", async () => await st.ResponseStream.MoveNext(CancellationToken.None));
}
var (gu0, du0, pd) = UdpPair();
using var gu = gu0;
using var du = du0;
using var gcsU = new MudChannel(new Peer("gcsu", key, 11, 1), gu);
using var droneU = new MudChannel(new Peer("droneu", key, 12, 1), du);
Drone.BindService(droneU.Binder, new DroneService());
await Task.Delay(200); // a server answers once its listen has reached its socket
var overUdp = new Drone.DroneClient(gcsU);
Console.WriteLine($"over UDP: armed {(await overUdp.ArmAsync(new ArmRequest { Id = 1 }, deadline: DateTime.UtcNow.AddSeconds(5))).Armed}, telemetry {await Read(overUdp.Telemetry(new TelemetryRequest { Count = 5 }))}");
var lostSocket = new Udp("127.0.0.1:0", $"127.0.0.1:{pd}");
using (var lost = new MudChannel(new Peer("lost", key, 13, 1), lostSocket))
{
    lostSocket.Dispose();
    Thread.Sleep(100);
    await Fails("its socket disposed", async () => await new Drone.DroneClient(lost).ArmAsync(new ArmRequest { Id = 1 }, deadline: DateTime.UtcNow.AddSeconds(2)));
}
Console.WriteLine($"a public bind: {Throws(() => new Udp("0.0.0.0:0", "127.0.0.1:9"))}, a public peer: {Throws(() => new Udp("127.0.0.1:0", "8.8.8.8:53"))}, this library public: {Udp.Internet}");

{
    Udp[] three = Array.Empty<Udp>();
    for (int i = 0; three.Length == 0; i++)
    {
        int[] ps = { FreePort(), FreePort(), FreePort() };
        try { three = ps.Select(p => new Udp($"127.0.0.1:{p}", ps.Where(q => q != p).Select(q => $"127.0.0.1:{q}").ToArray())).ToArray(); }
        catch (MudException e) when (e.Code == -2 && i < 50) { }
    }
    using var gT = new MudChannel(new Peer("gcst", key, 21, 1), three[0]);
    using var dT = new MudChannel(new Peer("dronet", key, 22, 1), three[1]);
    using var iT = new MudChannel(new Peer("idlet", key, 23, 1), three[2]);
    Drone.BindService(dT.Binder, new DroneService());
    await Task.Delay(200); // a server answers once its listen has reached its socket
    var threeClient = new Drone.DroneClient(gT);
    int armedT = 0;
    for (int i = 0; i < 10; i++) if ((await threeClient.ArmAsync(new ArmRequest { Id = 1 }, deadline: DateTime.UtcNow.AddSeconds(5))).Armed) armedT++;
    Console.WriteLine($"three on UDP, one serving nothing: {armedT} of 10 armed");
}

// A native peer answers the Rover service with the plain API: a stream ends on its last item, and a refusal has a detail.
using var roverPeer = new Peer("rover", key, 3, 1);
var native = new Native(roverPeer);
var link2 = new Link();
using var gcs2 = new MudChannel(new Peer("gcs2", key, 4, 1), o => link2.Send(0, o));
link2.Ends = new[] { gcs2 };
link2.Native = native;
native.Back = gcs2;
var rover = new Rover.RoverClient(gcs2);
Console.WriteLine($"native stream: {await Read(rover.Telemetry(new TelemetryRequest { Count = 3 }))}");
await Fails("native refusal", async () => await rover.ArmAsync(new ArmRequest { Id = 1 }));
{
    var crowd = Enumerable.Range(0, 48).Select(i => new Peer($"crowd{i}", key, 100 + (uint)i, 1)).ToList();   // 48 callers owed a stream each, past the 32 a second: the next caller waits its turn, not behind them all
    foreach (var p in crowd)
    {
        p.Call(0, "test.Drone", "Telemetry", new TelemetryRequest { Count = 20 }.ToByteArray(), 5);
        for (Outgoing? o; (o = p.Outgoing(0)) != null; p.Sent()) if (!o.Listen) drone.Deliver(o.Topic, o.Wire);
    }
    await Task.Delay(300);
    string crowdArmed;
    try { crowdArmed = (await client.ArmAsync(new ArmRequest { Id = 1 }, deadline: DateTime.UtcNow.AddSeconds(3))).Armed.ToString(); } catch (RpcException) { crowdArmed = "False"; }
    Console.WriteLine($"a caller behind 48 owed a stream each: armed {crowdArmed}");
    crowd.ForEach(p => p.Dispose());
}
native.Stop();
return 0;

static async Task Fails(string what, Func<Task> f, bool quiet = false)
{
    try { await f(); Console.WriteLine($"{what}: no error"); }
    catch (RpcException e) { if (!quiet) Console.WriteLine($"{what}: {e.StatusCode} {e.Status.Detail}"); }
}

/// <summary>Two sockets on loopback that list each other: ports chosen free, chosen again while another test takes one between the choosing and the binding.</summary>
static (Udp, Udp, int) UdpPair()
{
    for (int i = 0; ; i++)
    {
        int a = FreePort(), b = FreePort();
        Udp? x = null;
        try { x = new Udp($"127.0.0.1:{a}", $"127.0.0.1:{b}"); return (x, new Udp($"127.0.0.1:{b}", $"127.0.0.1:{a}"), b); }
        catch (MudException e) when (e.Code == -2 && i < 50) { x?.Dispose(); }
    }
}

static int FreePort() { using var s = new System.Net.Sockets.UdpClient(new System.Net.IPEndPoint(System.Net.IPAddress.Loopback, 0)); return ((System.Net.IPEndPoint)s.Client.LocalEndPoint!).Port; }

static string Throws(Action f) { try { f(); return "no error"; } catch (Exception e) { return e.GetType().Name; } }

static async Task<string> Read(AsyncServerStreamingCall<Sample> call, string? partial = null)
{
    var seen = new List<uint>();
    try { while (await call.ResponseStream.MoveNext(CancellationToken.None)) seen.Add(call.ResponseStream.Current.Seq); }
    catch (RpcException) when (partial != null) { Console.WriteLine($"{partial} after: {string.Join(" ", seen)}"); throw; }
    return string.Join(" ", seen);
}

sealed class DroneService : Drone.DroneBase
{
    public override Task<ArmReply> Arm(ArmRequest r, ServerCallContext ctx) => r.Id switch
    {
        0 => throw new RpcException(new Status(StatusCode.FailedPrecondition, "battery low")),
        13 => throw new InvalidOperationException("thirteen"),
        21 => Set(ctx, new ArmReply { Armed = true }),
        22 => throw new RpcException(new Status(StatusCode.Internal, null!)),
        _ => Task.FromResult(new ArmReply { Armed = true, Id = r.Id, By = ctx.Peer }),
    };

    static Task<ArmReply> Set(ServerCallContext ctx, ArmReply r) { ctx.Status = new Status(StatusCode.PermissionDenied, "not yours"); return Task.FromResult(r); }

    public override async Task Telemetry(TelemetryRequest r, IServerStreamWriter<Sample> w, ServerCallContext ctx)
    {
        for (uint i = 0; i < r.Count; i++)
        {
            if (r.FailAfter > 0 && i == r.FailAfter) throw new RpcException(new Status(StatusCode.Aborted, "lost the camera"));
            await w.WriteAsync(new Sample { Seq = i });
        }
    }
}

/// <summary>The radio between the channels: drops nothing unless cut, and can hold the drone's wires to deliver them reversed.</summary>
sealed class Link
{
    public MudChannel[] Ends = Array.Empty<MudChannel>();
    public Native? Native;
    public volatile bool Cut;
    public volatile int Hold;
    public volatile bool RefuseListen;
    public uint LastTtl;
    readonly List<(string, byte[])> held = new();

    public void Send(int from, Outgoing o)
    {
        if (o.Listen) { if (RefuseListen) throw new NotSupportedException("this radio has no listen"); return; }
        LastTtl = o.TtlSecs;
        if (Cut) return;
        if (Native != null) { Native.Deliver(o.Topic, o.Wire); return; }
        var to = Ends[1 - from];
        if (from == 1 && Hold > 0)
        {
            lock (held)
            {
                held.Add((o.Topic, o.Wire));
                if (held.Count < Hold) return;
                Hold = 0;
                for (int i = held.Count - 1; i >= 0; i--) to.Deliver(held[i].Item1, held[i].Item2);
                held.Clear();
            }
            return;
        }
        to.Deliver(o.Topic, o.Wire);
    }
}

/// <summary>A mudRPC server with no gRPC: Rover's Telemetry sends count items, 2 down to 0, end on the last; Arm refuses.</summary>
sealed class Native
{
    readonly Peer peer;
    readonly ConcurrentQueue<(string, byte[])> inbox = new();
    readonly Thread loop;
    volatile bool stop;
    public MudChannel? Back;

    public Native(Peer peer)
    {
        this.peer = peer;
        peer.Listen("test.Rover");
        loop = new Thread(Run) { IsBackground = true };
        loop.Start();
    }

    public void Deliver(string topic, byte[] wire) => inbox.Enqueue((topic, wire));
    public void Stop() { stop = true; loop.Join(); }

    void Run()
    {
        var clock = Stopwatch.StartNew();
        while (!stop)
        {
            uint now = (uint)(clock.ElapsedMilliseconds / 1000);
            while (inbox.TryDequeue(out var w))
            {
                var c = peer.Deliver(now, w.Item1, w.Item2);
                if (c.Kind != Kind.Call) continue;
                if (c.Method == "Arm") { peer.Reply(now, c, 9, Array.Empty<byte>(), detail: "native says no"); continue; }
                var n = TelemetryRequest.Parser.ParseFrom(c.Bytes).Count;
                for (uint i = 0; i < n; i++) peer.Reply(now, c, 0, new Sample { Seq = n - 1 - i }.ToByteArray(), i == n - 1, i);
            }
            for (Outgoing? o; (o = peer.Outgoing(now)) != null; peer.Sent()) if (!o.Listen) Back!.Deliver(o.Topic, o.Wire);
            Thread.Sleep(10);
        }
    }
}
