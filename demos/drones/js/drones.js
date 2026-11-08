// gRPC drone telemetry in JavaScript: the Connect client and service protoc-gen-es generated from drone.proto, run over
// two mudRPC peers on UDP in this process. Building the two transports is the only mudRPC code.
import { Code, ConnectError, createClient } from '@connectrpc/connect';
import { createSocket } from 'node:dgram';
import { Drone } from './gen/drone_pb.js';
import { Peer, Udp } from './vendor/mud.js';
import { MudTransport } from './vendor/mud-connect.js';

const KEY = Uint8Array.from({ length: 32 }, (_, i) => i + 1); // a product provisions 32 random bytes to every peer
const EPOCH = 1;
const free = () => new Promise((r) => { const s = createSocket('udp4'); s.bind(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => r(p)); }); });
const [gcsPort, dronePort] = [await free(), await free()];
const gcsUdp = await Udp.open(`127.0.0.1:${gcsPort}`, `127.0.0.1:${dronePort}`);
const droneUdp = await Udp.open(`127.0.0.1:${dronePort}`, `127.0.0.1:${gcsPort}`);
const gcsPeer = new Peer('gcs', KEY, 1, EPOCH), dronePeer = new Peer('drone', KEY, 2, EPOCH);
const gcs = new MudTransport(gcsPeer, gcsUdp), drone = new MudTransport(dronePeer, droneUdp);

let armed = false;
drone.service(Drone, {
  arm() { armed = true; return { armed: true, drone: 'drone-1' }; },
  async *telemetry(r) { for (let i = 0; i < r.samples; i++) yield { seq: i, altitudeM: 10 * (i + 1), batteryPct: 100 - 2 * i }; },
  land() {
    if (!armed) throw new ConnectError('not armed', Code.FailedPrecondition);
    armed = false;
    return { landed: true };
  },
});

// From here, the code a Connect product already has.
const client = createClient(Drone, gcs), opts = { timeoutMs: 10000 };
try { await client.land({}, opts); } catch (e) { console.log(`land: ${Code[e.code].replace(/(?<=[a-z])(?=[A-Z])/g, '_').toUpperCase()} ${e.rawMessage}`); }
const pilot = 'gcs', arm = await client.arm({ pilot }, opts);
console.log(`armed ${arm.drone} for ${pilot}`);
const samples = [];
for await (const s of client.telemetry({ samples: 3 }, opts)) samples.push(`${s.seq}:${s.altitudeM}m ${s.batteryPct}%`);
console.log(`telemetry ${samples.join(' ')}`);
if ((await client.land({}, opts)).landed) console.log('landed');

for (const t of [gcs, drone]) t.close();
for (const p of [gcsPeer, dronePeer]) p.close();
for (const u of [gcsUdp, droneUdp]) u.close();
