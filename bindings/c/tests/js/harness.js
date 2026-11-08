// The standard harness in JavaScript: must produce the same log as adapters/harness.rs, byte for byte.
// usage: MUDRPC_WASM=<mudrpc.wasm> node harness.js <scenario file> [--signed] [--gcm]
import { readFileSync } from 'node:fs';
import { MudError, Peer, Sim } from '../../../js/mud.js';

const OPEN = ['', 'Short', 'Tag', 'Full', 'Sig', 'Roster', 'Budget'];
const DECODE = ['', 'Codec(Truncated)', 'Codec(Overflow)', 'Codec(Tag)', 'Codec(Full)', 'Codec(Utf8)', 'Missing', 'Id'];
const counting = (start) => Uint8Array.from({ length: 32 }, (_, i) => (start + i) & 0xff);   // as every harness derives them; a constant secret is refused
const KEY = counting(0), NEXT_KEY = counting(32);
const ENC = new TextEncoder(), DEC = new TextDecoder();
const ERRORS = { '-2': 'Full', '-3': 'TooLarge', '-4': 'Corrupt', '-6': 'Epoch', '-7': 'Unprovisioned', '-8': 'Exhausted', '-9': 'Rate' };
// rpc::Error's name if the peer refuses a call, publish or reply, as the Rust harness logs it; null if it took it
const refused = (f) => { try { f(); return null; } catch (e) { if (!(e instanceof MudError)) throw e; return ERRORS[e.code] ?? '?'; } };

function droppedName(inc) {
  switch (inc.dropped) {
    case 'version': return `Version(${inc.status})`;
    case 'open': return inc.status >= 1 && inc.status <= 6 ? `Open(${OPEN[inc.status]})` : '?';
    case 'decode': return `Decode(${inc.status < 8 ? DECODE[inc.status] : '?'})`;
    case 'duplicate': return 'Duplicate'; case 'noPending': return 'NoPending'; case 'topic': return 'Topic'; case 'seq': return 'Seq';
    case 'id': return 'Id'; case 'replay': return 'Replay'; case 'rate': return 'Rate'; default: return '?';
  }
}

// Rust's {:?}: quotes and backslash escaped, \t \r \n \0 by name, other ASCII controls as \u{xx}
const q = (b) => {
  let o = '"';
  for (const ch of typeof b === 'string' ? b : DEC.decode(b)) {
    const c = ch.codePointAt(0);
    o += ch === '"' ? '\\"' : ch === '\\' ? '\\\\' : ch === '\n' ? '\\n' : ch === '\r' ? '\\r' : ch === '\t' ? '\\t' : ch === '\0' ? '\\0' : (c < 0x20 || c === 0x7f) ? `\\u{${c.toString(16)}}` : ch;
  }
  return o + '"';
};

const SIGNED = process.argv.includes('--signed'), GCM = process.argv.includes('--gcm');
let PEERS = 0;
function newPeer(name, sender, epoch, listenNav) {
  const p = new Peer(name, KEY, sender, epoch, SIGNED ? counting(sender) : null, 1000, SIGNED && sender === 1, GCM ? 1 : 0);   // gcs is the hub
  if (SIGNED) for (let s = 1; s <= PEERS; s++) { try { p.roster(s, Peer.publicKey(counting(s))); } catch (e) { if (e instanceof MudError && e.isFull) break; throw e; } }   // a leaf believes the hub and the drones that fit
  if (listenNav) p.listen('nav');
  return p;
}

const sim = new Sim(readFileSync(process.argv[2], 'utf8'));
const n = sim.peers;
PEERS = n;
const names = Array.from({ length: n }, (_, i) => (i === 0 ? 'gcs' : `drone-${i}`));
const hostKinds = new Set(['clone', 'spam', 'stage', 'rotate', 'retire', 'warp', 'hostile', 'stale_reboot', 'torn_save', 'revoke', 'telemetry', 'flood']);
const host = sim.events().filter((e) => hostKinds.has(e.kind)).sort((x, y) => x.atMs - y.atMs);
const clones = new Set(host.filter((e) => e.kind === 'clone').map((e) => e.b));
const peers = names.map((name, i) => (clones.has(i) ? null : newPeer(name, i + 1, 2, i > 0)));
const images = peers.map((p) => (p ? p.save() : null));   // what each host wrote to flash at boot
const spam = new Map();   // peer -> [rate, clock]
const flooding = names.map(() => false), landed = names.map(() => 0);   // radios flooding; events each peer took this second
let end = sim.lastEventMs + 50_000;
let t = 0, ping = 2, round = 0;   // gcs's cadence counts rounds, so a warp off the round grid keeps it
while (t <= end) {
  const now = Math.floor(t / 1000);
  let warped = false;
  if (round % 10 === 0) landed.forEach((k, i) => { if (k > 0 && flooding.includes(true)) sim.note(`peer${i} honest events landed this second: ${k}`); landed[i] = 0; });
  while (host.length && host[0].atMs <= t) {
    const e = host.shift(), a = e.a, b = e.b;
    switch (e.kind) {
      case 'clone': peers[b] = newPeer(names[a], a + 1, 1, true); sim.note(`host: radio ${b} is a clone of peer${a} from an old image`); break;
      case 'spam': spam.delete(a); if (b > 0) spam.set(a, [b, 1_000_000]); sim.note(`host: peer${a} spams at ${b}/s`); break;
      case 'stage': for (const p of peers) if (p) p.stage(NEXT_KEY); sim.note('host: every peer stages the next key'); break;
      case 'rotate': if (peers[a]) peers[a].rotate(); sim.note(`host: peer${a} rotates`); break;
      case 'retire': for (const p of peers) if (p) p.retire(); sim.note('host: every peer retires the previous key'); break;
      case 'stale_reboot': peers[a] = Peer.restore(images[a], 3); sim.note(`host: peer${a} reboots from its boot image at epoch 3, which its storage never kept`); break;
      case 'torn_save': {
        const latest = peers[a].save(), half = images[a].length >> 1;
        const torn = new Uint8Array(images[a].length); torn.set(latest.subarray(0, half)); torn.set(images[a].subarray(half), half);
        try { peers[a] = Peer.restore(torn, 3); sim.note(`host: peer${a} reboots from a torn block, accepted`); }
        catch (e) { sim.note(`host: peer${a} reboots from a torn block, refused as Corrupt; booting from the boot image`); peers[a] = Peer.restore(images[a], 3); }
        break;
      }
      case 'hostile': {
        sim.note(`host: peer${a} calls a hostile method`);
        const why = peers[a] && refused(() => peers[a].call(now, 'nav', e.text, ENC.encode('x'), 20));
        if (why) sim.note(`peer${a} refused call ${why}`);
        break;
      }
      case 'revoke': if (SIGNED && peers[a]) peers[a].revoke(b + 1); sim.note(`host: peer${a} revokes peer${b}'s roster line`); break;
      case 'telemetry':
        sim.note('host: every drone publishes its position to gcs');
        peers.forEach((p, i) => { const why = i > 0 && p && refused(() => p.publish(now, 'gcs', ENC.encode('pos'), 600)); if (why) sim.note(`peer${i} refused publish ${why}`); });
        break;
      case 'flood': flooding[a] = b > 0; break;
      case 'warp': sim.note(`host: the clock jumps ${a} ms`); sim.step(a); t += a; end = Math.max(end, t + 50_000); warped = true; break;
    }
    if (warped) break;
  }
  if (warped) continue;
  const gcs = peers[0];
  if (gcs) {
    if (round === 0) {
      gcs.call(now, 'nav', 'Ping', ENC.encode('1'), 60); gcs.call(now, 'nav', 'Ping', ENC.encode('2'), 10);
      sim.note('gcs calls Ping#1 ttl 60 and Ping#2 ttl 10');
    } else if (round % 150 === 0) {
      ping++;
      const why = refused(() => gcs.call(now, 'nav', 'Ping', ENC.encode(String(ping)), 20));
      sim.note(why ? `peer0 refused call ${why}` : `gcs calls Ping#${ping} ttl 20`);
    }
  }
  for (let i = 0; i < n; i++) {
    const p = peers[i];
    if (!p) continue;
    const calls = [], notes = [];   // notes: [text, kinded]
    const state = spam.get(i);
    for (let k = 0; state && k < Math.floor(state[0] * 100 / 1000); k++) {
      const why = refused(() => p.publish(++state[1], 'nav', ENC.encode('spam'), 5));
      if (why) notes.push([`peer${i} refused publish ${why}`, false]);
    }
    for (let o; (o = p.outgoing(now));) {
      const ok = o.listen ? sim.listen(i, o.topic) : sim.send(i, o.topic, o.wire, o.ttlSecs);
      if (!ok) break;
      p.sent();
    }
    for (let rx; (rx = sim.poll(i));) {
      if (rx.kind !== 1) continue;
      const inc = p.deliver(now, rx.topic, rx.wire);
      switch (inc.kind) {
        case 'call': notes.push([`peer${i} call ${q(inc.method)} ${q(inc.bytes)}`, true]); calls.push(inc); break;
        case 'reply': notes.push([`peer${i} reply seq=${inc.seq} end=${inc.end} status=${inc.status} ${q(inc.bytes)}`, true]); break;
        case 'event': landed[i]++; notes.push([`peer${i} event ${q(inc.topic)} ${q(inc.bytes)} from sender ${inc.sender}`, true]); break;
        case 'flood': notes.push([`peer${i} flood from sender ${inc.sender}`, false]); break;
        case 'rollback': notes.push([`peer${i} rollback from sender ${inc.sender}`, false]); break;
        case 'dropped': notes.push([`peer${i} dropped ${droppedName(inc)}`, false]); break;
      }
    }
    while (p.tick(now)) notes.push([`peer${i} expired`, false]);
    const slot = p.opened(), via = slot === 'previous' ? ' via Previous' : slot === 'next' ? ' via Next' : '';
    for (const c of calls) { const why = refused(() => p.reply(now, c, 0, c.bytes)); if (why) notes.push([`peer${i} refused reply ${why}`, false]); }
    for (let k = 0; k < notes.length;) {
      let run = 1;
      while (k + run < notes.length && notes[k + run][0] === notes[k][0]) run++;
      const [text, kinded] = notes[k];
      sim.note(run > 1 ? `${text} ×${run}` : kinded ? text + via : text);
      k += run;
    }
  }
  sim.step(100);
  t += 100; round++;
}
for (let i = 0; i < n; i++) if (peers[i]) sim.note(`peer${i} pressure ${peers[i].pressure()}`);
process.stdout.write(Buffer.from(sim.log(), 'utf8'));
