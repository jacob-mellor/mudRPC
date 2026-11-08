// mudRPC as gRPC's transport in Swift: the client and service protoc-gen-grpc-swift-2 generates run over mudRPC
// unchanged, `withGRPCClient(transport: mud.client)` and `GRPCServer(transport: mud.server, services:)` as with any
// grpc-swift transport; building the transport is the one line that differs. Beside Mud.swift, with grpc-swift 2's
// GRPCCore, whose ClientTransport and ServerTransport this implements after its GRPCInProcessTransport (Copyright the
// gRPC Authors, Apache License 2.0).
//
// Not a drop-in where gRPC needs a connection: a message of about 460 bytes, a service's full name of at most 64 bytes,
// no metadata, unary and server-streaming calls only, at most 63 items a stream, the first reply wins, and 16 calls in
// flight. A server sends 32 replies a second, half what a receiver takes from one sender, its callers in turn, and a
// client keeps under 32 calls a second. A timeout is the call's ttl, a week at most; a call cancelled after it was sent
// keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
import Foundation
import GRPCCore

/// The detail of the reply that ends a server stream and carries no item. A reply with end, OK and no detail is the
/// stream's last item, as the native API sends it.
public let streamEnd = "end"
let week: UInt32 = 604800, maxItems: UInt32 = 63, inboxCap = 1024, pace = 32, detailBytes = 128   // a message's longest life, a stream's items, wires or calls waiting, replies a second, a detail's bytes
let patience = Duration.seconds(1)   // how long a refused item is tried

/// A detail as text, at most 128 bytes of UTF-8, cut on a character.
func clip(_ s: String) -> String {
  var out = String.UnicodeScalarView(), n = 0
  for u in s.unicodeScalars {
    n += UTF8.width(u)
    if n > detailBytes { break }
    out.append(u)
  }
  return String(out)
}

func status(_ code: Status.Code, _ message: String) -> Status { Status(code: code, message: message) }

func refused(_ e: MudError) -> Status {
  [-2, -3, -9].contains(e.code) ? status(.resourceExhausted, e.description) : status(e.code == -1 ? .invalidArgument : .internalError, e.description)
}

/// A grpc-swift transport over one peer, its client and its server, pumped on a thread of its own.
///
/// The peer is the transport's while it lives: reach it only through `use`, and close the transport before the peer.
/// The radio is a `Udp`, which the pump sends through and reads in its own round, or a function taking each outgoing
/// item, with what arrives handed to `deliver`. `ttlSecs` is the life of a call made without a timeout; `fault` hears
/// every error the radio throws, and the item it threw on is tried for a second, then let go.
public final class MudTransport: @unchecked Sendable {   // every field below is guarded by `lock`, the peer's one thread
  public typealias Radio = @Sendable (Outgoing) throws -> Void
  let lock = NSRecursiveLock()   // recursive: a stream's continuation calls back into the transport as it finishes
  let peer: Peer
  let udp: Udp?
  let radio: Radio
  let fault: @Sendable (any Error) -> Void
  let ttl: UInt32
  let start = ContinuousClock.now   // monotonic, as a deadline must be: a wall clock stepped forward would expire every call at once
  let wake = DispatchSemaphore(value: 0)
  var closed = false, clientShut = false
  let mail = NSLock()   // the inbox's own, so a pump delivering into another transport never waits on that one's pump
  var inbox: [(String, [UInt8], ContinuousClock.Instant)] = [], taking = true   // guarded by `mail`; each wire with its arrival
  var faults: [any Error] = []   // told after the lock is let go, as the radio is called
  var pending: [[UInt8]: ClientCall] = [:]
  var open: [ObjectIdentifier: ClientCall] = [:]
  var idle: [AsyncStream<Void>.Continuation] = []
  var kinds: [String: MethodDescriptor.RPCType?] = [:], services: [String] = [], toListen: [String] = [], badService: String?
  var accept: AsyncStream<ServerCall>.Continuation?
  var queued: [ObjectIdentifier: ServerCall] = [:], running: [ObjectIdentifier: ServerCall] = [:]
  var answers: [UInt32: [Answer]] = [:], order: [UInt32] = []   // the senders owed a reply, each once, the next to send first
  var paceSec = UInt32.max, paced = 0, refusedSince: ContinuousClock.Instant?

  public convenience init(peer: Peer, udp: Udp, ttlSecs: UInt32 = 60, fault: @escaping @Sendable (any Error) -> Void = { _ in }) {
    self.init(peer, udp, { try udp.send($0) }, ttlSecs, fault)
  }

  public convenience init(peer: Peer, ttlSecs: UInt32 = 60, fault: @escaping @Sendable (any Error) -> Void = { _ in }, radio: @escaping Radio) {
    self.init(peer, nil, radio, ttlSecs, fault)
  }

  init(_ peer: Peer, _ udp: Udp?, _ radio: @escaping Radio, _ ttlSecs: UInt32, _ fault: @escaping @Sendable (any Error) -> Void) {
    self.peer = peer
    self.udp = udp
    self.radio = radio
    self.fault = fault
    ttl = max(1, min(ttlSecs, week))
    hear()
    Thread { [weak self] in
      while let (w, idle) = { () -> (DispatchSemaphore, Bool)? in   // the transport is held for a round, never across the wait, so one dropped unclosed goes
        guard let t = self, t.round() else { return nil }
        return (t.wake, t.locked { t.udp == nil && t.peer.pending == 0 && t.peer.queued == 0 && t.answers.isEmpty && t.toListen.isEmpty && t.refusedSince == nil })
      }() {
        _ = w.wait(timeout: idle ? .distantFuture : .now() + .milliseconds(10))   // nothing timed is waiting, core's slots included: a deliver, a call or a close wakes it
      }
    }.start()
  }

  deinit {
    wake.signal()   // the pump, asleep, wakes to find the transport gone
  }

  /// The client, for `withGRPCClient(transport:)` or `GRPCClient(transport:)`.
  public var client: Client { Client(t: self) }
  /// The server, for `GRPCServer(transport:services:)` or `withGRPCServer(transport:services:)`.
  public var server: Server { Server(t: self) }

  /// A wire from the transport. Past 1024 waiting for the pump, a wire is dropped, as core would drop a flood.
  public func deliver(topic: String, wire: [UInt8]) {
    mail.withLock {
      if !taking || inbox.count >= inboxCap { return }
      inbox.append((topic, wire, .now))
      wake.signal()
    }
  }

  /// The peer, between rounds of the pump: save, stage, rotate, roster, counts.
  public func use<R>(_ act: (Peer) throws -> R) rethrows -> R {
    try locked {
      defer { wake.signal() }
      return try act(peer)
    }
  }

  /// Stops the pump, ends every call in flight with UNAVAILABLE, cancels every handler's context, and lets the client's
  /// `connect()` return.
  public func close() {
    locked {
      clientShut = true
      shutDown()
    }
  }

  /// The transport closed, by its owner or by a peer or socket gone; a client's calls are UNAVAILABLE until its owner shuts it.
  func shutDown() {
    locked {
      defer { settle() }
      if closed { return }
      closed = true
      mail.withLock { taking = false; inbox.removeAll() }
      for c in Array(open.values) + Array(pending.values) { c.end(status(.unavailable, "the transport is closed"), cut: true) }
      pending.removeAll()
      accept?.finish()
      accept = nil
      for x in running.values { x.handle?.cancel() }   // a closed server's handlers hear it
      answers.removeAll()
      order.removeAll()
      wake.signal()
    }
  }

  func locked<R>(_ f: () throws -> R) rethrows -> R {
    lock.lock()
    defer { lock.unlock() }
    return try f()
  }

  func now() -> UInt32 { UInt32(clamping: (ContinuousClock.now - start).components.seconds) }
  func tell(_ e: any Error) { faults.append(e) }
  /// The faults the pump met, told with no lock held, so a fault callback may call back in from any thread.
  func flush() {
    for e in locked({ defer { faults.removeAll() }; return faults }) { fault(e) }
  }
  func shut(_ why: String) {
    let was = closed
    shutDown()
    if !was { tell(RPCError(code: .unavailable, message: why)) }
  }

  /// The listens now: the peer's, then over `Udp` each at the head of the outbox into the socket, heard before the init or
  /// `configure` that asked returns. The pump hears before it takes an item, so it never holds a listen another thread's
  /// `hear` could take from under its `sent()`; a full outbox or a refused send leaves it to the next round.
  func hear() {
    while let s = toListen.first, (try? peer.listen(s)) != nil { toListen.removeFirst() }
    guard let udp else { return }
    while let o = try? peer.outgoing(now: now()), o.listen, (try? udp.send(o)) != nil, (try? peer.sent()) != nil {}
  }

  /// One round of the pump; false once the transport is closed. The radio is called with no lock held, so it may hop
  /// to a thread that is calling into this transport; the item stays the peer's until `sent()`.
  func round() -> Bool {
    defer { flush() }
    guard locked({ () -> Bool in
      if closed { return false }
      let now = self.now()
      _ = step {
        for w in mail.withLock({ defer { inbox.removeAll() }; return inbox }) {
          if let e = try? peer.deliver(now: now, topic: w.0, wire: w.1) { land(e, w.2) }   // a wire the transport mangled: no verdict to act on
        }
        if let udp {
          do {
            for _ in 0..<inboxCap {
              guard let w = try udp.poll() else { break }
              if let e = try? peer.deliver(now: now, topic: w.topic, wire: w.wire) { land(e, .now) }
            }
          } catch {
            shut("the socket is closed")
            return
          }
        }
        while let e = try peer.tick(now: now) { land(e, .now) }
        answer(now)
      }
      return !closed
    }) else { return false }
    while let o = locked({ () -> Outgoing? in hear(); return closed ? nil : step { try peer.outgoing(now: now()) } ?? nil }) {
      var taken = true
      do { try radio(o) } catch {
        taken = false
        locked { udp != nil && (error as? MudError)?.code == -1 ? shut("the socket is closed") : tell(error) }
      }
      let more = locked { () -> Bool in
        if closed { return false }
        if !taken {
          let t = ContinuousClock.now
          if refusedSince == nil { refusedSince = t }
          if t - refusedSince! < patience { return false }   // the next round tries it again; past a second it is let go
        }
        refusedSince = nil
        return step { try peer.sent() }
      }
      if !more { break }
    }
    return locked { !closed }
  }

  /// A step on the peer: a peer that is gone closes the transport, any other error is a fault.
  func step<R>(_ f: () throws -> R) -> R? {
    do { return try f() } catch let e as MudError where [-1, -4, -5].contains(e.code) {
      shut("the peer is closed: \(e)")
    } catch { tell(error) }
    return nil
  }

  func step(_ f: () throws -> Void) -> Bool { step({ () throws -> Bool in try f(); return true }) != nil && !closed }

  func land(_ e: Incoming, _ at: ContinuousClock.Instant) {
    switch e.kind {
    case .reply, .expired: pending[e.id]?.land(e)
    case .call where services.contains(e.topic) && accept != nil:   // a call on a service this transport does not serve is not its to answer
      let until = at + .seconds(e.ttlSecs)   // its ttl counts from its arrival
      let x = ServerCall(call: e, until: until)
      if queued.count >= inboxCap { finish(x, status(.resourceExhausted, "the server is behind")); return }
      guard let kind = kinds["\(e.topic)/\(e.method)"] else { serve(x); return }   // the server's router answers a method it lacks
      if kind?.isRequestStreaming == true { finish(x, status(.unimplemented, "mudRPC carries no client stream")); return }
      x.unary = kind == .unary
      serve(x)
    default: break
    }
  }

  func serve(_ x: ServerCall) {
    queued[ObjectIdentifier(x)] = x
    accept?.yield(x)
  }

  /// The call as grpc-swift's server reads it, built as its handler starts: its deadline is what is left of its ttl.
  func stream(_ x: ServerCall) -> RPCStream<Server.Inbound, Server.Outbound> {
    let left = x.until - ContinuousClock.now
    var metadata: Metadata = [:]
    let ms = max(1, left.components.seconds * 1000 + left.components.attoseconds / 1_000_000_000_000_000)
    metadata.addString(ms < 10_000_000 ? "\(ms)m" : "\(ms / 1000)S", forKey: "grpc-timeout")   // eight characters at most, as gRPC's wire takes it
    let (inbound, c) = AsyncThrowingStream.makeStream(of: RPCRequestPart<[UInt8]>.self, throwing: (any Error).self)
    c.yield(.metadata(metadata))
    c.yield(.message(x.call.bytes))
    c.finish()
    return RPCStream(descriptor: MethodDescriptor(fullyQualifiedService: x.call.topic, method: x.call.method), inbound: RPCAsyncSequence(wrapping: inbound), outbound: RPCWriter.Closable(wrapping: ResponseWriter(t: self, x: x)))
  }

  /// A server's part: unary's message is its one reply, a stream's an item and its status the terminator.
  func respond(_ x: ServerCall, _ part: RPCResponsePart<[UInt8]>) throws {
    try locked {
      switch part {
      case .metadata: return
      case .message(let b):
        if x.done || closed || ContinuousClock.now > x.until { throw RPCError(code: .cancelled, message: "the call is over") }   // a handler streaming to a caller that has gone learns it
        if x.seq >= maxItems {
          finish(x, status(.resourceExhausted, "a stream carries at most 63 items"))
          throw RPCError(code: .resourceExhausted, message: "a stream carries at most 63 items")
        }
        owe(x, x.seq, 0, "", b, x.unary)
        x.seq += 1
        x.done = x.unary
      case .status(let s, _): if !closed { finish(x, s) }
      }
    }
  }

  func finish(_ x: ServerCall, _ s: Status) {
    if x.done { return }
    x.done = true
    owe(x, x.seq, UInt32(s.code.rawValue), s.code == .ok ? streamEnd : clip(s.message), [], true)
  }

  func owe(_ x: ServerCall, _ seq: UInt32, _ code: UInt32, _ detail: String, _ payload: [UInt8], _ end: Bool) {
    let sender = x.call.sender
    if closed || answers[sender, default: []].count >= inboxCap { return }   // a sender owed 16 full streams is flooding: its caller waits out its ttl
    if answers.values.reduce(0, { $0 + $1.count }) >= 4 * inboxCap, let longest = answers.max(by: { $0.value.count < $1.value.count })?.key {   // the server owes 64 full streams: the longest line gives way, so one flooding sender cannot crowd out the rest
      if longest == sender { return }
      answers[longest]!.removeLast()
    }
    if !order.contains(sender) { order.append(sender) }
    answers[sender, default: []].append(Answer(x: x, seq: seq, status: code, detail: detail, payload: payload, end: end))
    wake.signal()
  }

  /// The server's replies, 32 a second, a turn each: a caller sends one and goes to the back of the line, so a spent
  /// second leaves the next one starting with the caller after; a full outbox leaves the rest for the next round, each
  /// until its call's ttl.
  func answer(_ now: UInt32) {
    if now != paceSec { paceSec = now; paced = 0 }
    let t = ContinuousClock.now
    while paced < pace && !order.isEmpty {
      let sender = order.removeFirst()
      guard var line = answers.removeValue(forKey: sender), !line.isEmpty else { continue }
      let a = line.removeFirst()
      do {
        if t <= a.x.until { try peer.reply(now: now, to: a.x.call, status: a.status, payload: a.payload, end: a.end, seq: a.seq, detail: a.detail); paced += 1 }
      } catch let e as MudError where e.code == -2 || e.code == -9 {
        answers[sender] = [a] + line
        order.insert(sender, at: 0)
        return
      } catch let e as MudError where e.code == -3 && !a.payload.isEmpty {
        a.x.done = true   // the stream is over: its handler's writes fail from here
        line.removeAll { $0.x === a.x }
        line.insert(Answer(x: a.x, seq: a.seq, status: UInt32(Status.Code.resourceExhausted.rawValue), detail: "the reply is larger than a mudRPC wire", payload: [], end: true), at: 0)
      } catch { tell(error) }
      if !line.isEmpty { answers[sender] = line; order.append(sender) }
    }
  }

  // The client's side.

  func opened(_ c: ClientCall) { locked { open[ObjectIdentifier(c)] = c } }

  /// The call is over for its reader: `cut` when the reader left first, a cancel or a deadline, unread items cut off.
  func forget(_ c: ClientCall, cut: Bool) {
    locked {
      open[ObjectIdentifier(c)] = nil
      if let k = c.key { pending[k] = nil }
      if cut { c.end(nil, cut: true) }
      settle()
    }
  }

  func settle() {
    if clientShut && open.isEmpty {
      for c in idle { c.finish() }
      idle.removeAll()
    }
  }

  func collect(_ c: ClientCall, _ message: [UInt8]) throws {
    try locked {
      c.request.append(message)
      if c.request.count > 1 {
        c.end(status(.unimplemented, "mudRPC carries no client stream"))
        throw RPCError(code: .unimplemented, message: "mudRPC carries no client stream")
      }
    }
  }

  func begin(_ c: ClientCall, _ method: MethodDescriptor, _ options: CallOptions) {
    locked {
      if c.done { return }
      guard c.request.count == 1 else { c.end(status(.unimplemented, "mudRPC carries no client stream")); return }
      if closed || clientShut { c.end(status(.unavailable, "the transport is closed")); return }
      var life = ttl
      if let timeout = options.timeout {
        let secs = Double(timeout.components.seconds) + Double(timeout.components.attoseconds) / 1e18
        if !(secs > 0) { c.end(status(.deadlineExceeded, "the deadline had passed")); return }
        life = UInt32(min(secs.rounded(.up) + 1, Double(week)))   // a second over: the clock is whole seconds, grpc-swift's own timer is the deadline
      }
      let service = method.service.fullyQualifiedService
      if service.isEmpty || service.utf8.count > 64 { c.end(status(.invalidArgument, "mudRPC: invalid argument")); return }   // core's -1 and -3 for a topic, the call's own
      do {
        let id = try peer.call(now: now(), service: service, method: method.method, request: c.request[0], ttlSecs: life)
        c.key = id
        pending[id] = c
        wake.signal()
      } catch let e as MudError {
        wake.signal()   // a full peer frees its slots on a tick
        if [-1, -4, -5].contains(e.code) {
          shut("the peer is closed: \(e)")
          c.end(status(.unavailable, "the transport is closed"))
        } else { c.end(refused(e)) }
      } catch { c.end(status(.internalError, "\(error)")) }
    }
  }

  func connected() async {
    let (s, c) = AsyncStream.makeStream(of: Void.self)
    locked {
      idle.append(c)
      settle()
    }
    for await _ in s {}
  }

  // The server's side.

  func configure(_ methods: [MethodDescriptor]) {
    locked {
      for m in methods {
        kinds[m.fullyQualifiedMethod] = m.type
        let name = m.service.fullyQualifiedService
        if services.contains(name) { continue }
        if name.isEmpty || name.utf8.count > 64 { badService = name; continue }
        services.append(name)
        toListen.append(name)
      }
      hear()
      wake.signal()
    }
  }

  func listen(_ handler: @escaping @Sendable (RPCStream<Server.Inbound, Server.Outbound>, ServerContext) async -> Void) async throws {
    let (calls, c) = AsyncStream.makeStream(of: ServerCall.self)
    try locked {
      if let bad = badService { throw RPCError(code: .invalidArgument, message: "mudRPC carries a service's full name of 1 to 64 bytes: \(bad)") }
      if accept != nil { throw RPCError(code: .failedPrecondition, message: "a transport serves one server") }
      if closed { c.finish() } else { accept = c }
    }
    await withTaskCancellationHandler {
      await withDiscardingTaskGroup { group in
        for await x in calls {
          group.addTask {
            await withServerContextRPCCancellationHandle { handle in
              guard self.locked({ () -> Bool in
                guard self.queued.removeValue(forKey: ObjectIdentifier(x)) != nil, ContinuousClock.now < x.until else { return false }   // shut down, or its caller has given up: a server behind sheds the call, not the next one
                x.handle = handle
                self.running[ObjectIdentifier(x)] = x
                if self.closed { handle.cancel() }
                return true
              }) else { return }
              defer { self.locked { self.running[ObjectIdentifier(x)] = nil } }
              let stream = self.stream(x)
              await handler(stream, ServerContext(descriptor: stream.descriptor, remotePeer: "mudrpc:\(x.call.sender)", localPeer: "mudrpc", cancellation: handle))
            }
          }
        }
      }
    } onCancel: { self.halt() }
  }

  /// The task running `serve()` cancelled: calls waiting and running end CANCELLED, and their handlers hear it.
  func halt() {
    locked {
      accept?.finish()
      accept = nil
      for x in Array(queued.values) + running.values {
        finish(x, status(.cancelled, "the server shut down"))
        x.handle?.cancel()
      }
      queued.removeAll()
    }
  }

  func shutServer() {
    locked {
      accept?.finish()
      accept = nil
    }
  }

  public struct Client: ClientTransport {
    public typealias Bytes = [UInt8]
    let t: MudTransport
    public var retryThrottle: RetryThrottle? { nil }
    /// Returns once the client is shut down and its calls have ended; on a closed transport a call is UNAVAILABLE.
    public func connect() async throws { await t.connected() }
    public func beginGracefulShutdown() { t.locked { t.clientShut = true; t.settle() } }
    public func config(forMethod descriptor: MethodDescriptor) -> MethodConfig? { nil }

    public func withStream<T: Sendable>(descriptor: MethodDescriptor, options: CallOptions, _ closure: (RPCStream<Inbound, Outbound>, ClientContext) async throws -> T) async throws -> T {
      let (inbound, out) = AsyncThrowingStream.makeStream(of: RPCResponsePart<[UInt8]>.self, throwing: (any Error).self)
      let call = ClientCall(out: out)
      out.onTermination = { [t] how in t.forget(call, cut: { if case .cancelled = how { true } else { false } }()) }
      t.opened(call)
      defer { t.forget(call, cut: true) }
      if descriptor.type?.isRequestStreaming == true { t.locked { call.end(status(.unimplemented, "mudRPC carries no client stream")) } }
      let stream = RPCStream(descriptor: descriptor, inbound: RPCAsyncSequence(wrapping: Cut(t: t, call: call, base: inbound)), outbound: RPCWriter.Closable(wrapping: RequestWriter(t: t, call: call, method: descriptor, options: options)))
      return try await closure(stream, ClientContext(descriptor: descriptor, remotePeer: "mudrpc", localPeer: "mudrpc"))
    }
  }

  public struct Server: ServerTransport {
    public typealias Bytes = [UInt8]
    let t: MudTransport
    public func configure(context: GRPCServerContext) { t.configure(context.methods) }
    /// Answers the services the server was built with, until `beginGracefulShutdown`, then returns once their handlers have.
    public func listen(streamHandler: @escaping @Sendable (RPCStream<Inbound, Outbound>, ServerContext) async -> Void) async throws { try await t.listen(streamHandler) }
    /// Takes no new calls; the ones running finish.
    public func beginGracefulShutdown() { t.shutServer() }
  }
}

/// One call in flight: replies in any order, items taken in seq order, the server's status after the items before it;
/// a local end, a cancel, a deadline or a close, ends it whatever the replies say, unread items cut off.
final class ClientCall: @unchecked Sendable {   // guarded by the transport's lock
  let out: AsyncThrowingStream<RPCResponsePart<[UInt8]>, any Error>.Continuation
  var key: [UInt8]?, request: [[UInt8]] = [], next: UInt32 = 0, early: [UInt32: Incoming] = [:], started = false, done = false
  var cut: Status?   // a local end, a close or the ttl, which the reader meets before any unread item
  init(out: AsyncThrowingStream<RPCResponsePart<[UInt8]>, any Error>.Continuation) { self.out = out }

  /// The call's status, after the items before it, or with `cut` before them; none for the reader's own end.
  func end(_ s: Status?, cut: Bool = false) {
    if cut && self.cut == nil { self.cut = s ?? status(.cancelled, "the call was cancelled") }
    if done { return }
    done = true
    early.removeAll()
    if let s { out.yield(.status(s, [:])) }
    out.finish()
  }

  func land(_ e: Incoming) {
    if e.kind == .expired { end(status(.deadlineExceeded, "the call's ttl passed"), cut: true); return }
    if done || e.seq < next { return }
    if early[e.seq] == nil { early[e.seq] = e }
    while !done, let r = early.removeValue(forKey: next) {
      next += 1
      if r.status != 0 { end(status(Status.Code(rawValue: Int(r.status)) ?? .unknown, r.detail)); continue }
      if r.end && r.detail == streamEnd { end(status(.ok, "")); continue }
      if !started { started = true; out.yield(.metadata([:])) }
      out.yield(.message(r.bytes))
      if r.end { end(status(.ok, "")) }
    }
  }
}

final class ServerCall: @unchecked Sendable {   // guarded by the transport's lock
  let call: Incoming, until: ContinuousClock.Instant
  var seq: UInt32 = 0, unary = false, done = false, handle: ServerContext.RPCCancellationHandle?
  init(call: Incoming, until: ContinuousClock.Instant) { self.call = call; self.until = until }
}

struct Answer {
  let x: ServerCall, seq: UInt32, status: UInt32, detail: String, payload: [UInt8], end: Bool
}

/// A call's replies as its reader takes them: a cancelled reader, a closed transport or a passed ttl ends the call
/// there, unread items cut off, CANCELLED for the reader's own cancel, as gRPC's.
struct Cut: AsyncSequence, Sendable {
  let t: MudTransport, call: ClientCall, base: AsyncThrowingStream<RPCResponsePart<[UInt8]>, any Error>
  struct AsyncIterator: AsyncIteratorProtocol {
    let t: MudTransport, call: ClientCall
    var base: AsyncThrowingStream<RPCResponsePart<[UInt8]>, any Error>.AsyncIterator
    mutating func next() async throws -> RPCResponsePart<[UInt8]>? {
      let cancelled = RPCError(code: .cancelled, message: "the call was cancelled")
      if Task.isCancelled { throw cancelled }
      if let s = t.locked({ [call] in call.cut }) { throw RPCError(code: RPCError.Code(s.code) ?? .unknown, message: s.message) }
      let part = try await base.next()
      if part == nil && Task.isCancelled { throw cancelled }
      return part
    }
  }
  func makeAsyncIterator() -> AsyncIterator { AsyncIterator(t: t, call: call, base: base.makeAsyncIterator()) }
}

struct RequestWriter: ClosableRPCWriterProtocol {
  let t: MudTransport, call: ClientCall, method: MethodDescriptor, options: CallOptions
  func write(_ part: RPCRequestPart<[UInt8]>) async throws { if case .message(let b) = part { try t.collect(call, b) } }
  func write(contentsOf parts: some Sequence<RPCRequestPart<[UInt8]>>) async throws { for p in parts { try await write(p) } }
  func finish() async { t.begin(call, method, options) }
  func finish(throwing error: any Error) async {
    t.locked { call.end((error as? RPCError).map { status(Status.Code($0.code), $0.message) } ?? status(.unknown, "\(error)")) }
  }
}

struct ResponseWriter: ClosableRPCWriterProtocol {
  let t: MudTransport, x: ServerCall
  func write(_ part: RPCResponsePart<[UInt8]>) async throws { try t.respond(x, part) }
  func write(contentsOf parts: some Sequence<RPCResponsePart<[UInt8]>>) async throws { for p in parts { try await write(p) } }
  func finish() async { t.locked { t.finish(x, status(.unknown, "the server ended the call without a status")) } }
  func finish(throwing error: any Error) async { await finish() }
}
