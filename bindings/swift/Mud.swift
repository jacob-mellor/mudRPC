// mudRPC for Swift: one file over the C ABI, `mudrpc.h` imported as the module CMudRPC by `bindings/c/module.modulemap`.
// No dependencies beyond the standard library and Foundation's lock.
//
// A call is a message; a reply may come hours later or expire; nothing blocks. You poll: drain `outgoing(now:)` into
// your transport and call `sent()`, feed what arrives to `deliver(now:topic:wire:)`, call `tick(now:)`. `now` is your
// uptime in seconds.
import CMudRPC
import Foundation

/// A code from mudrpc.h's enum, the same number in every binding: -1 invalid argument, -2 full, -3 too large, -4 corrupt,
/// -5 bad memory, -6 epoch not higher, -7 unprovisioned, -8 counter exhausted, -9 this second's sends spent, -10 busy.
public struct MudError: Error, CustomStringConvertible, Sendable {
  public let code: Int32
  public var description: String {
    let names: [Int32: String] = [-1: "invalid argument", -2: "full", -3: "too large", -4: "corrupt", -5: "bad memory", -6: "epoch not higher", -7: "unprovisioned (sender 0)", -8: "counter exhausted", -9: "this second's sends spent", -10: "busy: another thread holds this peer", -99: "panic"]
    return "mudRPC: \(names[code] ?? String(code))"
  }
}

public enum Kind: Int32, Sendable { case none, call, reply, event, expired, dropped, flood, rollback }

/// A verdict. `dropped` names why a wire was refused: 1 version, 2 open, 3 decode, 4 duplicate, 5 no pending call, 6 topic,
/// 7 seq, 8 id, 9 replay, 10 rate, with `status` its detail.
public struct Incoming: Sendable {
  public var kind: Kind
  public var id: [UInt8] = []
  public var topic = "", method = "", replyTo = "", detail = ""
  public var bytes: [UInt8] = []
  public var ttlSecs: UInt32 = 0, seq: UInt32 = 0, status: UInt32 = 0
  public var end = false
  public var dropped: Int32 = 0
  public var sender: UInt32 = 0
}

/// An item for the transport: a wire to send on `topic`, kept for `ttlSecs`, or a topic to listen to.
public struct Outgoing: Sendable {
  public var listen: Bool
  public var topic: String
  public var wire: [UInt8]
  public var ttlSecs: UInt32
}

func check(_ code: Int32) throws -> Int32 {
  if code < 0 { throw MudError(code: code) }
  return code
}

func withBuf<R>(_ b: [UInt8], _ f: (mud_buf) throws -> R) rethrows -> R {
  try b.withUnsafeBufferPointer { try f(mud_buf(ptr: $0.baseAddress, len: $0.count)) }
}

func bytes(_ b: mud_buf) -> [UInt8] { b.len == 0 ? [] : Array(UnsafeBufferPointer(start: b.ptr, count: b.len)) }
func text(_ b: mud_buf) -> String { String(decoding: bytes(b), as: UTF8.self) }

/// A mudRPC peer. `name` is its inbox topic; `key` the 32-byte fleet key; `sender` the roster number (0 cannot send);
/// `epoch` a number the host persists beside the key and raises on every boot and restore, written to storage first.
///
/// A signed peer believes 16 senders; `hub: true` is the hub shape, 256, for the host that believes a fleet. The line
/// past either is `MudError` -2. Not thread-safe: one thread drives a peer. The bytes from `save()` contain the key;
/// store them as you store the key.
public final class Peer {
  public static let cipherChaCha20Poly1305: UInt32 = 0, cipherAes256Gcm: UInt32 = 1
  public static let cipherDefault = cipherAes256Gcm   // AES, the NIST algorithm buyers ask for; ChaCha20-Poly1305 smaller and faster without an AES instruction
  let mem: UnsafeMutableRawPointer   // lives as long as the Peer, so a thread still inside it never meets freed memory
  let size: Int
  var scratch = [UInt8](repeating: 0, count: 64 + 512)
  var plain = [UInt8](repeating: 0, count: 512)

  init(size: Int) {
    self.size = size
    mem = UnsafeMutableRawPointer.allocate(byteCount: size, alignment: mud_align())
    mem.initializeMemory(as: UInt8.self, repeating: 0, count: size)
  }

  public convenience init(name: String, key: [UInt8], sender: UInt32, epoch: UInt32, seed: [UInt8]? = nil, budget: UInt32 = 1000, hub: Bool = false, cipher: UInt32 = Peer.cipherDefault) throws {
    self.init(size: hub ? mud_hub_size() : mud_size())
    guard key.count == 32, seed == nil || seed!.count == 32 else { throw MudError(code: -1) }
    let code = withBuf(Array(name.utf8)) { n in withBuf(key) { k in withBuf(seed ?? []) { s in
      var cfg = mud_config(name: n, key: k, sender: sender, epoch: epoch, seed: s, budget: budget, hub: hub ? 1 : 0, cipher: cipher)
      return mud_new(mem, size, &cfg)
    } } }
    _ = try check(code)
  }

  /// A peer from `save()` bytes, from this same build of the library, with a higher epoch from the host.
  public static func restore(_ saved: [UInt8], epoch: UInt32) throws -> Peer {
    guard saved.count == mud_size() || saved.count == mud_hub_size() else { throw MudError(code: -4) }
    let p = Peer(size: saved.count)
    saved.withUnsafeBytes { p.mem.copyMemory(from: $0.baseAddress!, byteCount: saved.count) }
    _ = try check(mud_resume(p.mem, p.size, epoch))
    return p
  }

  deinit {
    close()
    mem.deallocate()
  }

  /// Ends the peer: its block is zeroed, keys included, once no other thread is inside it. Every later call is
  /// `MudError` -4, the counts 0; the memory goes with the Peer.
  public func close() {
    for _ in 0..<1000 {
      if mud_drop(mem, size) != -10 { return }
      Thread.sleep(forTimeInterval: 0.001)   // busy: an entry on another thread, which ends in microseconds
    }
  }

  /// The peer's bytes, to write to flash. They contain the key, and cannot send until `restore` has resumed them.
  public func save() throws -> [UInt8] {
    let out = UnsafeMutableRawPointer.allocate(byteCount: size, alignment: mud_align())
    defer { out.deallocate() }
    _ = try check(mud_save(mem, size, out, size))
    return Array(UnsafeRawBufferPointer(start: out, count: size))
  }

  /// A higher epoch for a live peer; clears `MudError` -8.
  public func renew(epoch: UInt32) throws { _ = try check(mud_renew(mem, size, epoch)) }
  /// The next fleet key, before the flag day.
  public func stage(key: [UInt8]) throws { _ = try check(withBuf(key) { mud_stage(mem, size, $0) }) }
  public func rotate() throws { _ = try check(mud_rotate(mem, size)) }
  public func retire() throws { _ = try check(mud_retire(mem, size)) }
  /// Which key slot opened the last accepted wire: 0 current, 1 previous, 2 next.
  public func opened() throws -> Int32 { try check(mud_opened(mem, size)) }

  /// Signed peers: a line for `sender`, its public key and the next one during a rotation.
  public func roster(sender: UInt32, key: [UInt8], next: [UInt8]? = nil) throws {
    _ = try check(withBuf(key) { k in withBuf(next ?? []) { n in mud_roster(mem, size, sender, k, n) } })
  }

  public func revoke(sender: UInt32) throws { _ = try check(mud_revoke(mem, size, sender)) }

  /// The 32-byte public key of a seed, for rosters.
  public static func publicKey(seed: [UInt8]) throws -> [UInt8] {
    var out = [UInt8](repeating: 0, count: 32)
    _ = try check(withBuf(seed) { s in out.withUnsafeMutableBufferPointer { mud_public_key(s, $0.baseAddress) } })
    return out
  }

  /// A call; its replies carry the id returned. It expires after `ttlSecs`.
  public func call(now: UInt32, service: String, method: String, request: [UInt8], ttlSecs: UInt32) throws -> [UInt8] {
    var id = mud_id()
    _ = try check(withBuf(Array(service.utf8)) { s in withBuf(Array(method.utf8)) { m in withBuf(request) { r in mud_call(mem, size, now, s, m, r, ttlSecs, &id) } } })
    return withUnsafeBytes(of: id.bytes) { Array($0) }
  }

  public func publish(now: UInt32, topic: String, payload: [UInt8], ttlSecs: UInt32) throws {
    _ = try check(withBuf(Array(topic.utf8)) { t in withBuf(payload) { p in mud_publish(mem, size, now, t, p, ttlSecs) } })
  }

  public func listen(_ topic: String) throws { _ = try check(withBuf(Array(topic.utf8)) { mud_listen(mem, size, $0) }) }

  /// Answers a `.call` verdict: `status` gRPC's number and `detail` its text; `seq` 0..63, `end` on the last item only.
  public func reply(now: UInt32, to call: Incoming, status: UInt32 = 0, payload: [UInt8], end: Bool = true, seq: UInt32 = 0, detail: String = "") throws {
    guard call.kind == .call, call.id.count == 12 else { throw MudError(code: -1) }
    var id = mud_id()
    withUnsafeMutableBytes(of: &id.bytes) { $0.copyBytes(from: call.id) }
    let code = withBuf(Array(call.topic.utf8)) { t in withBuf(Array(call.method.utf8)) { m in withBuf(Array(call.replyTo.utf8)) { r in withBuf(call.bytes) { b in withBuf(payload) { p in withBuf(Array(detail.utf8)) { d in
      var c = mud_incoming(kind: 1, id: id, topic: t, method: m, reply_to: r, detail: mud_buf(), bytes: b, ttl_secs: call.ttlSecs, seq: call.seq, status: 0, end: call.end ? 1 : 0, dropped: 0, sender: 0)
      return mud_reply(mem, size, now, &c, seq, status, d, p, end ? 1 : 0)
    } } } } } }
    _ = try check(code)
  }

  /// Every wire gets a verdict; a `.dropped` one names why.
  public func deliver(now: UInt32, topic: String, wire: [UInt8]) throws -> Incoming {
    try withBuf(Array(topic.utf8)) { t in try withBuf(wire) { w in try plain.withUnsafeMutableBufferPointer { s in
      var out = mud_incoming()
      _ = try check(mud_deliver(mem, size, now, t, w, mud_mutbuf(ptr: s.baseAddress, len: s.count), &out))
      return incoming(out)   // the verdict points into the topic and the scratch: copied while they live
    } } }
  }

  public func tick(now: UInt32) throws -> Incoming? {
    var out = mud_incoming()
    return try check(mud_tick(mem, size, now, &out)) == 0 ? nil : incoming(out)
  }

  /// The next item for the transport, left in place until `sent()`.
  public func outgoing(now: UInt32) throws -> Outgoing? {
    try scratch.withUnsafeMutableBufferPointer { s in
      var out = mud_out()
      let kind = try check(mud_outgoing(mem, size, now, mud_mutbuf(ptr: s.baseAddress, len: s.count), &out))
      return kind == 0 ? nil : Outgoing(listen: kind == 2, topic: text(out.topic), wire: bytes(out.wire), ttlSecs: out.ttl_secs)
    }
  }

  public func sent() throws { _ = try check(mud_sent(mem, size)) }

  /// Live seen or sender entries evicted for want of room; rising under load means replay protection is degraded.
  public var pressure: UInt32 { mud_pressure(mem, size) }
  /// Calls in flight, of 16: at 16 a call is `MudError` -2.
  public var pending: UInt32 { mud_pending(mem, size) }
  /// Items queued for the transport, of 16: at 16 every producer is `MudError` -2.
  public var queued: UInt32 { mud_queued(mem, size) }
  /// The operator's counters, kept across restores: managed (sealer taken for a change), rollbacks, floods, dropped.
  public var counts: (managed: UInt32, rollbacks: UInt32, floods: UInt32, dropped: UInt32) {
    (mud_count(mem, size, 0), mud_count(mem, size, 1), mud_count(mem, size, 2), mud_count(mem, size, 3))
  }

  func incoming(_ r: mud_incoming) -> Incoming {
    Incoming(kind: Kind(rawValue: r.kind) ?? .none, id: withUnsafeBytes(of: r.id.bytes) { Array($0) }, topic: text(r.topic), method: text(r.method), replyTo: text(r.reply_to), detail: text(r.detail),
             bytes: bytes(r.bytes), ttlSecs: r.ttl_secs, seq: r.seq, status: r.status, end: r.end != 0, dropped: r.dropped, sender: r.sender)
  }
}

/// A product's UDP: each wire to every listed peer, on private addresses alone unless the library was built with the
/// cargo feature `internet`, which no release is. A channel's pump and the owner's close never meet inside the library.
public final class Udp: @unchecked Sendable {
  var udp: OpaquePointer?
  let held = NSLock()
  var scratch = [UInt8](repeating: 0, count: 64 + 512)

  /// Binds "ip:port" and sends to each peer's, at least one; a bad or public address, or no peer, is `MudError` -1
  /// ("0.0.0.0" is public), a bind the system refuses -2.
  public init(bind: String, peers: [String]) throws {
    var code: Int32 = 0
    udp = withBuf(Array(bind.utf8)) { b in withBuf(Array(peers.joined(separator: " ").utf8)) { p in mud_udp_new(b, p, &code) } }
    if udp == nil { throw MudError(code: code) }
  }

  deinit { close() }

  /// Whether this library may bind or send to a public address.
  public static var internet: Bool { mud_internet() == 1 }

  func live<R>(_ f: (OpaquePointer) throws -> R) throws -> R {
    held.lock()
    defer { held.unlock() }
    guard let u = udp else { throw MudError(code: -1) }   // the socket is closed
    return try f(u)
  }

  /// The bound port, the one chosen for port 0.
  public var port: Int32 { get throws { try live { try check(mud_udp_port($0)) } } }

  /// One outgoing item: a listen keeps wires on its topic from then, every other topic dropped unread; a send throws
  /// -2 when no peer took the wire, so the item stays queued.
  public func send(_ item: Outgoing) throws {
    _ = try live { u in try check(withBuf(Array(item.topic.utf8)) { t in withBuf(item.wire) { w in item.listen ? mud_udp_listen(u, t) : mud_udp_send(u, t, w) } }) }
  }

  /// The next (topic, wire) from a listed peer, or nil.
  public func poll() throws -> (topic: String, wire: [UInt8])? {
    try live { u in
      try scratch.withUnsafeMutableBufferPointer { s in
        var topic = mud_buf(), wire = mud_buf()
        return try check(mud_udp_poll(u, mud_mutbuf(ptr: s.baseAddress, len: s.count), &topic, &wire)) == 1 ? (text(topic), bytes(wire)) : nil
      }
    }
  }

  public func close() {
    held.lock()
    defer { held.unlock() }
    mud_udp_free(udp)
    udp = nil
  }
}
