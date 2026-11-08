"""The standard harness in Python: must produce the same log as adapters/harness.rs, byte for byte.
usage: harness.py <scenario file> [--signed] [--gcm]"""
import sys
import mud

OPEN = ["", "Short", "Tag", "Full", "Sig", "Roster", "Budget"]
DECODE = ["", "Codec(Truncated)", "Codec(Overflow)", "Codec(Tag)", "Codec(Full)", "Codec(Utf8)", "Missing", "Id"]
PLAIN = {"duplicate": "Duplicate", "no_pending": "NoPending", "topic": "Topic", "seq": "Seq", "id": "Id", "replay": "Replay", "rate": "Rate"}
counting = lambda start: bytes((start + i) & 0xFF for i in range(32))   # as every harness derives them; a constant secret is refused
KEY, NEXT_KEY = counting(0), counting(32)
ERRORS = {-2: "Full", -3: "TooLarge", -4: "Corrupt", -6: "Epoch", -7: "Unprovisioned", -8: "Exhausted", -9: "Rate"}


def refused(f, *args):  # rpc::Error's name if the peer refuses a call, publish or reply, as the Rust harness logs it
    try: f(*args)
    except mud.MudError as e: return ERRORS.get(e.code, "?")


def dropped_name(inc):
    d = inc.dropped
    if d == "version": return f"Version({inc.status})"
    if d == "open": return f"Open({OPEN[inc.status]})" if 1 <= inc.status <= 6 else "?"
    if d == "decode": return f"Decode({DECODE[inc.status] if inc.status < 8 else '?'})"
    return PLAIN.get(d, "?")


def q(b: bytes) -> str:  # Rust's {:?}: quotes and backslash escaped, \t \r \n \0 by name, other ASCII controls as \u{xx}
    out = '"'
    for ch in b.decode("utf-8", "replace"):
        o = ord(ch)
        if ch in '"\\': out += "\\" + ch
        elif ch in "\n\r\t\0": out += {"\n": "\\n", "\r": "\\r", "\t": "\\t", "\0": "\\0"}[ch]
        elif o < 0x20 or o == 0x7F: out += "\\u{%x}" % o
        else: out += ch
    return out + '"'


SIGNED = "--signed" in sys.argv[2:]
GCM = "--gcm" in sys.argv[2:]
PEERS = 0  # set by main before any peer is built


def new_peer(name, sender, epoch, listen_nav):
    p = mud.Peer(name, KEY, sender, epoch, seed=counting(sender) if SIGNED else None, hub=SIGNED and sender == 1, cipher=int(GCM))  # gcs is the hub
    if SIGNED:
        for s in range(1, PEERS + 1):
            try: p.roster(s, mud.Peer.public_key(counting(s)))
            except mud.Full: break  # a leaf believes the hub and the drones that fit
    if listen_nav: p.listen("nav")
    return p


def main(path):
    sim = mud.Sim(open(path, encoding="utf-8").read())
    n = sim.peers
    global PEERS
    PEERS = n
    names = ["gcs" if i == 0 else f"drone-{i}" for i in range(n)]
    host = sorted([e for e in sim.events() if e[1] in ("clone", "spam", "stage", "rotate", "retire", "warp", "hostile", "stale_reboot", "torn_save", "revoke", "telemetry", "flood")], key=lambda e: e[0])
    clones = {e[3] for e in host if e[1] == "clone"}
    peers = [None if i in clones else new_peer(names[i], i + 1, 2, i > 0) for i in range(n)]
    images = [p.save() if p else None for p in peers]  # what each host wrote to flash at boot
    spam = {}  # peer -> [rate, clock]
    flooding, landed = [False] * n, [0] * n  # radios flooding; events each peer took this second
    end = sim.last_event_ms + 50_000
    ping, t, rnd = 2, 0, 0  # gcs's cadence counts rounds, so a warp off the round grid keeps it
    while t <= end:
        now = t // 1000
        warped = False
        if rnd % 10 == 0:
            for i, k in enumerate(landed):
                if k > 0 and any(flooding): sim.note(f"peer{i} honest events landed this second: {k}")
            landed = [0] * n
        while host and host[0][0] <= t:
            at, kind, a, b, text = host.pop(0)
            if kind == "clone": peers[b] = new_peer(names[a], a + 1, 1, True); sim.note(f"host: radio {b} is a clone of peer{a} from an old image")
            elif kind == "spam":
                spam.pop(a, None)
                if b > 0: spam[a] = [b, 1_000_000]
                sim.note(f"host: peer{a} spams at {b}/s")
            elif kind == "stage": [p.stage(NEXT_KEY) for p in peers if p]; sim.note("host: every peer stages the next key")
            elif kind == "rotate":
                if peers[a]: peers[a].rotate()
                sim.note(f"host: peer{a} rotates")
            elif kind == "retire": [p.retire() for p in peers if p]; sim.note("host: every peer retires the previous key")
            elif kind == "stale_reboot": peers[a] = mud.Peer.restore(images[a], 3); sim.note(f"host: peer{a} reboots from its boot image at epoch 3, which its storage never kept")
            elif kind == "torn_save":
                latest, half = peers[a].save(), len(images[a]) // 2
                try: peers[a] = mud.Peer.restore(latest[:half] + images[a][half:], 3); sim.note(f"host: peer{a} reboots from a torn block, accepted")
                except mud.Corrupt: sim.note(f"host: peer{a} reboots from a torn block, refused as Corrupt; booting from the boot image"); peers[a] = mud.Peer.restore(images[a], 3)
            elif kind == "hostile":
                sim.note(f"host: peer{a} calls a hostile method")
                if peers[a] and (e := refused(peers[a].call, now, "nav", text, b"x", 20)): sim.note(f"peer{a} refused call {e}")
            elif kind == "revoke":
                if SIGNED and peers[a]: peers[a].revoke(b + 1)
                sim.note(f"host: peer{a} revokes peer{b}'s roster line")
            elif kind == "telemetry":
                sim.note("host: every drone publishes its position to gcs")
                for i, p in enumerate(peers):
                    if i and p and (e := refused(p.publish, now, "gcs", b"pos", 600)): sim.note(f"peer{i} refused publish {e}")
            elif kind == "flood": flooding[a] = b > 0
            elif kind == "warp": sim.note(f"host: the clock jumps {a} ms"); sim.step(a); t += a; end = max(end, t + 50_000); warped = True; break
        if warped: continue
        if peers[0]:
            if rnd == 0:
                peers[0].call(now, "nav", "Ping", b"1", 60); peers[0].call(now, "nav", "Ping", b"2", 10)
                sim.note("gcs calls Ping#1 ttl 60 and Ping#2 ttl 10")
            elif rnd % 150 == 0:
                ping += 1
                e = refused(peers[0].call, now, "nav", "Ping", str(ping).encode(), 20)
                sim.note(f"peer0 refused call {e}" if e else f"gcs calls Ping#{ping} ttl 20")
        for i, p in enumerate(peers):
            if p is None: continue
            calls, notes = [], []
            if i in spam:
                for _ in range(spam[i][0] * 100 // 1000):
                    spam[i][1] += 1
                    if e := refused(p.publish, spam[i][1], "nav", b"spam", 5): notes.append((f"peer{i} refused publish {e}", False))
            while (o := p.outgoing(now)) is not None:
                ok = sim.send(i, o.topic, o.wire, o.ttl_secs) if o.kind == "send" else sim.listen(i, o.topic)
                if not ok: break
                p.sent()
            while (rx := sim.poll(i)) is not None:
                if rx[0] != "message": continue
                inc = p.deliver(now, rx[1], rx[2])
                if inc.kind == "call": notes.append((f"peer{i} call {q(inc.method.encode())} {q(inc.bytes)}", True)); calls.append(inc)
                elif inc.kind == "reply": notes.append((f"peer{i} reply seq={inc.seq} end={'true' if inc.end else 'false'} status={inc.status} {q(inc.bytes)}", True))
                elif inc.kind == "event": landed[i] += 1; notes.append((f"peer{i} event {q(inc.topic.encode())} {q(inc.bytes)} from sender {inc.sender}", True))
                elif inc.kind == "flood": notes.append((f"peer{i} flood from sender {inc.sender}", False))
                elif inc.kind == "rollback": notes.append((f"peer{i} rollback from sender {inc.sender}", False))
                elif inc.kind == "dropped": notes.append((f"peer{i} dropped {dropped_name(inc)}", False))
            while p.tick(now) is not None: notes.append((f"peer{i} expired", False))
            via = {"previous": " via Previous", "next": " via Next"}.get(p.opened, "")
            for c in calls:
                if e := refused(p.reply, now, c, 0, c.bytes): notes.append((f"peer{i} refused reply {e}", False))
            k = 0
            while k < len(notes):
                run = 1
                while k + run < len(notes) and notes[k + run][0] == notes[k][0]: run += 1
                text, kinded = notes[k]
                sim.note(f"{text} ×{run}" if run > 1 else text + via if kinded else text)
                k += run
        sim.step(100)
        t, rnd = t + 100, rnd + 1
    for i, p in enumerate(peers):
        if p: sim.note(f"peer{i} pressure {p.pressure}")
    sys.stdout.write(sim.log())


if __name__ == "__main__":
    main(sys.argv[1])
