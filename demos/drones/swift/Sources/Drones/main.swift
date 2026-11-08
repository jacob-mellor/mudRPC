// gRPC drone telemetry in Swift: the client and service protoc-gen-grpc-swift-2 generates from drone.proto, a ground
// station and a mock drone, two mudRPC peers over UDP in one process. Building the transports is the only mudRPC code.
import GRPCCore
import GRPCProtobuf
import Synchronization

let key = (1...32).map { UInt8($0) }   // the fleet key; a product provisions 32 random bytes to every peer
let epoch: UInt32 = 1
let pilot = "gcs"
let options = { var o = CallOptions.defaults; o.timeout = .seconds(10); return o }()
let grpcNames = ["OK", "CANCELLED", "UNKNOWN", "INVALID_ARGUMENT", "DEADLINE_EXCEEDED", "NOT_FOUND", "ALREADY_EXISTS", "PERMISSION_DENIED", "RESOURCE_EXHAUSTED", "FAILED_PRECONDITION", "ABORTED", "OUT_OF_RANGE", "UNIMPLEMENTED", "INTERNAL", "UNAVAILABLE", "DATA_LOSS", "UNAUTHENTICATED"]

/// Sockets on free loopback ports, each listing the others: the ports found by binding, then bound again listed.
func sockets(_ n: Int) throws -> [Udp] {
  let probes = try (0..<n).map { _ in try Udp(bind: "127.0.0.1:0", peers: ["127.0.0.1:9"]) }
  let ports = try probes.map { try $0.port }
  probes.forEach { $0.close() }
  return try ports.map { p in try Udp(bind: "127.0.0.1:\(p)", peers: ports.filter { $0 != p }.map { "127.0.0.1:\($0)" }) }
}

final class DroneService: Demo_Drones_Drone.SimpleServiceProtocol, Sendable {
  let armed = Mutex(false)
  func arm(request: Demo_Drones_ArmRequest, context: ServerContext) async throws -> Demo_Drones_ArmReply {
    armed.withLock { $0 = true }
    return .with { $0.armed = true; $0.drone = "drone-1" }
  }
  func telemetry(request: Demo_Drones_TelemetryRequest, response: RPCWriter<Demo_Drones_Sample>, context: ServerContext) async throws {
    for i in 0..<request.samples { try await response.write(.with { $0.seq = i; $0.altitudeM = 10 * Int32(i + 1); $0.batteryPct = 100 - 2 * Int32(i) }) }
  }
  func land(request: Demo_Drones_LandRequest, context: ServerContext) async throws -> Demo_Drones_LandReply {
    guard armed.withLock({ $0 }) else { throw RPCError(code: .failedPrecondition, message: "not armed") }
    armed.withLock { $0 = false }
    return .with { $0.landed = true }
  }
}

let (gcsSocket, droneSocket) = try { let s = try sockets(2); return (s[0], s[1]) }()
let gcs = MudTransport(peer: try Peer(name: "gcs", key: key, sender: 1, epoch: epoch), udp: gcsSocket)
let drone = MudTransport(peer: try Peer(name: "drone", key: key, sender: 2, epoch: epoch), udp: droneSocket)
let server = GRPCServer(transport: drone.server, services: [DroneService()])
let serving = Task { try await server.serve() }
try await Task.sleep(for: .milliseconds(200))   // a server answers once its listen has reached its socket

// From here, the code a grpc-swift product already has.
try await withGRPCClient(transport: gcs.client) { grpc in
  let client = Demo_Drones_Drone.Client(wrapping: grpc)
  do { _ = try await client.land(.init(), options: options) } catch let e as RPCError { print("land: \(grpcNames[e.code.rawValue]) \(e.message)") }
  print("armed \(try await client.arm(.with { $0.pilot = pilot }, options: options).drone) for \(pilot)")
  let samples = try await client.telemetry(.with { $0.samples = 3 }, options: options) { try await $0.messages.reduce(into: [String]()) { $0.append("\($1.seq):\($1.altitudeM)m \($1.batteryPct)%") } }
  print("telemetry \(samples.joined(separator: " "))")
  if try await client.land(.init(), options: options).landed { print("landed") }
}
server.beginGracefulShutdown()
_ = await serving.result
for t in [gcs, drone] { t.close() }
for s in [gcsSocket, droneSocket] { s.close() }
