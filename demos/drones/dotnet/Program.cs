// gRPC drone telemetry in .NET: a ground station and a mock drone, two peers over UDP on this machine, the client and
// service base Grpc.Tools generated from drone.proto over mudRPC channels. Building the channels is the only mudRPC code.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Demo.Drones;
using Grpc.Core;
using MudRpc;

var key = Enumerable.Range(1, 32).Select(i => (byte)i).ToArray();   // the fleet key: a product provisions 32 random bytes
int gcsPort = FreePort(), dronePort = FreePort();
using var gcsSocket = new Udp($"127.0.0.1:{gcsPort}", $"127.0.0.1:{dronePort}");
using var droneSocket = new Udp($"127.0.0.1:{dronePort}", $"127.0.0.1:{gcsPort}");
using var gcsPeer = new Peer("gcs", key, sender: 1, epoch: 1);
using var dronePeer = new Peer("drone", key, sender: 2, epoch: 1);
using var gcs = new MudChannel(gcsPeer, gcsSocket);
using var drone = new MudChannel(dronePeer, droneSocket);
Drone.BindService(drone.Binder, new DroneService());

// From here, the code a gRPC product already has.
var client = new Drone.DroneClient(gcs);
try { await client.LandAsync(new LandRequest(), deadline: Deadline()); }
catch (RpcException e) { Console.WriteLine($"land: {Mud.StatusName((uint)e.StatusCode)} {e.Status.Detail}"); }
var armed = await client.ArmAsync(new ArmRequest { Pilot = "gcs" }, deadline: Deadline());
Console.WriteLine($"armed {armed.Drone} for gcs");
var samples = new List<string>();
using (var telemetry = client.Telemetry(new TelemetryRequest { Samples = 3 }, deadline: Deadline()))
    while (await telemetry.ResponseStream.MoveNext(CancellationToken.None))
    {
        var s = telemetry.ResponseStream.Current;
        samples.Add($"{s.Seq}:{s.AltitudeM}m {s.BatteryPct}%");
    }
Console.WriteLine($"telemetry {string.Join(" ", samples)}");
if ((await client.LandAsync(new LandRequest(), deadline: Deadline())).Landed) Console.WriteLine("landed");
return 0;

static DateTime Deadline() => DateTime.UtcNow.AddSeconds(10);

static int FreePort() { using var s = new System.Net.Sockets.UdpClient(new System.Net.IPEndPoint(System.Net.IPAddress.Loopback, 0)); return ((System.Net.IPEndPoint)s.Client.LocalEndPoint!).Port; }

/// <summary>The mock drone: Arm arms it, Telemetry streams the samples asked for, Land is refused until it is armed.</summary>
sealed class DroneService : Drone.DroneBase
{
    bool armed;

    public override Task<ArmReply> Arm(ArmRequest r, ServerCallContext ctx) { armed = true; return Task.FromResult(new ArmReply { Armed = true, Drone = "drone-1" }); }

    public override async Task Telemetry(TelemetryRequest r, IServerStreamWriter<Sample> w, ServerCallContext ctx)
    {
        for (int i = 0; i < r.Samples; i++) await w.WriteAsync(new Sample { Seq = (uint)i, AltitudeM = 10 * (i + 1), BatteryPct = 100 - 2 * i });
    }

    public override Task<LandReply> Land(LandRequest r, ServerCallContext ctx)
    {
        if (!armed) throw new RpcException(new Status(StatusCode.FailedPrecondition, "not armed"));
        armed = false;
        return Task.FromResult(new LandReply { Landed = true });
    }
}
