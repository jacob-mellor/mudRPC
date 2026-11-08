// Sensor telemetry over UDP in Swift: two sensors publish readings a gateway listens to, and the gateway calibrates one
// by a call; three mudRPC peers over UDP in one process, pumped by one loop, the messages protoc-gen-swift's.
import Foundation

let key = (1...32).map { UInt8($0) }   // the fleet key; a product provisions 32 random bytes to every peer
let epoch: UInt32 = 1
func degrees(_ centi: Int32) -> String { String(format: "%.2fC", Double(centi) / 100) }

/// Sockets on free loopback ports, each listing the others: the ports found by binding, then bound again listed.
func sockets(_ n: Int) throws -> [Udp] {
  let probes = try (0..<n).map { _ in try Udp(bind: "127.0.0.1:0", peers: ["127.0.0.1:9"]) }
  let ports = try probes.map { try $0.port }
  probes.forEach { $0.close() }
  return try ports.map { p in try Udp(bind: "127.0.0.1:\(p)", peers: ports.filter { $0 != p }.map { "127.0.0.1:\($0)" }) }
}

let gateway = try Peer(name: "gateway", key: key, sender: 1, epoch: epoch)
let sensors = try (1...2).map { try Peer(name: "sensor-\($0)", key: key, sender: UInt32($0 + 1), epoch: epoch) }
let peers = [gateway] + sensors   // the gateway first: its listens reach its socket before the readings do
let wires = try sockets(peers.count)
try gateway.listen("sensors.Readings")
try sensors[1].listen("demo.sensors.Sensor")
for (i, centi) in [2150, 1900].enumerated() {
  try sensors[i].publish(now: 0, topic: "sensors.Readings", payload: try Demo_Sensors_Reading.with { $0.sensor = "sensor-\(i + 1)"; $0.centiDegrees = Int32(centi) }.serializedBytes(), ttlSecs: 60)
}

var readings: [Demo_Sensors_Reading] = []
var done = false
let start = ContinuousClock.now
while !done {
  let now = UInt32((ContinuousClock.now - start).components.seconds)
  for (peer, wire) in zip(peers, wires) {
    while let o = try peer.outgoing(now: now) {
      do { try wire.send(o) } catch { break }
      try peer.sent()
    }
    while let (topic, bytes) = try wire.poll() {
      let got = try peer.deliver(now: now, topic: topic, wire: bytes)
      switch got.kind {
      case .event:
        readings.append(try Demo_Sensors_Reading(serializedBytes: got.bytes))
        guard readings.count == 2 else { break }
        for r in readings.sorted(by: { $0.sensor < $1.sensor }) { print("reading \(r.sensor) \(degrees(r.centiDegrees))") }
        _ = try gateway.call(now: now, service: "demo.sensors.Sensor", method: "Calibrate", request: try Demo_Sensors_Offset.with { $0.centiDegrees = 50 }.serializedBytes(), ttlSecs: 10)
      case .call:
        let offset = try Demo_Sensors_Offset(serializedBytes: got.bytes)
        try peer.reply(now: now, to: got, payload: try Demo_Sensors_Ack.with { $0.sensor = "sensor-2"; $0.offsetCentiDegrees = offset.centiDegrees }.serializedBytes())
      case .reply:
        let ack = try Demo_Sensors_Ack(serializedBytes: got.bytes)
        print("calibrated \(ack.sensor) by \(degrees(ack.offsetCentiDegrees))")
        done = true
      default: break
      }
    }
    _ = try peer.tick(now: now)
  }
  usleep(5000)
}
peers.forEach { $0.close() }
wires.forEach { $0.close() }
