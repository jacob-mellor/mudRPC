// mudRPC for JavaScript: one file over the C ABI compiled to WebAssembly. ES module, Node 22 or later, or a browser.
// No package to install, no dependencies, no build step. Copy it into your project and put mudrpc.wasm beside it, or
// point MUDRPC_WASM at the binary. Types are in the JSDoc, so the IDE sees them without TypeScript.
//
// A call is a message; a reply may come hours later or expire; nothing blocks. You poll: drain outgoing() into your
// transport and call sent(), feed what arrives to deliver(), call tick(). `now` is your uptime in seconds.

/** What a delivered wire was, or why it was dropped. @typedef {'none'|'call'|'reply'|'event'|'expired'|'dropped'|'flood'|'rollback'} Kind */
export const Kind = Object.freeze(['none', 'call', 'reply', 'event', 'expired', 'dropped', 'flood', 'rollback']);
/** Why a wire never reached your code. Log every one in development; count them in the field. @typedef {'none'|'version'|'open'|'decode'|'duplicate'|'noPending'|'topic'|'seq'|'id'|'replay'|'rate'} Dropped */
export const Dropped = Object.freeze(['none', 'version', 'open', 'decode', 'duplicate', 'noPending', 'topic', 'seq', 'id', 'replay', 'rate']);
/** Which key slot opened the last accepted wire. @typedef {'current'|'previous'|'next'} Slot */
export const Slot = Object.freeze(['current', 'previous', 'next']);
/** The ciphers, as mudrpc.h names them: AES the default, the NIST algorithm buyers ask for; ChaCha20-Poly1305 smaller and faster without an AES instruction. */
export const CIPHER_CHACHA20_POLY1305 = 0, CIPHER_AES_256_GCM = 1, CIPHER_DEFAULT = CIPHER_AES_256_GCM;

/**
 * One delivered wire. For a call, pass it back to Peer.reply. For flood and rollback, `sender` is the number concerned.
 * @typedef {{ kind: Kind, id: Uint8Array, topic: string, method: string, replyTo: string, detail: string, bytes: Uint8Array,
 *   ttlSecs: number, seq: number, status: number, end: boolean, dropped: Dropped, sender: number }} Incoming
 */
/** What the transport must do next: listen on a topic, or send a wire with its lifetime. @typedef {{ listen: boolean, topic: string, wire: Uint8Array, ttlSecs: number }} Outgoing */
/** A scenario event; clone, spam, stage, rotate, retire, warp, hostile, stale_reboot, torn_save, revoke and telemetry are the host's to perform. @typedef {{ atMs: number, kind: string, a: number, b: number, text: string }} SimEvent */

/** A failure code from the library. `code` is the C constant: -2 full, -3 too large, -4 corrupt, -6 epoch, -7 unprovisioned, -8 exhausted, -9 rate, -10 busy. */
export class MudError extends Error {
  static NAMES = { '-1': 'invalid argument', '-2': 'full', '-3': 'too large', '-4': 'corrupt', '-5': 'bad memory', '-6': 'epoch not higher', '-7': 'unprovisioned (sender 0)', '-8': 'counter exhausted', '-9': 'this second\'s sends spent', '-10': 'busy: another thread holds this peer', '-99': 'panic' };
  constructor(code) { super('mudRPC: ' + (MudError.NAMES[code] ?? code)); this.code = code; }
  get isFull() { return this.code === -2; }
}

// The binary: MUDRPC_WASM, else mudrpc.wasm beside this file, read from disk under Node and fetched in a browser.
const ex = await (async () => {
  const beside = new URL('./mudrpc.wasm', import.meta.url);
  let bytes;
  if (globalThis.process?.versions?.node) {
    const { readFile } = await import('node:fs/promises');
    const { fileURLToPath } = await import('node:url');
    const file = process.env.MUDRPC_WASM || fileURLToPath(beside);
    bytes = await readFile(file).catch(() => { throw new Error(`mudrpc.wasm not found at ${file}: build with \`cargo build --release -p mudrpc-c --target wasm32-unknown-unknown\`, then set MUDRPC_WASM or put it beside mud.js`); });
  } else bytes = await (await fetch(beside)).arrayBuffer();
  const { instance } = await WebAssembly.instantiate(bytes, {});
  if (!instance.exports.mud_alloc || instance.exports.mud_publish.length !== 6) throw new Error('mudrpc.wasm is from another build or an older Rust whose wasm C ABI differs: rebuild it');
  return instance.exports;
})();

// The C ABI, field for field from mudrpc.h, in wasm32 terms: pointers and size_t are 4 bytes, so mud_buf is {ptr, len}
// in 8; mud_id is 12 bytes with alignment 1, so it follows kind at offset 4 with nothing padded after it; u64 aligns to
// 8, so mud_event pads after kind. A struct passed by value crosses as a pointer to a copy. Offsets in bytes.
const IN = { kind: 0, id: 4, topic: 16, method: 24, reply_to: 32, detail: 40, bytes: 48, ttl_secs: 56, seq: 60, status: 64, end: 68, dropped: 72, sender: 76, size: 80 };
const OUT = { kind: 0, topic: 4, wire: 12, ttl_secs: 20, size: 24 };
const CONFIG = { name: 0, key: 8, sender: 16, epoch: 20, seed: 24, budget: 32, hub: 36, cipher: 40, size: 44 };
const EVENT = { at_ms: 0, kind: 8, a: 16, b: 24, text: 32, size: 40 };
const MAX_TOPIC = 64, MSG = 512;
const ENC = new TextEncoder(), DEC = new TextDecoder();

// Memory may grow on any call, which detaches old views: take a fresh view every time.
const mem = () => new DataView(ex.memory.buffer);
const u8 = () => new Uint8Array(ex.memory.buffer);
const check = (code) => { if (code < 0) throw new MudError(code); return code; };
const alloc = (n) => { const p = ex.mud_alloc(n); if (!p) throw new MudError(-5); return p; };
/** `b` must be a Uint8Array, of length `n` when given. */
const arg = (b, n = b?.length) => { if (!(b instanceof Uint8Array) || b.length !== n) throw new MudError(-1); return b; };
/** The bytes a mud_buf at `a` points to, copied out. */
const bytesAt = (a) => { const d = mem(), p = d.getUint32(a, true), n = d.getUint32(a + 4, true); return u8().slice(p, p + n); };
const strAt = (a) => DEC.decode(bytesAt(a));

/** gRPC's name for a status number, "FAILED_PRECONDITION" for 9, "" past 16. @param {number} status @returns {string} */
export function statusName(status) { const p = ex.mud_status_name(status), b = u8(); return DEC.decode(b.subarray(p, b.indexOf(0, p))); }

// Out-parameters live in one block for the life of the module; every call copies its result out before it returns.
const REG = alloc(180);
const R = { in: REG, out: REG + 80, id: REG + 104, event: REG + 120, topic: REG + 160, wire: REG + 168, line: REG + 176 };

/** Runs `f` with a scope whose copies of arguments into the module are all freed when `f` returns. */
function scope(f) {
  const owned = [];
  const s = {
    take(n) { const p = alloc(n); owned.push([p, n]); return p; },
    bytes(b) { const p = s.take(arg(b).length); u8().set(b, p); return p; },
    /** A mud_mutbuf (or mud_buf) over memory the module already holds. */
    span(ptr, n) { const p = s.take(8), d = mem(); d.setUint32(p, ptr, true); d.setUint32(p + 4, n, true); return p; },
    /** A mud_buf over a copy of `b`. */
    buf(b) { return s.span(s.bytes(b), b.length); },
    text(t) { return s.buf(ENC.encode(t)); },
  };
  try { return f(s); } finally { for (const [p, n] of owned) ex.mud_free(p, n); }
}

/** @returns {Incoming} the mud_incoming at `a`, copied out. */
function wrap(a) {
  const d = mem();
  return { kind: Kind[d.getInt32(a + IN.kind, true)], id: u8().slice(a + IN.id, a + IN.id + 12), topic: strAt(a + IN.topic), method: strAt(a + IN.method), replyTo: strAt(a + IN.reply_to), detail: strAt(a + IN.detail), bytes: bytesAt(a + IN.bytes),
    ttlSecs: d.getUint32(a + IN.ttl_secs, true), seq: d.getUint32(a + IN.seq, true), status: d.getUint32(a + IN.status, true), end: d.getInt32(a + IN.end, true) !== 0, dropped: Dropped[d.getInt32(a + IN.dropped, true)], sender: d.getUint32(a + IN.sender, true) };
}

const SIZE = ex.mud_size(), HUB_SIZE = ex.mud_hub_size();
const RESTORE = Symbol('restore');

/**
 * A mudRPC peer. `name` is its inbox topic; `key` the 32-byte fleet key; `sender` the roster number (0 cannot send);
 * `epoch` a number the host persists beside the key and raises on every boot and restore, written to storage before
 * the peer is created. save() bytes contain the key; store them as you store the key. close() zeroes and frees its memory.
 * A signed peer believes 16 senders; `hub` is the hub shape, 256, for the host that believes a fleet; the line past either throws Full.
 */
export class Peer {
  static MAX_TOPIC = MAX_TOPIC; static MSG = MSG;
  #size; #block; #mem; #scratch; #plain;
  #place(size) { this.#size = size; this.#block = (size + 7) & ~7; this.#mem = alloc(this.#block + MAX_TOPIC + MSG + MSG); this.#scratch = this.#mem + this.#block; this.#plain = this.#scratch + MAX_TOPIC + MSG; }   // the peer, then its two scratch buffers, in one allocation

  /** Unsigned under the fleet key alone, or signed with this sender's 32-byte Ed25519 seed and a verification budget per second.
   * @param {string} name @param {Uint8Array} key @param {number} sender @param {number} epoch @param {Uint8Array|null} [seed] @param {number} [budget] @param {boolean} [hub] @param {number} [cipher] a CIPHER_ constant */
  constructor(name, key, sender, epoch, seed = null, budget = 1000, hub = false, cipher = CIPHER_DEFAULT) {
    this.#place(name === RESTORE ? key : hub ? HUB_SIZE : SIZE);
    if (name === RESTORE) return;
    try {
      scope((s) => {
        const c = s.take(CONFIG.size), k = s.bytes(arg(key, 32)), n = s.text(name), sd = seed ? s.bytes(arg(seed, 32)) : 0, d = mem(); // every allocation before the view: a growth detaches it
        u8().copyWithin(c + CONFIG.name, n, n + 8);
        d.setUint32(c + CONFIG.key, k, true); d.setUint32(c + CONFIG.key + 4, 32, true); d.setUint32(c + CONFIG.sender, sender, true); d.setUint32(c + CONFIG.epoch, epoch, true);
        d.setUint32(c + CONFIG.seed, sd, true); d.setUint32(c + CONFIG.seed + 4, seed ? 32 : 0, true); d.setUint32(c + CONFIG.budget, budget, true); d.setUint32(c + CONFIG.hub, hub ? 1 : 0, true); d.setUint32(c + CONFIG.cipher, cipher, true);
        check(ex.mud_new(this.#mem, this.#size, c));
      });
    } catch (e) { this.close(); throw e; }
  }

  /** A peer from save() bytes of this same build of the library, with a higher epoch from the host. @param {Uint8Array} saved @param {number} epoch */
  static restore(saved, epoch) {
    if (!(saved instanceof Uint8Array) || (saved.length !== SIZE && saved.length !== HUB_SIZE)) throw new MudError(-4);
    const p = new Peer(RESTORE, saved.length);
    u8().set(saved, p.#mem);
    try { check(ex.mud_resume(p.#mem, saved.length, epoch)); } catch (e) { p.close(); throw e; }
    return p;
  }

  /** Signed peers: a roster line for sender, its public key and the next one during a rotation. @param {number} sender @param {Uint8Array} key @param {Uint8Array|null} [next] */
  roster(sender, key, next = null) { scope((s) => check(ex.mud_roster(this.#mem, this.#size, sender, s.buf(arg(key, 32)), s.buf(next ? arg(next, 32) : new Uint8Array(0))))); }
  /** @param {number} sender */
  revoke(sender) { check(ex.mud_revoke(this.#mem, this.#size, sender)); }
  /** The 32-byte public key of a seed, for rosters. @param {Uint8Array} seed @returns {Uint8Array} */
  static publicKey(seed) { return scope((s) => { const o = s.take(32); check(ex.mud_public_key(s.buf(arg(seed, 32)), o)); return u8().slice(o, o + 32); }); }

  /** The peer's bytes, to write to flash. They contain the key, and cannot send until restore() has resumed them. @returns {Uint8Array} */
  save() { if (!this.#mem) throw new MudError(-1); const o = alloc(this.#block); try { check(ex.mud_save(this.#mem, this.#size, o, this.#size)); return u8().slice(o, o + this.#size); } finally { ex.mud_free(o, this.#block); } }
  /** A higher epoch for a live peer; clears "counter exhausted". @param {number} epoch */
  renew(epoch) { check(ex.mud_renew(this.#mem, this.#size, epoch)); }
  /** The next fleet key, before the flag day. @param {Uint8Array} key */
  stage(key) { scope((s) => check(ex.mud_stage(this.#mem, this.#size, s.buf(arg(key, 32))))); }
  /** Previous ← current ← next. */
  rotate() { check(ex.mud_rotate(this.#mem, this.#size)); }
  /** Drop the previous key; late wires under it are refused from now on. */
  retire() { check(ex.mud_retire(this.#mem, this.#size)); }
  /** Which key slot opened the last accepted wire. @returns {Slot} */
  opened() { return Slot[check(ex.mud_opened(this.#mem, this.#size))]; }
  /** Live table entries evicted, or senders refused a window, for want of room; rising means replay protection is degraded. @returns {number} */
  pressure() { return ex.mud_pressure(this.#mem, this.#size) >>> 0; }
  /** Calls in flight, of 16: at 16 a call is full. @returns {number} */
  pending() { return ex.mud_pending(this.#mem, this.#size) >>> 0; }
  /** Items queued for the transport, of 16: at 16 every producer is full. @returns {number} */
  queued() { return ex.mud_queued(this.#mem, this.#size) >>> 0; }
  /** The operator's counters, kept across restores: managed (sealer taken for a change), rollbacks, floods, dropped. @returns {{managed: number, rollbacks: number, floods: number, dropped: number}} */
  counts() { const c = (i) => ex.mud_count(this.#mem, this.#size, i) >>> 0; return { managed: c(0), rollbacks: c(1), floods: c(2), dropped: c(3) }; }

  /** Queues a call. Returns its id; the reply carries the same id, or tick() reports it expired. @param {number} now @param {string} service @param {string} method @param {Uint8Array} request @param {number} ttlSecs @returns {Uint8Array} */
  call(now, service, method, request, ttlSecs) {
    return scope((s) => { check(ex.mud_call(this.#mem, this.#size, now, s.text(service), s.text(method), s.buf(request), ttlSecs, R.id)); return u8().slice(R.id, R.id + 12); });
  }

  /** Queues an event on a topic; no reply. @param {number} now @param {string} topic @param {Uint8Array} payload @param {number} ttlSecs */
  publish(now, topic, payload, ttlSecs) { scope((s) => check(ex.mud_publish(this.#mem, this.#size, now, s.text(topic), s.buf(payload), ttlSecs))); }

  /** Announce and subscribe, one act. @param {string} topic */
  listen(topic) { scope((s) => check(ex.mud_listen(this.#mem, this.#size, s.text(topic)))); }

  /** Answers a call exactly as deliver() returned it; in one piece by default, or as a stream with seq 0..n and end on the last. @param {number} now @param {Incoming} call @param {number} status @param {Uint8Array} payload, and detail the status's text */
  reply(now, call, status, payload, end = true, seq = 0, detail = '') {
    if (call.kind !== 'call') throw new MudError(-1);
    arg(call.id, 12);
    scope((s) => {
      const c = R.in, b = u8();
      b.fill(0, c, c + IN.size);
      b.set(call.id, c + IN.id);
      for (const [off, buf] of [[IN.topic, s.text(call.topic)], [IN.method, s.text(call.method)], [IN.reply_to, s.text(call.replyTo)], [IN.bytes, s.buf(call.bytes)]]) u8().copyWithin(c + off, buf, buf + 8);
      const d = mem();
      d.setInt32(c + IN.kind, 1, true); d.setUint32(c + IN.ttl_secs, call.ttlSecs, true); d.setUint32(c + IN.seq, call.seq, true); d.setInt32(c + IN.end, call.end ? 1 : 0, true);
      check(ex.mud_reply(this.#mem, this.#size, now, c, seq, status, s.text(detail), s.buf(payload), end ? 1 : 0));
    });
  }

  /** A wire from the transport. Every wire gets a verdict; a dropped kind names why. @param {number} now @param {string} topic @param {Uint8Array} wire @returns {Incoming} */
  deliver(now, topic, wire) {
    return scope((s) => { check(ex.mud_deliver(this.#mem, this.#size, now, s.text(topic), s.buf(wire), s.span(this.#plain, MSG), R.in)); return wrap(R.in); });
  }

  /** Time passes: the next expired call, or null. Call until null. @param {number} now @returns {Incoming|null} */
  tick(now) { return check(ex.mud_tick(this.#mem, this.#size, now, R.in)) === 0 ? null : wrap(R.in); }

  /** Next item for the transport, left in place until sent(). Null when the outbox is empty. @param {number} now @returns {Outgoing|null} */
  outgoing(now) {
    const kind = scope((s) => check(ex.mud_outgoing(this.#mem, this.#size, now, s.span(this.#scratch, MAX_TOPIC + MSG), R.out)));
    return kind === 0 ? null : { listen: kind === 2, topic: strAt(R.out + OUT.topic), wire: bytesAt(R.out + OUT.wire), ttlSecs: mem().getUint32(R.out + OUT.ttl_secs, true) };
  }

  /** The transport took the item outgoing() showed. */
  sent() { check(ex.mud_sent(this.#mem, this.#size)); }

  /** Ends the peer: its block is zeroed, keys included, then freed. Every later call throws. */
  close() { if (this.#mem) { ex.mud_drop(this.#mem, this.#size); ex.mud_free(this.#mem, this.#block + MAX_TOPIC + MSG + MSG); } this.#mem = 0; }
}

/** A loopback, private or link-local address, v4 or v6: the only kind Udp binds or sends to. JavaScript has no build to opt into more: a public address takes the native library. */
function privateAddress(host) {
  const octet = '(25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)'; // no leading zero, which a system resolver may read as octal: 010.0.0.1 is 8.0.0.1 to glibc
  const v4 = new RegExp(`^${octet}\\.${octet}\\.${octet}\\.${octet}$`).exec(host);
  if (v4) { const [a, b] = [Number(v4[1]), Number(v4[2])]; return a === 127 || a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 169 && b === 254); }
  if (!host.includes(':')) return false;
  const [bare, zone] = host.split('%');
  if (zone !== undefined && !/^[A-Za-z0-9_.-]+$/.test(zone)) return false; // a scope id, as a link-local address needs on Node
  let canonical;
  try { canonical = new URL(`http://[${bare}]`).hostname; } catch { return false; }
  if (zone !== undefined) return (parseInt(canonical.slice(1).split(':')[0] || '0', 16) & 0xffc0) === 0xfe80; // a scope belongs to link-local alone
  if (canonical === '[::1]') return true;
  const first = canonical.slice(1).split(':')[0];
  const seg = first === '' ? 0 : parseInt(first, 16);
  return (seg & 0xfe00) === 0xfc00 || (seg & 0xffc0) === 0xfe80;
}

/** "ip:port" or "[ip]:port" as [host, port], or null. */
function parseAddress(text) {
  const m = /^\[([^\]]+)\]:(\d+)$/.exec(text) ?? /^([^:\[\]]+):(\d+)$/.exec(text);
  if (!m) return null;
  const port = Number(m[2]);
  return port >= 0 && port <= 65535 ? [m[1], port] : null;
}

/** The address as a peer list compares it: an IPv4 address an IPv6 socket reports as ::ffff:a.b.c.d is itself. */
function canonicalHost(host) { const m = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/i.exec(host); return m ? m[1] : host; }

/**
 * A product's UDP on Node: each wire to every listed peer, which keeps it only on a topic its peer listens to; private,
 * loopback or link-local addresses alone. A datagram from an address not listed, on a topic not listened to, or one no
 * wire can be, is dropped unread; past 1024 waiting, one is dropped, as core would drop a flood. Datagram: topic
 * length (1) ‖ topic ‖ wire, as the native library's.
 */
export class Udp {
  #sock; #peers; #to = []; #listens = new Set(); #inbox = []; #closed = false; #fault;
  /** Binds "ip:port" (port 0 allowed) and sends to each peer's, at least one; a bad or public address, or no peer, throws MudError -1 (0.0.0.0 is public), a bind the system refuses -2. @returns {Promise<Udp>} */
  static async open(bind, ...peers) {
    const at = parseAddress(bind), to = peers.map(parseAddress);
    if (!at || !peers.length || to.some((p) => !p || p[1] === 0) || ![at, ...to].every(([h]) => privateAddress(h))) throw new MudError(-1); // a peer has a port; the bind may take 0
    const dgram = await import('node:dgram');
    const u = new Udp();
    u.#peers = new Set(to.map(([h, p]) => `${canonicalHost(h)}|${p}`));
    u.#to = to;
    u.#sock = dgram.createSocket(at[0].includes(':') ? 'udp6' : 'udp4');
    u.#sock.on('message', (msg, from) => u.#take(msg, from));
    u.#sock.on('error', (e) => u.#fault?.(e));
    await new Promise((resolve, reject) => { u.#sock.once('error', () => reject(new MudError(-2))); u.#sock.bind(at[1], at[0], resolve); });
    return u;
  }
  /** Whether this binding may bind or send to a public address: never, in JavaScript. */
  static internet() { return false; }
  /** The bound port, the one chosen for port 0. */
  get port() { if (this.#closed) throw new MudError(-1); return this.#sock.address().port; }
  /** Whether the socket holds the Node process; a MudTransport holds it while it has work. */
  ref() { this.#sock?.ref(); } unref() { this.#sock?.unref(); }
  /** Hears the socket's own errors, an earlier send's among them. */
  set fault(f) { this.#fault = f; }
  /** One outgoing item: a listen keeps wires on its topic from then; a send goes to every listed peer, and UDP says nothing of whether one took it. */
  send(o) {
    if (this.#closed) throw new MudError(-1);
    if (o.listen) { this.#listens.add(o.topic); return; }
    const t = ENC.encode(o.topic);
    if (!t.length || t.length > 64 || !o.wire.length || o.wire.length > 512) throw new MudError(-1);
    const d = new Uint8Array(1 + t.length + o.wire.length);
    d[0] = t.length; d.set(t, 1); d.set(o.wire, 1 + t.length);
    for (const [h, p] of this.#to) this.#sock.send(d, p, h, (e) => { if (e) this.#fault?.(e); });
  }
  #take(msg, from) {
    if (this.#inbox.length >= 1024 || !this.#peers.has(`${canonicalHost(from.address)}|${from.port}`)) return;
    const tl = msg[0];
    if (!tl || tl > 64 || msg.length < 2 + tl || msg.length - 1 - tl > 512) return;
    let topic;
    try { topic = new TextDecoder('utf-8', { fatal: true }).decode(msg.subarray(1, 1 + tl)); } catch { return; }
    if (this.#listens.has(topic)) this.#inbox.push([topic, new Uint8Array(msg.subarray(1 + tl))]);
  }
  /** The next [topic, wire] from a listed peer, or null. */
  poll() { if (this.#closed) throw new MudError(-1); return this.#inbox.shift() ?? null; }
  close() { if (!this.#closed) { this.#closed = true; this.#sock?.close(); } }
}

/** The simulator: one world from scenario text, radios 0..peers-1. A development tool. */
export class Sim {
  static EVENT_KINDS = ['', 'partition', 'heal', 'jam', 'unjam', 'malformed', 'replay', 'flood', 'bandwidth', 'peer_down', 'peer_up', 'clone', 'spam', 'stage', 'rotate', 'retire', 'warp', 'hostile', 'noise', 'stale_reboot', 'torn_save', 'revoke', 'telemetry', 'hold'];
  #sim; #scratch = alloc(MAX_TOPIC + MSG);
  peers; lastEventMs;

  /** @param {string} scenario */
  constructor(scenario) {
    this.#sim = scope((s) => ex.mud_sim_new(s.text(scenario), R.line));
    if (!this.#sim) { this.close(); throw new Error('scenario error at line ' + mem().getInt32(R.line, true)); }
    this.peers = ex.mud_sim_peers(this.#sim);
    this.lastEventMs = Number(ex.mud_sim_last_event_ms(this.#sim));
  }

  nowMs() { return Number(ex.mud_sim_now_ms(this.#sim)); }
  step(dtMs = 100) { ex.mud_sim_step(this.#sim, BigInt(dtMs)); }
  /** A host line in the log. @param {string} text */
  note(text) { scope((s) => ex.mud_sim_note(this.#sim, s.text(text))); }
  silent(radio, on) { ex.mud_sim_silent(this.#sim, radio, on ? 1 : 0); }
  /** False when the radio is down; the item stays with the peer. @param {number} radio @param {string} topic @param {Uint8Array} wire @param {number} ttlSecs */
  send(radio, topic, wire, ttlSecs) { return Sim.up(scope((s) => ex.mud_sim_send(this.#sim, radio, s.text(topic), s.buf(wire), ttlSecs))); }
  listen(radio, topic) { return Sim.up(scope((s) => ex.mud_sim_listen(this.#sim, radio, s.text(topic)))); }
  static up(code) { if (code === -1) throw new MudError(code); return code === 0; }

  /** What a radio handed up: kind 1 a message with its topic and wire, 2 on-mesh, 3 off-mesh; null when idle. @returns {{ kind: number, topic: string, wire: Uint8Array }|null} */
  poll(radio) {
    const k = scope((s) => check(ex.mud_sim_poll(this.#sim, radio, s.span(this.#scratch, MAX_TOPIC + MSG), R.topic, R.wire)));
    return k === 0 ? null : { kind: k, topic: k === 1 ? strAt(R.topic) : '', wire: k === 1 ? bytesAt(R.wire) : new Uint8Array(0) };
  }

  /** @returns {SimEvent[]} */
  events() {
    const list = [], e = R.event;
    for (let i = 0; i < ex.mud_sim_events(this.#sim); i++) {
      check(ex.mud_sim_event(this.#sim, i, e));
      const d = mem();
      list.push({ atMs: Number(d.getBigUint64(e + EVENT.at_ms, true)), kind: Sim.EVENT_KINDS[d.getInt32(e + EVENT.kind, true)], a: Number(d.getBigUint64(e + EVENT.a, true)), b: Number(d.getBigUint64(e + EVENT.b, true)), text: strAt(e + EVENT.text) });
    }
    return list;
  }

  /** The world's log, one line per entry. @returns {string} */
  log() {
    return scope((s) => {
      const n = check(ex.mud_sim_log(this.#sim, s.span(0, 0))), out = s.take(n);
      ex.mud_sim_log(this.#sim, s.span(out, n));
      return DEC.decode(u8().subarray(out, out + n));
    });
  }

  close() { if (this.#sim) ex.mud_sim_free(this.#sim); this.#sim = 0; if (this.#scratch) ex.mud_free(this.#scratch, MAX_TOPIC + MSG); this.#scratch = 0; }
}
