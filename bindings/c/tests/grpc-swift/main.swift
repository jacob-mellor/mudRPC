// The Swift drop-in under test: the client and service protoc-gen-grpc-swift-2 generated from drone.proto, written as
// grpc-swift code is, over two mudRPC peers in this process. Building the transports is the only mudRPC code above them.
import Foundation
import GRPCCore
import GRPCProtobuf
import Synchronization

let key = (1...32).map { UInt8($0) }
let heard = Mutex(Set<UInt32>()), stopped = Mutex(false)   // the ids whose handlers heard their call end
func say(_ s: String) { FileHandle.standardOutput.write(Data((s + "\n").utf8)) }   // each line as it happens, so a run cut short shows where
func nap(_ ms: Int) async { try? await Task.sleep(for: .milliseconds(ms)) }
func options(_ timeout: Duration?) -> CallOptions { var o = CallOptions.defaults; o.timeout = timeout; return o }
func arm(_ id: UInt32, note: Int = 0) -> Test_ArmRequest { .with { $0.id = id; $0.note = Data(count: note) } }
func telemetry(_ count: UInt32, failAfter: UInt32 = 0, bigAt: UInt32 = 0) -> Test_TelemetryRequest { .with { $0.count = count; $0.failAfter = failAfter; $0.bigAt = bigAt } }

func told(_ e: any Error) -> String {
  if let e = e as? RuntimeError { return "\(e.code): \((e.cause as? RPCError)?.message ?? e.message)" }
  return (e as? RPCError).map { "\($0.code) \($0.message)" } ?? "\(type(of: e))"
}
@MainActor func fails(_ what: String, _ f: () async throws -> Void) async {
  do { try await f(); say("\(what): no error") } catch { say("\(what): \(told(error))") }
}
func thrown(_ f: () throws -> Void) -> String {
  do { try f(); return "no error" } catch let e as MudError { return "MudError \(e.code)" } catch { return "\(type(of: error))" }
}

typealias Drone = Test_Drone.Client<MudTransport.Client>
func read(_ c: Drone, _ r: Test_TelemetryRequest, _ o: CallOptions = .defaults, partial: String? = nil, pause: Int = 0) async throws -> String {
  try await c.telemetry(r, options: o) { response in
    var seen: [String] = []
    do {
      for try await s in response.messages {
        seen.append(String(s.seq))
        if pause > 0 { await nap(pause) }
      }
    } catch {
      if let partial { say("\(partial) after: \(seen.joined(separator: " "))") }
      throw error
    }
    return seen.joined(separator: " ")
  }
}

/// A method the service lacks, called through the generic client as a hand-written stub would.
func nope(_ grpc: GRPCClient<MudTransport.Client>, _ o: CallOptions = .defaults, service: String = "test.Drone") async throws {
  _ = try await grpc.unary(request: ClientRequest(message: arm(1)), descriptor: MethodDescriptor(fullyQualifiedService: service, method: "Nope"), serializer: ProtobufSerializer<Test_ArmRequest>(), deserializer: ProtobufDeserializer<Test_ArmReply>(), options: o) { try $0.message }
}

/// Sockets on loopback that list each other: ports chosen free, chosen again while another test takes one between the choosing and the binding.
func udpSet(_ n: Int) throws -> [Udp] {
  for i in 0... {
    let probes = try (0..<n).map { _ in try Udp(bind: "127.0.0.1:0", peers: ["127.0.0.1:9"]) }
    let ports = try probes.map { try $0.port }
    probes.forEach { $0.close() }
    var made: [Udp] = []
    do {
      for p in ports { made.append(try Udp(bind: "127.0.0.1:\(p)", peers: ports.filter { $0 != p }.map { "127.0.0.1:\($0)" })) }
      return made
    } catch let e as MudError where e.code == -2 && i < 50 { made.forEach { $0.close() } }
  }
  fatalError()
}

/// The radio between the transports: drops nothing unless cut, and can hold the drone's wires to deliver them reversed.
final class Link: @unchecked Sendable {
  let lock = NSLock()
  var ends: [MudTransport] = [], native: Native?, cut = false, hold = 0, held: [(String, [UInt8])] = [], refuseListen = false, lastTtl: UInt32 = 0
  func send(_ from: Int, _ o: Outgoing) throws {
    lock.lock()
    if o.listen {
      lock.unlock()
      if refuseListen { throw RPCError(code: .unavailable, message: "this radio has no listen") }
      return
    }
    lastTtl = o.ttlSecs
    if cut { lock.unlock(); return }
    if let native { lock.unlock(); native.deliver(o.topic, o.wire); return }
    let to = ends[1 - from]
    var now = [(o.topic, o.wire)]
    if from == 1 && hold > 0 {
      held.append((o.topic, o.wire))
      if held.count < hold { lock.unlock(); return }
      hold = 0
      now = held.reversed()
      held = []
    }
    lock.unlock()
    for (t, w) in now { to.deliver(topic: t, wire: w) }
  }
}

struct DroneService: Test_Drone.SimpleServiceProtocol {
  func arm(request r: Test_ArmRequest, context: ServerContext) async throws -> Test_ArmReply {
    switch r.id {
    case 0: throw RPCError(code: .failedPrecondition, message: "battery low")
    case 13: throw CocoaError(.featureUnsupported)
    case 23: return .with { $0.armed = true; $0.by = String(repeating: "x", count: 1000) }
    case 25: throw RPCError(code: .failedPrecondition, message: String(repeating: "é", count: 100))
    case 24, 28, 29:
      while !context.cancellation.isCancelled { await nap(20) }
      heard.withLock { _ = $0.insert(r.id) }
      return .init()
    default: return .with { $0.armed = true; $0.id = r.id; $0.by = context.remotePeer }
    }
  }

  func telemetry(request r: Test_TelemetryRequest, response: RPCWriter<Test_Sample>, context: ServerContext) async throws {
    for i in 0..<r.count {
      if r.failAfter != 0 && i == r.failAfter { throw RPCError(code: .aborted, message: "lost the camera") }
      if r.bigAt != 0 { await nap(50) }
      do { try await response.write(.with { $0.seq = i; if r.bigAt != 0 && i == r.bigAt { $0.pad = Data(count: 1000) } }) } catch {
        if r.bigAt != 0 { stopped.withLock { $0 = true } }
        throw error
      }
    }
  }

  func upload(request: RPCAsyncSequence<Test_Sample, any Error>, context: ServerContext) async throws -> Test_ArmReply { .with { $0.armed = true } }
  func fly(request: Test_ArmRequest, context: ServerContext) async throws -> Test_ArmReply { .with { $0.armed = true } }
}

/// The deadline each call reached its server with, as grpc-swift's own wire carries it.
let reached = Mutex("")
struct Deadline: ServerInterceptor {
  func intercept<Input: Sendable, Output: Sendable>(request: StreamingServerRequest<Input>, context: ServerContext, next: (StreamingServerRequest<Input>, ServerContext) async throws -> StreamingServerResponse<Output>) async throws -> StreamingServerResponse<Output> {
    reached.withLock { $0 = request.metadata[stringValues: "grpc-timeout"].first(where: { _ in true }) ?? "" }
    return try await next(request, context)
  }
}

/// A service whose full name is past 64 bytes.
struct LongName: RegistrableRPCService {
  func registerMethods<T: ServerTransport>(with router: inout RPCRouter<T>) {
    router.registerHandler(forMethod: MethodDescriptor(fullyQualifiedService: String(repeating: "a", count: 70), method: "Arm"), deserializer: ProtobufDeserializer<Test_ArmRequest>(), serializer: ProtobufSerializer<Test_ArmReply>()) { _, _ in
      StreamingServerResponse(error: RPCError(code: .internalError, message: "never"))
    }
  }
}

/// A mudRPC server with no gRPC: Rover's Telemetry sends count items, count-1 down to 0, end on the last; Arm refuses.
final class Native: @unchecked Sendable {
  let lock = NSLock()
  let peer: Peer
  var inbox: [(String, [UInt8])] = [], back: MudTransport?, stopped = false
  init(_ peer: Peer) throws {
    self.peer = peer
    try peer.listen("test.Rover")
    let start = ContinuousClock.now
    Thread { [self] in
      while !self.lock.withLock({ self.stopped }) {
        self.lock.withLock {
          let now = UInt32((ContinuousClock.now - start).components.seconds)
          for w in self.inbox {
            guard let c = try? self.peer.deliver(now: now, topic: w.0, wire: w.1), c.kind == .call else { continue }
            let n = c.bytes.count > 1 ? c.bytes[1] : 0
            if c.method == "Telemetry" && n == 99 { try? self.peer.reply(now: now, to: c, payload: [0x0a, 0xff, 0xff]); continue }
            if c.method == "Arm" { try? self.peer.reply(now: now, to: c, status: 9, payload: [], detail: "native says no"); continue }
            for i in 0..<UInt32(n) { try? self.peer.reply(now: now, to: c, payload: try! Test_Sample.with { $0.seq = UInt32(n) - 1 - i }.serializedBytes(), end: i == UInt32(n) - 1, seq: i) }
          }
          self.inbox.removeAll()
          while let o = try? self.peer.outgoing(now: now) {
            if !o.listen { self.back?.deliver(topic: o.topic, wire: o.wire) }
            try? self.peer.sent()
          }
        }
        Thread.sleep(forTimeInterval: 0.01)
      }
    }.start()
  }
  func deliver(_ t: String, _ w: [UInt8]) { lock.withLock { inbox.append((t, w)) } }
  func stop() { lock.withLock { stopped = true } }
}

let link = Link()
let gcs = MudTransport(peer: try Peer(name: "gcs", key: key, sender: 1, epoch: 1)) { try link.send(0, $0) }
let drone = MudTransport(peer: try Peer(name: "drone", key: key, sender: 2, epoch: 1)) { try link.send(1, $0) }
link.ends = [gcs, drone]
let server = GRPCServer(transport: drone.server, services: [DroneService()], interceptors: [Deadline()])
let serving = Task { try await server.serve() }

// From here, the code a grpc-swift product already has.
try await withGRPCClient(transport: gcs.client) { grpc in
  let client = Test_Drone.Client(wrapping: grpc)
  let r = try await client.arm(arm(7))
  say("arm: armed \(r.armed) id \(r.id) by \(r.by)")
  await fails("refused") { _ = try await client.arm(arm(0)) }
  await fails("handler throws") { _ = try await client.arm(arm(13)) }
  await fails("unknown method") { try await nope(grpc) }

  link.lock.withLock { link.hold = 6 }   // five items and the stream's end, delivered newest first
  say("telemetry reversed: \(try await read(client, telemetry(5)))")
  say("telemetry empty: \(try await read(client, telemetry(0)))")
  await fails("telemetry fails") { _ = try await read(client, telemetry(5, failAfter: 2), partial: "telemetry fails") }
  await fails("upload") { _ = try await client.upload(request: StreamingClientRequest { w in try await w.write(.with { $0.seq = 1 }) }) { try $0.message } }
  await fails("too large") { _ = try await client.arm(arm(1, note: 1000)) }

  link.lock.withLock { link.cut = true }
  var t0 = ContinuousClock.now
  await fails("deadline") { _ = try await client.arm(arm(9), options: options(.seconds(2))) }
  let took = ContinuousClock.now - t0
  say("deadline in time: \(took >= .seconds(2) && took < .milliseconds(3500))")
  let cancelled = Task { try await client.arm(arm(10)) }
  await nap(300)
  cancelled.cancel()
  await fails("cancelled") { _ = try await cancelled.value }
  await fails("past the deadline") { _ = try await client.arm(arm(1), options: options(.seconds(-1))) }
  link.lock.withLock { link.cut = false }
  say("after the cut: armed \(try await client.arm(arm(11)).armed)")

  await fails("a reply past the wire") { _ = try await client.arm(arm(23)) }
  do { _ = try await client.arm(arm(25)) } catch let e as RPCError { say("a detail of 100 two-byte characters: \(e.code), \(e.message.count) kept, \(e.message.utf8.count) bytes") }
  say("two full streams back to back: \(try await read(client, telemetry(63)).split(separator: " ").count) \(try await read(client, telemetry(63)).split(separator: " ").count)")
  await fails("a 64th item") { _ = try await read(client, telemetry(64)) }
  let mid = try await client.telemetry(telemetry(40, bigAt: 2)) { response in
    var seen: [String] = []
    do { for try await s in response.messages { seen.append(String(s.seq)) } } catch { return "\(seen.joined(separator: " ")), \(told(error))" }
    return seen.joined(separator: " ")
  }
  await nap(100)
  say("an item past the wire mid-stream: \(mid), its handler stopped: \(stopped.withLock { $0 })")
  await fails("a call to an empty service name") { try await nope(grpc, service: "") }
  await fails("a call to a service name of 65 bytes") { try await nope(grpc, service: String(repeating: "a", count: 65)) }

  let lonePeer = try Peer(name: "lone", key: key, sender: 9, epoch: 1)
  let lone = MudTransport(peer: lonePeer) { _ in }
  lonePeer.close()
  await nap(200)   // the pump's next round finds the peer gone and closes the transport
  await fails("the peer closed first") { try await withGRPCClient(transport: lone.client) { _ = try await Test_Drone.Client(wrapping: $0).arm(arm(1), options: options(.seconds(2))) } }

  let free16 = 16 - Int(gcs.use { $0.pending })   // a call cancelled after sending keeps its slot until its ttl
  let codes = await withTaskGroup(of: String.self) { g in
    for _ in 0..<free16 { g.addTask { do { try await nope(grpc, options(.seconds(5))); return "ok" } catch { return told(error).split(separator: " ")[0].description } } }
    var seen = Set<String>()
    for await c in g { seen.insert(c) }
    return seen
  }
  say("unknown method, every free slot at once: \(codes == ["unimplemented"] ? "all unimplemented" : "\(codes)")")
  link.lock.withLock { link.cut = true }
  let far = Task { try await client.arm(arm(1), options: options(.seconds(8 * 86400))) }
  await nap(200)
  link.lock.withLock { link.cut = false }
  far.cancel()
  say("a deadline eight days out: ttl \(link.lock.withLock { link.lastTtl })")
  for _ in 0..<5000 { gcs.deliver(topic: "gcs", wire: [UInt8](repeating: 0, count: 40)) }
  say("five thousand junk wires: then armed \(try await client.arm(arm(1)).armed)")

  let reading = Task {
    try await client.telemetry(telemetry(5)) { response in
      var it = response.messages.makeAsyncIterator()
      _ = try await it.next()
      await nap(500)   // every item lands, and the reader is cancelled
      return try await it.next().map { "item \($0.seq)" } ?? "the end"
    }
  }
  await nap(250)
  reading.cancel()
  do { say("cancelled with items unread: \(try await reading.value)") } catch { say("cancelled with items unread: \(told(error))") }
  t0 = ContinuousClock.now
  await fails("a slow reader past its deadline") { _ = try await read(client, telemetry(5), options(.seconds(1)), pause: 600) }
  say("the slow reader's deadline in time: \(ContinuousClock.now - t0 < .seconds(2))")
  say("an infinite timeout: armed \(try await client.arm(arm(1), options: options(nil)).armed)")
  _ = try await client.arm(arm(1), options: options(.seconds(3 * 3600)))
  say("a three-hour ttl reaches the server as: \(reached.withLock { $0 })")
  await fails("a second server on one transport") { try await GRPCServer(transport: drone.server, services: [DroneService()]).serve() }
  await fails("a server that never answers") { _ = try await client.arm(arm(24), options: options(.seconds(1))) }
  await nap(1500)
  say("the server saw its ttl pass: \(heard.withLock { $0.contains(24) })")
}

let link3 = Link()
link3.refuseListen = true
let faults = Mutex(0)
let gcs3 = MudTransport(peer: try Peer(name: "gcs3", key: key, sender: 7, epoch: 1), fault: { _ in faults.withLock { $0 += 1 } }) { try link3.send(0, $0) }
let drone3 = MudTransport(peer: try Peer(name: "drone3", key: key, sender: 8, epoch: 1)) { try link3.send(1, $0) }
link3.ends = [gcs3, drone3]
let server3 = GRPCServer(transport: drone3.server, services: [DroneService()])
let serving3 = Task { try await server3.serve() }
try await withGRPCClient(transport: gcs3.client) { grpc in
  say("a transport refusing a listen: armed \(try await Test_Drone.Client(wrapping: grpc).arm(arm(1), options: options(.seconds(10))).armed), faults heard \(faults.withLock { $0 } > 0 ? "yes" : "no")")
}

final class Box: @unchecked Sendable { var t: MudTransport? }
let linkB = Link()
let callingBack = Box()
let gcsB = MudTransport(peer: try Peer(name: "gcsb", key: key, sender: 19, epoch: 1)) { o in
  DispatchQueue.global().sync { _ = callingBack.t?.use { $0.pending } }   // the radio waits on a thread that calls into its transport
  try linkB.send(0, o)
}
callingBack.t = gcsB
let droneB = MudTransport(peer: try Peer(name: "droneb", key: key, sender: 20, epoch: 1)) { try linkB.send(1, $0) }
linkB.ends = [gcsB, droneB]
let serverB = GRPCServer(transport: droneB.server, services: [DroneService()])
let servingB = Task { try await serverB.serve() }
try await withGRPCClient(transport: gcsB.client) { grpc in
  say("a radio calling back in from another thread: armed \(try await Test_Drone.Client(wrapping: grpc).arm(arm(1), options: options(.seconds(5))).armed)")
}
let deaf = MudTransport(peer: try Peer(name: "deaf", key: key, sender: 31, epoch: 1)) { _ in }
try await withGRPCClient(transport: deaf.client) { grpc in
  let c = Test_Drone.Client(wrapping: grpc)
  await withTaskGroup(of: Void.self) { g in for _ in 0..<16 { g.addTask { _ = try? await c.arm(arm(1), options: options(.seconds(1))) } } }
  await nap(2500)   // past every ttl, which only a tick frees in core
  await fails("after sixteen calls timed out") { _ = try await c.arm(arm(1), options: options(.seconds(1))) }
}
deaf.close()
weak var dropped = MudTransport(peer: try Peer(name: "dropped", key: key, sender: 32, epoch: 1)) { _ in }
await nap(100)
say("a transport dropped unclosed is freed: \(dropped == nil)")
let alone = MudTransport(peer: try Peer(name: "alone", key: key, sender: 29, epoch: 1)) { _ in }
let running = Task { try await GRPCClient(transport: alone.client).runConnections() }
await nap(100)
alone.close()
_ = await running.result
say("runConnections after close: returned")
let selfish = Box()
selfish.t = MudTransport(peer: try Peer(name: "selfish", key: key, sender: 10, epoch: 1)) { _ in selfish.t?.close() }
await nap(300)
say("closed from its own send: \(thrown { selfish.t?.close() })")

let pair = try udpSet(2)
let gcsU = MudTransport(peer: try Peer(name: "gcsu", key: key, sender: 11, epoch: 1), udp: pair[0])
let droneU = MudTransport(peer: try Peer(name: "droneu", key: key, sender: 12, epoch: 1), udp: pair[1])
let serverU = GRPCServer(transport: droneU.server, services: [DroneService()])
let servingU = Task { try await serverU.serve() }
await nap(200)   // a server answers once its listen has reached its socket
try await withGRPCClient(transport: gcsU.client) { grpc in
  let overUdp = Test_Drone.Client(wrapping: grpc)
  say("over UDP: armed \(try await overUdp.arm(arm(1), options: options(.seconds(5))).armed), telemetry \(try await read(overUdp, telemetry(5)))")
}
let lostSocket = try Udp(bind: "127.0.0.1:0", peers: ["127.0.0.1:\(try pair[1].port)"])
let lost = MudTransport(peer: try Peer(name: "lost", key: key, sender: 13, epoch: 1), udp: lostSocket)
lostSocket.close()
await nap(100)
await fails("its socket closed") { try await withGRPCClient(transport: lost.client) { _ = try await Test_Drone.Client(wrapping: $0).arm(arm(1), options: options(.seconds(2))) } }
say("a public bind: \(thrown { _ = try Udp(bind: "0.0.0.0:0", peers: ["127.0.0.1:9"]) }), a public peer: \(thrown { _ = try Udp(bind: "127.0.0.1:0", peers: ["8.8.8.8:53"]) }), this library public: \(Udp.internet)")
say("an octal address: \(thrown { _ = try Udp(bind: "127.0.0.1:0", peers: ["010.0.0.1:7000"]) }), no peer: \(thrown { _ = try Udp(bind: "127.0.0.1:0", peers: []) })")

let longName = MudTransport(peer: try Peer(name: "long", key: key, sender: 14, epoch: 1)) { _ in }
await fails("a service name past 64 bytes") { try await GRPCServer(transport: longName.server, services: [LongName()]).serve() }

let three = try udpSet(3)
let nodes = try three.enumerated().map { MudTransport(peer: try Peer(name: "node\($0.offset)", key: key, sender: 21 + UInt32($0.offset), epoch: 1), udp: $0.element) }
let serverT = GRPCServer(transport: nodes[1].server, services: [DroneService()])
let servingT = Task { try await serverT.serve() }
await nap(200)   // a server answers once its listen has reached its socket
try await withGRPCClient(transport: nodes[0].client) { grpc in
  var armed = 0
  for _ in 0..<10 where try await Test_Drone.Client(wrapping: grpc).arm(arm(1), options: options(.seconds(5))).armed { armed += 1 }
  say("three on UDP, one serving nothing: \(armed) of 10 armed")
}

do {
  let linkC = Link()
  let gcsC = MudTransport(peer: try Peer(name: "gcsc", key: key, sender: 17, epoch: 1)) { try linkC.send(0, $0) }
  let droneC = MudTransport(peer: try Peer(name: "dronec", key: key, sender: 18, epoch: 1)) { try linkC.send(1, $0) }
  linkC.ends = [gcsC, droneC]
  let serverC = GRPCServer(transport: droneC.server, services: [DroneService()])
  let servingC = Task { try await serverC.serve() }
  let waiting = Task { try await withGRPCClient(transport: gcsC.client) { _ = try await Test_Drone.Client(wrapping: $0).arm(arm(28), options: options(.seconds(30))) } }
  await nap(300)
  droneC.close()
  gcsC.close()
  await fails("a call when its transport closes") { try await waiting.value }
  _ = await servingC.result
  say("a handler hears its server close: \(heard.withLock { $0.contains(28) })")
}

do {
  let linkD = Link()
  let gcsD = MudTransport(peer: try Peer(name: "gcsd", key: key, sender: 34, epoch: 1)) { try linkD.send(0, $0) }
  let droneD = MudTransport(peer: try Peer(name: "droned", key: key, sender: 35, epoch: 1)) { try linkD.send(1, $0) }
  linkD.ends = [gcsD, droneD]
  let serverD = GRPCServer(transport: droneD.server, services: [DroneService()])
  let servingD = Task { try await serverD.serve() }
  let waiting = Task { try await withGRPCClient(transport: gcsD.client) { _ = try await Test_Drone.Client(wrapping: $0).arm(arm(29), options: options(.seconds(30))) } }
  await nap(300)
  serverD.beginGracefulShutdown()
  await nap(200)
  servingD.cancel()   // grpc-swift's shutdown past a deadline
  _ = await servingD.result
  do { try await waiting.value; say("a shutdown past its deadline: no error") } catch { say("a shutdown past its deadline: \(told(error)), the handler heard it: \(heard.withLock { $0.contains(29) })") }
  gcsD.close()
  droneD.close()
}

// A native peer answers the Rover service with the plain API: a stream ends on its last item, and a refusal has a detail.
let native = try Native(try Peer(name: "rover", key: key, sender: 3, epoch: 1))
let link2 = Link()
let gcs2 = MudTransport(peer: try Peer(name: "gcs2", key: key, sender: 4, epoch: 1)) { try link2.send(0, $0) }
link2.ends = [gcs2]
link2.native = native
native.lock.withLock { native.back = gcs2 }
try await withGRPCClient(transport: gcs2.client) { grpc in
  let rover = Test_Rover.Client(wrapping: grpc)
  let stream = try await rover.telemetry(telemetry(3)) { r in try await r.messages.reduce(into: []) { $0.append(String($1.seq)) }.joined(separator: " ") }
  say("native stream: \(stream)")
  await fails("native refusal") { _ = try await rover.arm(arm(1)) }
  await fails("a reply that does not decode") { _ = try await rover.telemetry(telemetry(99)) { r in for try await _ in r.messages {} } }
}
native.stop()

let linkE = Link()
let gcsE = MudTransport(peer: try Peer(name: "gcse", key: key, sender: 36, epoch: 1)) { try linkE.send(0, $0) }
let droneE = MudTransport(peer: try Peer(name: "dronee", key: key, sender: 37, epoch: 1)) { try linkE.send(1, $0) }
linkE.ends = [gcsE, droneE]
let serverE = GRPCServer(transport: droneE.server, services: [DroneService()])
let servingE = Task { try await serverE.serve() }
await nap(100)   // the server listens before its callers
let ask: [UInt8] = try telemetry(20).serializedBytes()
let crowd = try (0..<48).map { try Peer(name: "crowd\($0)", key: key, sender: 100 + UInt32($0), epoch: 1) }   // 48 callers owed a stream each, past the 32 a second: the next caller waits its turn, not behind them all
for p in crowd {
  _ = try p.call(now: 0, service: "test.Drone", method: "Telemetry", request: ask, ttlSecs: 5)
  while let o = try p.outgoing(now: 0) {
    if !o.listen { droneE.deliver(topic: o.topic, wire: o.wire) }
    try p.sent()
  }
}
await nap(300)
try await withGRPCClient(transport: gcsE.client) { grpc in
  let armed = (try? await Test_Drone.Client(wrapping: grpc).arm(arm(1), options: options(.seconds(3))).armed) ?? false
  say("a caller behind 48 owed a stream each: armed \(armed)")
}

let spare = try Peer(name: "spare", key: key, sender: 30, epoch: 1)
let saved = try spare.save()
say("restored at a higher epoch: \(thrown { _ = try Peer.restore(saved, epoch: 2) }), at the same: \(thrown { _ = try Peer.restore(saved, epoch: 1) }), a short copy: \(thrown { _ = try Peer.restore(Array(saved.dropLast()), epoch: 3) })")
spare.close()
say("a closed peer: \(thrown { try spare.listen("x") }), pending \(spare.pending), a short key: \(thrown { _ = try Peer(name: "k", key: [1], sender: 1, epoch: 1) })")
for s in [server, server3, serverU, serverT, serverB, serverE] { s.beginGracefulShutdown() }
for t in [serving, serving3, servingU, servingT, servingB, servingE] { _ = await t.result }
say("every server returned from serve")
for t in [gcs, drone, gcs2, gcs3, drone3, gcsU, droneU, lost, longName, gcsB, droneB, gcsE, droneE] + nodes { t.close() }
crowd.forEach { $0.close() }
for u in pair + three { u.close() }
