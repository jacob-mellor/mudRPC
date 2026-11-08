# C# gRPC without IIS

**C# gRPC without IIS**, ASP.NET Core or Kestrel is a console app: the service `Grpc.Tools` generated, bound to a mudRPC channel in place of the web host, answering calls from a peer it has never connected to.

gRPC for .NET is `Grpc.AspNetCore`: a service is an ASP.NET Core app, hosted by Kestrel or IIS, and a client is `GrpcChannel.ForAddress` over HTTP/2. A ground station on Windows, a service on a board's Linux or a tool that must run where no web server is allowed carries all of that for one `.proto`. mudRPC's .NET binding is one file, `Mud.cs`, over the C ABI through P/Invoke with no unsafe code, and `MudGrpc.cs` beside it is gRPC's own `ChannelBase` and `ServiceBinderBase` from `Grpc.Core.Api`, the API package alone.

The client is the generated one, as before:

```csharp
// was: var channel = GrpcChannel.ForAddress("https://drone:5001");
var channel = new MudChannel(new Peer("gcs", fleetKey, sender: 1, epoch: epoch), new Udp("10.0.0.2:7000", "10.0.0.3:7000"));
var client = new Drone.DroneClient(channel);
var reply = await client.ArmAsync(new ArmRequest { Id = 7 }, deadline: DateTime.UtcNow.AddSeconds(30));
```

The server is `Drone.BindService(channel.Binder, new DroneService())` where ASP.NET Core's `MapGrpcService` was, in any process, with `Grpc.Core.Api` and `Google.Protobuf` the dependencies: no `Grpc.AspNetCore`, no `Grpc.Net.Client`, no HTTP/2. How deadlines, statuses and exceptions arrive, and what a connected channel has that this one has not, is the .NET skill's [Drop-in for gRPC](../bindings/dotnet/SKILL.md#drop-in-for-grpc), with the Integrate steps from a clone to a signed fleet before it. The silo this page belongs to is [gRPC without HTTP/2](grpc-without-http2.md); the [home page](../README.md) has every language.
