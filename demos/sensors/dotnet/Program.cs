// Sensor telemetry over UDP in .NET: two sensors publish readings to a gateway, which calibrates one by a call. Three peers
// over UDP on this machine, one loop, the binding's own API with the messages Grpc.Tools generates from sensor.proto.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Threading;
using Demo.Sensors;
using Google.Protobuf;
using MudRpc;

var key = Enumerable.Range(1, 32).Select(i => (byte)i).ToArray();   // the fleet key: a product provisions 32 random bytes
var names = new[] { "gateway", "sensor-1", "sensor-2" };
var ports = names.Select(_ => FreePort()).ToArray();
var sockets = ports.Select(p => new Udp($"127.0.0.1:{p}", ports.Where(q => q != p).Select(q => $"127.0.0.1:{q}").ToArray())).ToArray();
var peers = names.Select((n, i) => new Peer(n, key, sender: (uint)i + 1, epoch: 1)).ToArray();
peers[0].Listen("sensors.Readings");
peers[2].Listen("demo.sensors.Sensor");
peers[1].Publish(0, "sensors.Readings", new Reading { Sensor = "sensor-1", CentiDegrees = 2150 }.ToByteArray(), ttlSecs: 60);
peers[2].Publish(0, "sensors.Readings", new Reading { Sensor = "sensor-2", CentiDegrees = 1900 }.ToByteArray(), ttlSecs: 60);

var readings = new SortedDictionary<string, int>();
var clock = Stopwatch.StartNew();
for (bool done = false; !done; Thread.Sleep(5))
{
    uint now = (uint)(clock.ElapsedMilliseconds / 1000);
    for (int i = 0; i < 3; i++)
    {
        var p = peers[i];
        for (Outgoing? o; (o = p.Outgoing(now)) != null; p.Sent())
            try { sockets[i].Send(o); } catch (MudException) { break; }   // refused: the item is there next round
        for ((string Topic, byte[] Wire)? w; (w = sockets[i].Poll()) != null;)
        {
            var m = p.Deliver(now, w.Value.Topic, w.Value.Wire);
            switch (m.Kind)
            {
                case Kind.Event:
                    var r = Reading.Parser.ParseFrom(m.Bytes);
                    readings[r.Sensor] = r.CentiDegrees;
                    if (readings.Count < 2) break;
                    foreach (var (sensor, centi) in readings) Console.WriteLine($"reading {sensor} {Degrees(centi)}");
                    p.Call(now, "demo.sensors.Sensor", "Calibrate", new Offset { CentiDegrees = 50 }.ToByteArray(), ttlSecs: 10);
                    break;
                case Kind.Call:
                    p.Reply(now, m, status: 0, new Ack { Sensor = names[i], OffsetCentiDegrees = Offset.Parser.ParseFrom(m.Bytes).CentiDegrees }.ToByteArray());
                    break;
                case Kind.Reply:
                    var a = Ack.Parser.ParseFrom(m.Bytes);
                    Console.WriteLine($"calibrated {a.Sensor} by {Degrees(a.OffsetCentiDegrees)}");
                    done = true;
                    break;
            }
        }
        p.Tick(now);
    }
}
foreach (var p in peers) p.Dispose();
foreach (var s in sockets) s.Dispose();
return 0;

static string Degrees(int centi) => $"{centi / 100}.{centi % 100:D2}C";

static int FreePort() { using var s = new System.Net.Sockets.UdpClient(new System.Net.IPEndPoint(System.Net.IPAddress.Loopback, 0)); return ((System.Net.IPEndPoint)s.Client.LocalEndPoint!).Port; }
