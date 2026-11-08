// The Connect drop-in under test: the descriptors protoc-gen-es generated from drone.proto, a Connect client and a
// Connect service written as Connect code is, over two mudRPC peers in this process. Building the transports is the only
// mudRPC code above them.
import { create, toBinary } from '@bufbuild/protobuf';
import { Code, ConnectError, createClient } from '@connectrpc/connect';
import { createSocket } from 'node:dgram';
import { ArmReplySchema, ArmRequestSchema, Drone, Rover, SampleSchema, TelemetryRequestSchema } from './drone_pb.js';
import { Peer, Udp } from './mud.js';
import { CALLER, MudTransport } from './mud-connect.js';

const KEY = Uint8Array.from({ length: 32 }, (_, i) => i + 1);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const say = (s) => console.log(s);
let heard = false, closedHeard = false, stopped = false;

async function fails(what, f) {
  try { await f(); say(`${what}: no error`); }
  catch (e) { if (e instanceof ConnectError) say(`${what}: ${Code[e.code]} ${e.rawMessage}`); else say(`${what}: ${e.constructor.name}`); }
}
async function thrown(f) { try { await f(); return 'no error'; } catch (e) { return e.constructor.name; } }
async function read(stream, partial) {
  const seen = [];
  try { for await (const s of stream) seen.push(String(s.seq)); }
  catch (e) { if (partial) say(`${partial} after: ${seen.join(' ')}`); throw e; }
  return seen.join(' ');
}
const free = () => new Promise((r) => { const s = createSocket('udp4'); s.bind(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => r(p)); }); });

/** Sockets on loopback that list each other: ports chosen free, chosen again while another test takes one between the choosing and the binding. */
async function udpSet(n) {
  for (let i = 0; ; i++) {
    const ps = await Promise.all(Array.from({ length: n }, free));
    const made = [];
    try { for (const p of ps) made.push(await Udp.open(`127.0.0.1:${p}`, ...ps.filter((q) => q !== p).map((q) => `127.0.0.1:${q}`))); return made; }
    catch (e) { for (const u of made) u.close(); if (e.code !== -2 || i >= 50) throw e; }
  }
}

/** The radio between the transports: drops nothing unless cut, and can hold the drone's wires to deliver them reversed. */
class Link {
  ends = []; native = null; cut = false; hold = 0; held = []; refuseListen = false; lastTtl = 0;
  send(from, o) {
    if (o.listen) { if (this.refuseListen) throw new Error('this radio has no listen'); return; }
    this.lastTtl = o.ttlSecs;
    if (this.cut) return;
    if (this.native) { this.native.inbox.push([o.topic, o.wire]); return; }
    const to = this.ends[1 - from];
    if (from === 1 && this.hold) {
      this.held.push([o.topic, o.wire]);
      if (this.held.length < this.hold) return;
      this.hold = 0;
      for (const [t, w] of this.held.reverse()) to.deliver(t, w);
      this.held = [];
      return;
    }
    to.deliver(o.topic, o.wire);
  }
}

const droneService = {
  async arm(r, ctx) {
    if (r.id === 0) throw new ConnectError('battery low', Code.FailedPrecondition);
    if (r.id === 13) throw new Error('thirteen');
    if (r.id === 23) return { armed: true, by: 'x'.repeat(1000) };
    if (r.id === 27) { const o = Object.create(null); throw o; }
    if (r.id === 28) { await new Promise((res) => ctx.signal.addEventListener('abort', res, { once: true })); closedHeard = ctx.signal.reason?.code === Code.Unavailable; return {}; }
    if (r.id === 24) { await new Promise((res) => ctx.signal.addEventListener('abort', res, { once: true })); heard = true; return {}; }
    return { armed: true, id: r.id, by: `mudrpc:${ctx.values.get(CALLER)}` };
  },
  async *telemetry(r) {
    let i = 0;
    try {
      for (; i < r.count; i++) {
        if (r.failAfter && i === r.failAfter) throw new ConnectError('lost the camera', Code.Aborted);
        if (r.count !== 40) { yield { seq: i }; continue; }
        await sleep(30); // 40 items, the third past the wire
        yield i === 2 ? Object.assign(create(SampleSchema, { seq: i }), { $unknown: [{ no: 9, wireType: 2, data: Uint8Array.of(0xe8, 0x07, ...new Uint8Array(1000)) }] }) : { seq: i };
      }
    } finally { if (r.count === 40) stopped = i < r.count; }
  },
};

/** A mudRPC server with no Connect: Rover's Telemetry sends count items, 2 down to 0, end on the last; Arm refuses. */
class Native {
  inbox = []; back = null;
  constructor(peer) {
    this.peer = peer;
    peer.listen('test.Rover');
    const start = Date.now();
    this.timer = setInterval(() => {
      const now = Math.floor((Date.now() - start) / 1000);
      for (let w; (w = this.inbox.shift());) {
        const c = this.peer.deliver(now, w[0], w[1]);
        if (c.kind !== 'call') continue;
        if (c.method === 'Telemetry' && c.bytes[1] === 99) { this.peer.reply(now, c, 0, Uint8Array.of(0x0a, 0xff, 0xff), true, 0); continue; }
        if (c.method === 'Arm') { this.peer.reply(now, c, 9, new Uint8Array(0), true, 0, 'native says no'); continue; }
        const n = c.bytes.length ? c.bytes[1] : 0;
        for (let i = 0; i < n; i++) this.peer.reply(now, c, 0, toBinary(SampleSchema, create(SampleSchema, { seq: n - 1 - i })), i === n - 1, i);
      }
      for (let o; (o = this.peer.outgoing(now)); this.peer.sent()) if (!o.listen && this.back) this.back.deliver(o.topic, o.wire);
    }, 10);
  }
  stop() { clearInterval(this.timer); }
}

const link = new Link();
const gcs = new MudTransport(new Peer('gcs', KEY, 1, 1), (o) => link.send(0, o));
const drone = new MudTransport(new Peer('drone', KEY, 2, 1), (o) => link.send(1, o));
link.ends = [gcs, drone];
drone.routes((router) => router.service(Drone, droneService));

// From here, the code a Connect product already has.
const client = createClient(Drone, gcs);
const r = await client.arm({ id: 7 });
say(`arm: armed ${r.armed} id ${r.id} by ${r.by}`);
await fails('refused', () => client.arm({ id: 0 }));
await fails('handler throws', () => client.arm({ id: 13 }));
await fails('unknown method', () => client.fly({ id: 1 }));

link.hold = 6; // five items and the stream's end, delivered newest first
say(`telemetry reversed: ${await read(client.telemetry({ count: 5 }))}`);
say(`telemetry empty: ${await read(client.telemetry({ count: 0 }))}`);
await fails('telemetry fails', () => read(client.telemetry({ count: 5, failAfter: 2 }), 'telemetry fails'));
await fails('upload', () => client.upload((async function* () { yield { seq: 1 }; })()));
await fails('too large', () => client.arm({ id: 1, note: new Uint8Array(1000) }));

link.cut = true;
let t0 = Date.now();
await fails('deadline', () => client.arm({ id: 9 }, { timeoutMs: 2000 }));
const took = (Date.now() - t0) / 1000;
say(`deadline in time: ${took >= 2.0 && took < 3.5}`);
const ac = new AbortController();
setTimeout(() => ac.abort(), 300);
await fails('cancelled', () => client.arm({ id: 10 }, { signal: ac.signal }));
await fails('past the deadline', () => client.arm({ id: 1 }, { timeoutMs: -1 }));
link.cut = false;
say(`after the cut: armed ${(await client.arm({ id: 11 })).armed}`);

await fails('a reply past the wire', () => client.arm({ id: 23 }));
const mid = [];
const midEnd = await (async () => { for await (const s of client.telemetry({ count: 40 })) mid.push(s.seq); })().then(() => 'no error', (e) => `${Code[e.code]} ${e.rawMessage}`);
await sleep(100);
say(`an item past the wire mid-stream: ${mid.join(' ')}, ${midEnd}, its handler stopped: ${stopped}`);
const badName = await gcs.unary({ ...Drone.method.arm, name: '\ud800' }, undefined, undefined, {}, { id: 1 }).then(() => 'no error', (e) => `${Code[e.code]} ${e.rawMessage}`);
say(`a method name that is not UTF-8: ${badName}, then armed ${(await client.arm({ id: 1 })).armed}`);
const longService = await gcs.unary({ ...Drone.method.arm, parent: { ...Drone, typeName: 'a'.repeat(65) } }, undefined, undefined, {}, { id: 1 }).then(() => 'no error', (e) => `${Code[e.code]} ${e.rawMessage}`);
say(`a service name of 65 bytes: ${longService}`);
say(`two full streams back to back: ${(await read(client.telemetry({ count: 63 }))).split(' ').length} ${(await read(client.telemetry({ count: 63 }))).split(' ').length}`);
await fails('a 64th item', () => read(client.telemetry({ count: 64 })));

const lonePeer = new Peer('lone', KEY, 9, 1);
const lone = new MudTransport(lonePeer, () => {});
lonePeer.close();
await sleep(200); // the pump's next round finds the peer gone and closes the transport
await fails('the peer closed first', () => createClient(Drone, lone).arm({ id: 1 }, { timeoutMs: 2000 }));

const free16 = 16 - gcs.use((p) => p.pending()); // a call cancelled after sending keeps its slot until its ttl
const codes = new Set(await Promise.all(Array.from({ length: free16 }, () => client.fly({ id: 1 }, { timeoutMs: 5000 }).then(() => 'OK', (e) => Code[e.code]))));
say(`unknown method, every free slot at once: ${codes.size === 1 && codes.has('Unimplemented') ? 'all Unimplemented' : [...codes]}`);
say(`a null wire: ${await thrown(() => gcs.deliver('gcs', null))}`);
link.cut = true;
client.arm({ id: 1 }, { timeoutMs: 8 * 86400 * 1000 }).catch(() => {});
await sleep(200);
link.cut = false;
say(`a deadline eight days out: ttl ${link.lastTtl}`);
for (let i = 0; i < 5000; i++) gcs.deliver('gcs', new Uint8Array(40));
say(`five thousand junk wires: then armed ${(await client.arm({ id: 1 })).armed}`);

const reading = new AbortController();
const stream = client.telemetry({ count: 5 }, { signal: reading.signal })[Symbol.asyncIterator]();
await stream.next();
await sleep(500); // every item has landed
reading.abort();
await fails('cancelled with items unread', () => stream.next());
t0 = Date.now();
await fails('a slow reader past its deadline', async () => { for await (const s of client.telemetry({ count: 5 }, { timeoutMs: 1000 })) await sleep(600); });
say(`the slow reader's deadline in time: ${(Date.now() - t0) / 1000 < 2.0}`);

const link3 = new Link();
link3.refuseListen = true;
const faults = [];
const gcs3 = new MudTransport(new Peer('gcs3', KEY, 7, 1), (o) => link3.send(0, o), { fault: (e) => faults.push(e) });
const drone3 = new MudTransport(new Peer('drone3', KEY, 8, 1), (o) => link3.send(1, o));
link3.ends = [gcs3, drone3];
drone3.service(Drone, droneService);
say(`a transport refusing a listen: armed ${(await createClient(Drone, gcs3).arm({ id: 1 }, { timeoutMs: 10000 })).armed}, faults heard ${faults.length ? 'yes' : 'no'}`);

let selfish = null;
selfish = new MudTransport(new Peer('selfish', KEY, 10, 1), () => selfish.close());
await sleep(300);
say(`closed from its own send: ${await thrown(() => selfish.close())}`);

const [gu, du] = await udpSet(2);
const gcsU = new MudTransport(new Peer('gcsu', KEY, 11, 1), gu);
const droneU = new MudTransport(new Peer('droneu', KEY, 12, 1), du);
droneU.service(Drone, droneService);
const overUdp = createClient(Drone, gcsU);
say(`over UDP: armed ${(await overUdp.arm({ id: 1 }, { timeoutMs: 5000 })).armed}, telemetry ${await read(overUdp.telemetry({ count: 5 }))}`);
let atOnce = 0;
for (let i = 0; i < 20; i++) { // a server registered and called at once: its listen is on the socket before the call
  const [cu, su] = await udpSet(2), cp = new Peer('caller', KEY, 19, 1), sp = new Peer('server', KEY, 20, 1);
  const server = new MudTransport(sp, su).service(Drone, droneService), caller = new MudTransport(cp, cu);
  if ((await createClient(Drone, caller).arm({ id: 1 }, { timeoutMs: 5000 })).armed) atOnce++;
  for (const x of [server, caller, sp, cp, su, cu]) x.close();
}
say(`twenty servers called at once: ${atOnce} of 20 armed`);
const lostSocket = await Udp.open('127.0.0.1:0', `127.0.0.1:${du.port}`);
const lost = new MudTransport(new Peer('lost', KEY, 13, 1), lostSocket);
lostSocket.close();
await sleep(100);
await fails('its socket closed', () => createClient(Drone, lost).arm({ id: 1 }, { timeoutMs: 2000 }));
say(`a public bind: ${await thrown(() => Udp.open('0.0.0.0:0', '127.0.0.1:9'))}, a public peer: ${await thrown(() => Udp.open('127.0.0.1:0', '8.8.8.8:53'))}, this binding public: ${Udp.internet()}`);

await fails('a server that never answers', () => client.arm({ id: 24 }, { timeoutMs: 1000 }));
await sleep(3000);
say(`the server saw its ttl pass: ${heard}`);
say(`a service name past 64 bytes: ${await thrown(() => drone.service({ ...Drone, typeName: 'a'.repeat(70), methods: [] }, {}))}`);
say(`a service name with a lone surrogate: ${await thrown(() => drone.service({ ...Drone, typeName: 'test.\ud800', methods: [] }, {}))}`);

const three = await udpSet(3);
const [gT, dT, iT] = three.map((u, i) => new MudTransport(new Peer(`node${i}`, KEY, 21 + i, 1), u));
dT.service(Drone, droneService);
let armed = 0;
for (let i = 0; i < 10; i++) if ((await createClient(Drone, gT).arm({ id: 1 }, { timeoutMs: 5000 })).armed) armed++;
say(`three on UDP, one serving nothing: ${armed} of 10 armed`);

await fails('a throw nothing can print', () => client.arm({ id: 27 }));
say(`an octal or scoped address: ${await thrown(() => Udp.open('127.0.0.1:0', '010.0.0.1:7000'))}, ${await thrown(async () => (await Udp.open('127.0.0.1:0', '[fe80::1%lo0]:7000')).close())}, a peer at port 0: ${await thrown(() => Udp.open('127.0.0.1:0', '127.0.0.1:0'))}`);
say(`a radio that is neither: ${await thrown(() => new MudTransport(new Peer('none', KEY, 15, 1), null))}`);
say(`a timeout of null: armed ${(await client.arm({ id: 1 }, { timeoutMs: null })).armed}`);
{
  const p = new Peer('soon', KEY, 16, 1);
  const soon = new MudTransport(p, () => {});
  p.close();
  await fails('a call right after its peer closed', () => createClient(Drone, soon).arm({ id: 1 }));
}
{
  const linkC = new Link();
  const gcsC = new MudTransport(new Peer('gcsc', KEY, 17, 1), (o) => linkC.send(0, o));
  const droneC = new MudTransport(new Peer('dronec', KEY, 18, 1), (o) => linkC.send(1, o));
  linkC.ends = [gcsC, droneC];
  droneC.service(Drone, droneService);
  const waiting = createClient(Drone, gcsC).arm({ id: 28 }, { timeoutMs: 30000 }).catch(() => {});
  await sleep(300);
  droneC.close();
  gcsC.close();
  await waiting;
  await sleep(50);
  say(`a handler hears its server close: ${closedHeard}`);
}

// A native peer answers the Rover service with the plain API: a stream ends on its last item, and a refusal has a detail.
const native = new Native(new Peer('rover', KEY, 3, 1));
const link2 = new Link();
const gcs2 = new MudTransport(new Peer('gcs2', KEY, 4, 1), (o) => link2.send(0, o));
link2.ends = [gcs2];
link2.native = native;
native.back = gcs2;
const rover = createClient(Rover, gcs2);
say(`native stream: ${await read(rover.telemetry({ count: 3 }))}`);
await fails('native refusal', () => rover.arm({ id: 1 }));
await fails('a reply that does not decode', () => read(rover.telemetry({ count: 99 })));
native.stop();
// 48 callers owed a stream each, past the 32 a second: the next caller waits its turn, not behind them all.
const crowd = Array.from({ length: 48 }, (_, i) => new Peer(`crowd${i}`, KEY, 100 + i, 1));
for (const p of crowd) {
  p.call(0, 'test.Drone', 'Telemetry', toBinary(TelemetryRequestSchema, create(TelemetryRequestSchema, { count: 20 })), 5);
  for (let o; (o = p.outgoing(0)); p.sent()) if (!o.listen) drone.deliver(o.topic, o.wire);
}
await sleep(300);
say(`a caller behind 48 owed a stream each: armed ${await client.arm({ id: 1 }, { timeoutMs: 3000 }).then((a) => a.armed, () => false)}`);
for (const p of crowd) p.close();
for (const t of [gcs, drone, gcs2, gcs3, drone3, gcsU, droneU, lost, gT, dT, iT]) t.close();
for (const u of [gu, du, ...three]) u.close();
