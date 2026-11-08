// Sensor telemetry over UDP in JavaScript: two sensors publish a Reading each on a topic a gateway listens to, and the
// gateway calibrates one by a call, over mudRPC's own API with the messages protoc-gen-es generated from sensor.proto.
import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { createSocket } from 'node:dgram';
import { AckSchema, OffsetSchema, ReadingSchema } from './gen/sensor_pb.js';
import { Peer, Udp } from '../../../bindings/js/mud.js';

const KEY = Uint8Array.from({ length: 32 }, (_, i) => i + 1); // a product provisions 32 random bytes to every peer
const EPOCH = 1, TOPIC = 'sensors.Readings', SERVICE = 'demo.sensors.Sensor';
const free = () => new Promise((r) => { const s = createSocket('udp4'); s.bind(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => r(p)); }); });
const ports = [await free(), await free(), await free()];
const udps = [];
for (const p of ports) udps.push(await Udp.open(`127.0.0.1:${p}`, ...ports.filter((q) => q !== p).map((q) => `127.0.0.1:${q}`)));
const peers = ['gateway', 'sensor-1', 'sensor-2'].map((name, i) => new Peer(name, KEY, i + 1, EPOCH));
const [gateway, sensor1, sensor2] = peers;

gateway.listen(TOPIC);
sensor2.listen(SERVICE);
sensor1.publish(0, TOPIC, toBinary(ReadingSchema, create(ReadingSchema, { sensor: 'sensor-1', centiDegrees: 2150 })), 60);
sensor2.publish(0, TOPIC, toBinary(ReadingSchema, create(ReadingSchema, { sensor: 'sensor-2', centiDegrees: 1900 })), 60);

const degrees = (c) => `${(c / 100).toFixed(2)}C`;
const readings = [];
let calibrate = null, done = false;
function heard(peer, now, m) {
  if (m.kind === 'event') {
    readings.push(fromBinary(ReadingSchema, m.bytes));
    if (readings.length < 2) return;
    for (const r of readings.sort((a, b) => a.sensor.localeCompare(b.sensor))) console.log(`reading ${r.sensor} ${degrees(r.centiDegrees)}`);
    calibrate = gateway.call(now, SERVICE, 'Calibrate', toBinary(OffsetSchema, create(OffsetSchema, { centiDegrees: 50 })), 10);
  } else if (m.kind === 'call') {
    const offset = fromBinary(OffsetSchema, m.bytes);
    peer.reply(now, m, 0, toBinary(AckSchema, create(AckSchema, { sensor: 'sensor-2', offsetCentiDegrees: offset.centiDegrees })));
  } else if (m.kind === 'reply' && calibrate && m.id.every((b, k) => b === calibrate[k])) {
    const ack = fromBinary(AckSchema, m.bytes);
    console.log(`calibrated ${ack.sensor} by ${degrees(ack.offsetCentiDegrees)}`);
    done = true;
  }
}

const start = performance.now();
while (!done) {
  const now = Math.floor((performance.now() - start) / 1000);
  peers.forEach((peer, i) => {
    for (let o; (o = peer.outgoing(now)); peer.sent()) udps[i].send(o);
    for (let w; (w = udps[i].poll());) heard(peer, now, peer.deliver(now, w[0], w[1]));
    while (peer.tick(now));
  });
  await new Promise((r) => setTimeout(r, 5));
}
for (const p of peers) p.close();
for (const u of udps) u.close();
