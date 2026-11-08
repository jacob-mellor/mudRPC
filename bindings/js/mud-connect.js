// mudRPC as Connect's transport in JavaScript and TypeScript: the clients and services protoc-gen-es describes run over
// mudRPC unchanged, `createClient(Drone, transport)` as with any Connect transport and `router.service(Drone, impl)`
// as on Connect's router; building the transport is the one line that differs. Beside mud.js, with @bufbuild/protobuf
// and @connectrpc/connect, whose Transport, ConnectError and handler context this uses (Copyright Buf Technologies,
// Apache License 2.0).
//
// Not a drop-in where Connect needs a connection: a message of about 460 bytes, a service's full name of at most 64
// bytes, no headers or trailers, unary and server-streaming calls only, at most 63 items a stream, the first reply
// wins, and 16 calls in flight. A server sends 32 replies a second, half what a receiver takes from one sender, its
// callers in turn, and a client keeps under 32 calls a second. A timeout is the call's ttl, a week at most; a call
// cancelled after it was sent keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { Code, ConnectError, createContextKey, createContextValues, createHandlerContext } from '@connectrpc/connect';
import { Udp } from './mud.js';

/** The caller's roster number, in a handler's `ctx.values`: proven per device under a signed fleet. */
export const CALLER = createContextKey(0, { description: "the caller's mudRPC roster number" });

/** The detail of the reply that ends a server stream and carries no item. A reply with end, OK and no detail is the stream's last item, as the native API sends it. */
export const STREAM_END = 'end';
const WEEK = 604800, MAX_ITEMS = 63, INBOX = 1024, PACE = 32, PATIENCE = 1000, DETAIL = 128; // a message's longest life, a stream's items, wires or calls waiting, replies a second, ms a refused item is tried, a detail's bytes
const ENC = new TextEncoder();
const clock = () => performance.now(); // monotonic, as a deadline must be: a wall clock stepped forward would expire every call at once
const hex = (b) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');

/** A detail as text, at most 128 bytes of UTF-8, cut on a character. */
function clip(detail) {
  const b = ENC.encode(String(detail ?? '')).subarray(0, DETAIL);
  return new TextDecoder('utf-8', { fatal: false, ignoreBOM: true }).decode(b).replace(/�$/, '');
}

function refused(e) {
  const code = [-2, -3, -9].includes(e.code) ? Code.ResourceExhausted : e.code === -1 ? Code.InvalidArgument : Code.Internal;
  return new ConnectError(e.message, code);
}

const expired = () => new ConnectError('the operation timed out', Code.DeadlineExceeded);

/** A reply's message, or Connect's error for one it cannot read. */
function decode(schema, bytes) {
  try { return fromBinary(schema, bytes); } catch (e) { throw new ConnectError(`failed to deserialize response: ${e.message}`, Code.Internal, undefined, undefined, e); }
}

/**
 * A Connect transport over one peer, and the router for the services added to it: it pumps on the event loop.
 *
 * The peer is the transport's while it lives: reach it only through use(), and close the transport before the peer.
 * `radio` is a Udp, which the pump sends through and reads in its own round, or a function taking each outgoing item,
 * with what arrives handed to deliver(). `ttlSecs` is the life of a call made without a timeout; `fault` hears every
 * exception the radio throws, and the item it threw on is tried for a second, then let go.
 */
export class MudTransport {
  #peer; #udp; #send; #fault; #ttl; #start = clock(); #contexts = new Map(); #inbox = []; #pending = new Map(); #open = new Set();
  #methods = new Map(); #services = new Set(); #toListen = []; #answers = new Map(); #closed = false;
  #paceSec = -1; #paced = 0; #refusedSince = 0; #timer; #soon = false; #waiting = 0;

  constructor(peer, radio, { ttlSecs = 60, fault = () => {} } = {}) {
    this.#peer = peer;
    this.#udp = radio instanceof Udp || (typeof radio?.send === 'function' && typeof radio?.poll === 'function') ? radio : null; // a Udp, from this copy of mud.js or another
    if (!this.#udp && typeof radio !== 'function') throw new TypeError('a radio is a Udp or a function taking each outgoing item');
    this.#send = this.#udp ? (o) => this.#udp.send(o) : radio;
    this.#fault = fault;
    if (this.#udp) this.#udp.fault = (e) => this.#tell(e); // the socket's own errors, an earlier send's among them
    this.#ttl = Math.max(1, Math.min(Math.floor(ttlSecs), WEEK));
    this.#timer = setInterval(() => this.#round(), this.#udp ? 10 : 50);
    this.#timer.unref?.();
  }

  /** A wire from the transport. Past 1024 waiting for the pump, a wire is dropped, as core would drop a flood. */
  deliver(topic, wire) {
    if (topic == null || wire == null) throw new TypeError('a wire has a topic and bytes');
    if (this.#closed || this.#inbox.length >= INBOX) return;
    this.#inbox.push([topic, wire]);
    this.#kick();
  }

  /** The peer, between rounds of the pump: save, stage, rotate, roster, counts. */
  use(act) { const r = act(this.#peer); this.#kick(); return r; }

  /** Connect's router shape, so `routes` written for connectNodeAdapter serve here: `t.routes((router) => router.service(Drone, impl))`. */
  routes(fn) { fn(this); return this; }

  /** The generated service descriptor and its implementation, as ConnectRouter.service takes them. */
  service(desc, impl) {
    const name = desc.typeName;
    if (!name || !name.isWellFormed() || ENC.encode(name).length > 64) throw new ConnectError(`mudRPC carries a service's full name of 1 to 64 bytes of UTF-8: ${name}`, Code.InvalidArgument);
    for (const m of desc.methods) this.#methods.set(`${name}/${m.name}`, { m, impl, fn: impl[m.localName] });
    if (!this.#services.has(name)) { this.#services.add(name); this.#toListen.push(name); }
    this.#hold();
    this.#kick();
    return this;
  }

  /** Stops the pump and ends every call in flight with Unavailable. */
  close() {
    if (this.#closed) return;
    this.#closed = true;
    clearInterval(this.#timer);
    for (const c of [...this.#open]) c.abort(new ConnectError('the transport is closed', Code.Unavailable));
    for (const ctx of this.#contexts.values()) ctx.abort(new ConnectError('the transport is closed', Code.Unavailable)); // a closed server's handlers hear it, and their timers let the process go
    this.#answers.clear();
    this.#udp?.unref?.();
  }

  #now() { return Math.floor((clock() - this.#start) / 1000); }
  #kick() { if (this.#soon || this.#closed) return; this.#soon = true; setImmediate(() => { this.#soon = false; this.#round(); }); }
  /** The pump holds the process while it has work, calls in flight or services to answer, as a socket would, and lets it go when idle. */
  #hold() {
    if (this.#closed) return;
    const busy = this.#open.size || this.#services.size || this.#answers.size || this.#contexts.size;
    this.#timer[busy ? 'ref' : 'unref']?.();
    this.#udp?.[busy ? 'ref' : 'unref']?.();
  }
  #tell(e) { try { this.#fault(e); } catch { /* the fault callback's own */ } }

  #round() {
    if (this.#closed) return;
    this.#hold();
    const now = this.#now();
    try {
      while (this.#toListen.length) { this.#peer.listen(this.#toListen[0]); this.#toListen.shift(); }
    } catch { /* the outbox is full: the next round listens */ }
    try {
      for (let w; (w = this.#inbox.shift());) {
        let e;
        try { e = this.#peer.deliver(now, w[0], w[1]); } catch { continue; } // a wire the transport mangled: no verdict to act on
        this.#land(e);
      }
      if (this.#udp) {
        try { for (let i = 0, w; i < INBOX && (w = this.#udp.poll()); i++) { try { this.#land(this.#peer.deliver(now, w[0], w[1])); } catch { /* mangled */ } } }
        catch { this.#shutFor('the socket is closed'); return; }
      }
      for (let e; (e = this.#peer.tick(now));) this.#land(e);
      this.#answer(now);
      for (let o; (o = this.#peer.outgoing(now));) {
        try { this.#send(o); }
        catch (x) {
          if (this.#udp && x?.code === -1) { this.#shutFor('the socket is closed'); return; }
          this.#tell(x);
          const t = clock();
          this.#refusedSince ||= t;
          if (t - this.#refusedSince < PATIENCE) break; // the next round tries it again; past a second it is let go
        }
        this.#refusedSince = 0;
        this.#peer.sent();
      }
    } catch (x) {
      if ([-1, -4, -5].includes(x?.code)) { this.#shutFor(`the peer is closed: ${x.message}`); return; }
      this.#tell(x);
    }
  }

  #shutFor(why) { const was = this.#closed; this.close(); if (!was) this.#tell(new Error(why)); }

  #land(e) {
    if (e.kind === 'reply' || e.kind === 'expired') this.#pending.get(hex(e.id))?.land(e);
    else if (e.kind === 'call' && this.#services.has(e.topic)) { // a call on a service this transport does not serve is not its to answer
      const until = clock() + e.ttlSecs * 1000; // its ttl counts from its arrival
      if (this.#waiting >= INBOX) { this.#owe(e, 0, Code.ResourceExhausted, 'the server is behind', new Uint8Array(0), true, until); return; }
      this.#waiting++;
      queueMicrotask(() => { this.#waiting--; this.#serve(e, until).catch((x) => this.#tell(x)); }); // nothing a call does rejects unheard, which Node would end the process for
    }
  }

  #owe(call, seq, status, detail, payload, end, until) {
    const line = this.#answers.get(call.sender) ?? [];
    if (line.length >= INBOX) return; // a sender owed 16 full streams is flooding: its caller waits out its ttl
    let owed = 0, longest = line;
    for (const l of this.#answers.values()) { owed += l.length; if (l.length > longest.length) longest = l; }
    if (owed >= 4 * INBOX) { if (longest === line) return; longest.pop(); } // 64 full streams owed: the longest line gives way, so one flooding sender cannot crowd out the rest
    if (!line.length) this.#answers.set(call.sender, line);
    line.push({ call, seq, status, detail: clip(detail), payload, end, until });
    this.#kick();
  }

  /** The server's replies, 32 a second, a turn each: a caller sends one and goes to the back of the line, so a spent second leaves the next one starting with the caller after; a full outbox leaves the rest for the next round, each until its call's ttl. */
  #answer(now) {
    if (now !== this.#paceSec) { this.#paceSec = now; this.#paced = 0; }
    const t = clock();
    while (this.#paced < PACE && this.#answers.size) {
      let [sender, line] = this.#answers.entries().next().value;
      const a = line.shift();
      try {
        if (a && t <= a.until) { this.#peer.reply(now, a.call, a.status, a.payload, a.end, a.seq, a.detail); this.#paced++; }
      } catch (x) {
        if (x?.code === -2 || x?.code === -9) { line.unshift(a); return; }
        if (x?.code === -3 && a.payload.length) {
          this.#contexts.get(a.call)?.abort(new ConnectError('the reply is larger than a mudRPC wire', Code.ResourceExhausted)); // the stream is over: its handler is not read from here
          line = line.filter((b) => b.call !== a.call);
          line.unshift({ ...a, status: Code.ResourceExhausted, detail: 'the reply is larger than a mudRPC wire', payload: new Uint8Array(0), end: true });
        } else this.#tell(x);
      }
      this.#answers.delete(sender);
      if (line.length) this.#answers.set(sender, line);
    }
  }

  async #serve(call, until) {
    if (clock() >= until) return; // its caller has given up: a server behind sheds the call, not the next one
    const route = this.#methods.get(`${call.topic}/${call.method}`);
    let ctx;
    const owe = (seq, status, detail, payload, end) => { if (!this.#closed && !ctx?.signal.aborted) this.#owe(call, seq, status, detail, payload, end, until); };
    if (!route || !route.fn) { owe(0, Code.Unimplemented, `no method ${call.topic}/${call.method}`, new Uint8Array(0), true); return; }
    const { m, impl, fn } = route;
    if (m.methodKind !== 'unary' && m.methodKind !== 'server_streaming') { owe(0, Code.Unimplemented, 'mudRPC carries no client stream', new Uint8Array(0), true); return; }
    let req;
    try { req = fromBinary(m.input, call.bytes); } catch { owe(0, Code.InvalidArgument, 'failed to deserialize request', new Uint8Array(0), true); return; }
    ctx = createHandlerContext({ service: m.parent, method: m, protocolName: 'mudrpc', requestMethod: 'POST', url: `mudrpc:/${m.parent.typeName}/${m.name}`, timeoutMs: until - clock(), requestHeader: {}, contextValues: createContextValues().set(CALLER, call.sender) }); // its signal aborts when the call's ttl passes
    let seq = 0;
    this.#contexts.set(call, ctx);
    try {
      if (m.methodKind === 'unary') {
        const res = await fn.call(impl, req, ctx);
        owe(0, 0, '', toBinary(m.output, create(m.output, res)), true);
      } else {
        for await (const item of fn.call(impl, req, ctx)) {
          if (ctx.signal.aborted) return; // past its ttl, or a reply past the wire: the stream is over
          if (seq >= MAX_ITEMS) throw new ConnectError(`a stream carries at most ${MAX_ITEMS} items`, Code.ResourceExhausted);
          owe(seq++, 0, '', toBinary(m.output, create(m.output, item)), false);
        }
        owe(seq, 0, STREAM_END, new Uint8Array(0), true);
      }
    } catch (e) {
      let c;
      try { c = ConnectError.from(e); } catch { c = new ConnectError('the handler threw what cannot be told', Code.Unknown); } // a throw String() cannot render still gets its answer
      owe(seq, c.code, c.rawMessage, new Uint8Array(0), true);
    } finally { this.#contexts.delete(call); ctx.abort(); } // as Connect's server does when a handler is done: its deadline timer let go
  }

  // Connect's Transport.

  async unary(method, signal, timeoutMs, header, input) {
    const call = this.#begin(method, signal, timeoutMs, toBinary(method.input, create(method.input, input)), false);
    const bytes = await call.result();
    return { stream: false, service: method.parent, method, header: new Headers(), trailer: new Headers(), message: decode(method.output, bytes) };
  }

  async stream(method, signal, timeoutMs, header, input) {
    if (method.methodKind !== 'server_streaming') throw new ConnectError('mudRPC carries no client stream', Code.Unimplemented);
    let first;
    for await (const m of input) { first = m; break; }
    const call = this.#begin(method, signal, timeoutMs, toBinary(method.input, create(method.input, first)), true);
    await call.started();
    const output = method.output;
    return { stream: true, service: method.parent, method, header: new Headers(), trailer: new Headers(), message: (async function* () {
      let read = false;
      try { for await (const b of call.items()) yield decode(output, b); read = true; }
      finally { if (!read) call.abort(new ConnectError('the stream was abandoned', Code.Canceled)); } // a reader that stops early ends the call, as Connect's
    })() };
  }

  #begin(method, signal, timeoutMs, payload, stream) {
    if (typeof timeoutMs !== 'number') timeoutMs = undefined; // none, as Connect reads it
    const call = new Call(stream, (c) => { this.#open.delete(c); if (c.key) this.#pending.delete(c.key); this.#hold(); });
    if (signal?.aborted) { call.abort(ConnectError.from(signal.reason, Code.Canceled)); return call; }
    if (timeoutMs !== undefined && !(timeoutMs > 0)) { call.abort(expired()); return call; }
    if (this.#closed) { call.abort(new ConnectError('the transport is closed', Code.Unavailable)); return call; }
    const life = timeoutMs === undefined || !Number.isFinite(timeoutMs) ? this.#ttl : Math.min(Math.ceil(timeoutMs / 1000) + 1, WEEK); // a second over: the clock is whole seconds, the timer below is the deadline
    const service = method.parent.typeName; // 1 to 64 bytes of UTF-8, a method name UTF-8: a lone surrogate mud.js would send as U+FFFD; checked here, a -1 below is a closed peer's
    if (!service || ENC.encode(service).length > 64 || !service.isWellFormed() || !method.name.isWellFormed()) { call.abort(new ConnectError('mudRPC: invalid argument', Code.InvalidArgument)); return call; }
    try { call.key = hex(this.#peer.call(this.#now(), method.parent.typeName, method.name, payload, life)); }
    catch (e) {
      if ([-1, -4, -5].includes(e?.code)) { this.#shutFor(`the peer is closed: ${e.message}`); call.abort(new ConnectError('the transport is closed', Code.Unavailable)); return call; }
      call.abort(refused(e)); return call;
    }
    this.#pending.set(call.key, call);
    this.#open.add(call);
    if (timeoutMs !== undefined && Number.isFinite(timeoutMs) && timeoutMs <= WEEK * 1000) { const t = setTimeout(() => call.abort(expired()), timeoutMs); call.timer = t; call.hold(() => clearTimeout(t)); }
    if (signal) { const on = () => call.abort(ConnectError.from(signal.reason, Code.Canceled)); signal.addEventListener('abort', on, { once: true }); call.hold(() => signal.removeEventListener('abort', on)); }
    this.#hold();
    this.#kick();
    return call;
  }
}

/** One call in flight: replies in any order, items taken in seq order, the server's status after the items before it; a local end, a cancel, a timeout or a close, ends it whatever the replies say, unread items cut off. */
class Call {
  #early = new Map(); #next = 0; #done = false; #over = false; #items = []; #error = null; #release = []; #forget; #stream; #response = null; #waiters = [];
  key = null; timer = null;
  constructor(stream, forget) { this.#stream = stream; this.#forget = forget; }
  hold(letGo) { this.#release.push(letGo); }

  land(e) {
    if (e.kind === 'expired') { this.abort(expired()); return; }
    if (this.#done || e.seq < this.#next) return;
    if (!this.#early.has(e.seq)) this.#early.set(e.seq, e);
    while (!this.#done && this.#early.has(this.#next)) {
      const r = this.#early.get(this.#next);
      this.#early.delete(this.#next++);
      if (r.status !== 0) this.#finish(new ConnectError(r.detail, r.status <= 16 ? r.status : Code.Unknown));
      else if (r.end && r.detail === STREAM_END) this.#finish(null);
      else {
        if (this.#stream) this.#items.push(r.bytes); else this.#response ??= r.bytes;
        if (r.end) this.#finish(null);
      }
    }
    this.#notify();
  }

  /** The server's status: the call is over once the items before it are read. */
  #finish(error) {
    this.#done = true;
    this.#early.clear();
    this.#forget(this); // the exchange is over: what is left unread holds neither the transport nor the process
    this.timer?.unref?.();
    this.#error = error ?? (!this.#stream && this.#response === null ? new ConnectError('no response', Code.Internal) : null);
    if (!this.#stream || !this.#items.length) this.#end();
  }

  #end() {
    if (this.#over) return;
    this.#over = true;
    this.#forget(this);
    for (const r of this.#release) r();
    this.#notify();
  }

  abort(error) {
    if (this.#over) return;
    this.#items = [];
    this.#done = true;
    this.#error = error;
    this.#end();
  }

  #notify() { const w = this.#waiters; this.#waiters = []; for (const f of w) f(); }
  #wait() { return new Promise((r) => this.#waiters.push(r)); }

  async result() {
    while (!this.#over) await this.#wait();
    if (this.#error) throw this.#error;
    return this.#response;
  }

  /** A stream's call is under way once it has been sent; a refusal before that is the stream call's own rejection, as Connect's. */
  async started() { if (this.#over && this.#error) throw this.#error; }

  async *items() {
    while (true) {
      while (!this.#items.length && !this.#over) await this.#wait();
      if (this.#items.length) {
        const b = this.#items.shift();
        if (!this.#items.length && this.#done) this.#end();
        yield b;
        continue;
      }
      if (this.#error) throw this.#error;
      return;
    }
  }
}
