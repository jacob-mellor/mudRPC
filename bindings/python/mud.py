"""mudRPC for Python: one file over the C ABI through ctypes. No dependencies beyond the standard library.

A call is a message; a reply may come hours later or expire; nothing blocks. You poll: drain `outgoing()` into your
transport and call `sent()`, feed what arrives to `deliver()`, call `tick()`. `now` is your uptime in seconds.
"""
import ctypes as C
import os
import sys
import threading
from dataclasses import dataclass
from typing import Optional

__all__ = ["CIPHER_CHACHA20_POLY1305", "CIPHER_AES_256_GCM", "CIPHER_DEFAULT", "Peer", "Sim", "Incoming", "Outgoing", "MudError", "Full", "TooLarge", "Corrupt", "ProtoError", "load", "load_proto", "status_name"]

MAX_TOPIC, MSG = 64, 512
CIPHER_CHACHA20_POLY1305, CIPHER_AES_256_GCM = 0, 1   # as mudrpc.h names them; ChaCha20-Poly1305 smaller and faster without an AES instruction
CIPHER_DEFAULT = CIPHER_AES_256_GCM                   # AES, the NIST algorithm buyers ask for
KIND = {0: "none", 1: "call", 2: "reply", 3: "event", 4: "expired", 5: "dropped", 6: "flood", 7: "rollback"}
DROPPED = {1: "version", 2: "open", 3: "decode", 4: "duplicate", 5: "no_pending", 6: "topic", 7: "seq", 8: "id", 9: "replay", 10: "rate"}
CODES = {-1: "invalid argument", -2: "full", -3: "too large", -4: "corrupt", -5: "bad memory", -6: "epoch not higher", -7: "unprovisioned (sender 0)", -8: "counter exhausted", -9: "this second's sends spent", -10: "busy: another thread holds this peer", -99: "panic"}
SLOT = {0: "current", 1: "previous", 2: "next"}


class MudError(Exception):
    def __init__(self, code):
        super().__init__(f"mudRPC: {CODES.get(code, code)}")
        self.code = code


class Full(MudError): ...
class TooLarge(MudError): ...
class Corrupt(MudError): ...


class ProtoError(Exception):
    """A .proto that did not parse; `line` names the line (0: a missing header)."""
    def __init__(self, line):
        super().__init__(f"proto error at line {line}")
        self.line = line


def _raise(code):
    raise {-2: Full, -3: TooLarge, -4: Corrupt}.get(code, MudError)(code)


class _Buf(C.Structure):
    _fields_ = [("ptr", C.POINTER(C.c_uint8)), ("len", C.c_size_t)]


class _Id(C.Structure):
    _fields_ = [("bytes", C.c_uint8 * 12)]


class _Config(C.Structure):
    _fields_ = [("name", _Buf), ("key", _Buf), ("sender", C.c_uint32), ("epoch", C.c_uint32), ("seed", _Buf), ("budget", C.c_uint32), ("hub", C.c_uint32), ("cipher", C.c_uint32)]


class _Incoming(C.Structure):
    _fields_ = [("kind", C.c_int32), ("id", _Id), ("topic", _Buf), ("method", _Buf), ("reply_to", _Buf), ("detail", _Buf), ("bytes", _Buf),
                ("ttl_secs", C.c_uint32), ("seq", C.c_uint32), ("status", C.c_uint32), ("end", C.c_int32), ("dropped", C.c_int32), ("sender", C.c_uint32)]


class _Event(C.Structure):
    _fields_ = [("at_ms", C.c_uint64), ("kind", C.c_int32), ("a", C.c_uint64), ("b", C.c_uint64), ("text", _Buf)]


class _Out(C.Structure):
    _fields_ = [("kind", C.c_int32), ("topic", _Buf), ("wire", _Buf), ("ttl_secs", C.c_uint32)]


def _buf(b: bytes):
    arr = (C.c_uint8 * max(len(b), 1)).from_buffer_copy(b or b"\0")
    return _Buf(C.cast(arr, C.POINTER(C.c_uint8)), len(b)), arr   # keep `arr` alive while the call runs


def _bytes(b: _Buf) -> bytes:
    return C.string_at(b.ptr, b.len) if b.len else b""


def _text(b: _Buf) -> str:
    return _bytes(b).decode("utf-8", "replace")


_LIB = None


def load(path: Optional[str] = None):
    """Loads libmudrpc from `path`, else $MUDRPC_LIB when set, else next to this file."""
    global _LIB
    if _LIB is not None and path is None:
        return _LIB
    name = {"darwin": "libmudrpc.dylib", "win32": "mudrpc.dll"}.get(sys.platform, "libmudrpc.so")
    p = path or os.environ.get("MUDRPC_LIB") or os.path.join(os.path.dirname(os.path.abspath(__file__)), name)
    if not os.path.exists(p):
        raise FileNotFoundError(f"libmudrpc not found at {p}: build with `cargo build --release -p mudrpc-c`, then set MUDRPC_LIB or put it beside mud.py")
    lib = C.CDLL(p)
    V, B, M = C.c_void_p, _Buf, C.POINTER
    sig = {
        "mud_size": ([], C.c_size_t), "mud_hub_size": ([], C.c_size_t), "mud_align": ([], C.c_size_t),
        "mud_new": ([V, C.c_size_t, M(_Config)], C.c_int32), "mud_save": ([V, C.c_size_t, V, C.c_size_t], C.c_int32), "mud_resume": ([V, C.c_size_t, C.c_uint32], C.c_int32), "mud_renew": ([V, C.c_size_t, C.c_uint32], C.c_int32), "mud_drop": ([V, C.c_size_t], C.c_int32), "mud_roster": ([V, C.c_size_t, C.c_uint32, B, B], C.c_int32), "mud_revoke": ([V, C.c_size_t, C.c_uint32], C.c_int32), "mud_public_key": ([B, M(C.c_uint8)], C.c_int32),
        "mud_stage": ([V, C.c_size_t, B], C.c_int32), "mud_rotate": ([V, C.c_size_t], C.c_int32), "mud_retire": ([V, C.c_size_t], C.c_int32), "mud_opened": ([V, C.c_size_t], C.c_int32),
        "mud_sim_events": ([V], C.c_int32), "mud_sim_event": ([V, C.c_int32, M(_Event)], C.c_int32),
        "mud_call": ([V, C.c_size_t, C.c_uint32, B, B, B, C.c_uint32, M(_Id)], C.c_int32),
        "mud_publish": ([V, C.c_size_t, C.c_uint32, B, B, C.c_uint32], C.c_int32), "mud_listen": ([V, C.c_size_t, B], C.c_int32),
        "mud_reply": ([V, C.c_size_t, C.c_uint32, M(_Incoming), C.c_uint32, C.c_uint32, B, B, C.c_int32], C.c_int32),
        "mud_deliver": ([V, C.c_size_t, C.c_uint32, B, B, _Buf, M(_Incoming)], C.c_int32), "mud_tick": ([V, C.c_size_t, C.c_uint32, M(_Incoming)], C.c_int32),
        "mud_outgoing": ([V, C.c_size_t, C.c_uint32, _Buf, M(_Out)], C.c_int32), "mud_status_name": ([C.c_uint32], C.c_char_p), "mud_sent": ([V, C.c_size_t], C.c_int32), "mud_pressure": ([V, C.c_size_t], C.c_uint32), "mud_pending": ([V, C.c_size_t], C.c_uint32), "mud_queued": ([V, C.c_size_t], C.c_uint32), "mud_count": ([V, C.c_size_t, C.c_int32], C.c_uint32),
        "mud_sim_new": ([B, M(C.c_int32)], V), "mud_sim_free": ([V], None), "mud_sim_peers": ([V], C.c_int32), "mud_sim_last_event_ms": ([V], C.c_uint64),
        "mud_sim_now_ms": ([V], C.c_uint64), "mud_sim_step": ([V, C.c_uint64], None), "mud_sim_note": ([V, B], None),
        "mud_sim_send": ([V, C.c_int32, B, B, C.c_uint32], C.c_int32), "mud_sim_listen": ([V, C.c_int32, B], C.c_int32),
        "mud_sim_poll": ([V, C.c_int32, _Buf, M(_Buf), M(_Buf)], C.c_int32), "mud_sim_silent": ([V, C.c_int32, C.c_int32], None),
        "mud_sim_log": ([V, _Buf], C.c_int32), "mud_gen": ([B, B, _Buf, M(C.c_int32)], C.c_int32),
        "mud_internet": ([], C.c_int32), "mud_udp_new": ([B, B, M(C.c_int32)], V), "mud_udp_free": ([V], None), "mud_udp_port": ([V], C.c_int32),
        "mud_udp_send": ([V, B, B], C.c_int32), "mud_udp_listen": ([V, B], C.c_int32), "mud_udp_poll": ([V, _Buf, M(_Buf), M(_Buf)], C.c_int32),
    }
    for name, (args, ret) in sig.items():
        f = getattr(lib, name)
        f.argtypes, f.restype = args, ret
    _LIB = lib
    return lib


def status_name(code: int) -> str:
    """gRPC's name for a status number, "FAILED_PRECONDITION" for 9, "" past 16."""
    return load().mud_status_name(code).decode()


@dataclass(frozen=True)
class Incoming:
    kind: str
    id: bytes = b""
    topic: str = ""
    method: str = ""
    reply_to: str = ""
    detail: str = ""
    bytes: bytes = b""
    ttl_secs: int = 0
    seq: int = 0
    status: int = 0
    end: bool = False
    dropped: str = ""
    sender: int = 0


@dataclass(frozen=True)
class Outgoing:
    kind: str
    topic: str
    wire: bytes = b""
    ttl_secs: int = 0


def _incoming(r: _Incoming) -> Incoming:
    return Incoming(KIND[r.kind], bytes(r.id.bytes), _text(r.topic), _text(r.method), _text(r.reply_to), _text(r.detail), _bytes(r.bytes),
                    r.ttl_secs, r.seq, r.status, bool(r.end), DROPPED.get(r.dropped, ""), r.sender)


class Peer:
    """A mudRPC peer. `name` is its inbox topic; `key` the 32-byte fleet key; `sender` the roster number (0 cannot send);
    `epoch` a number the host persists beside the key and raises on every boot and restore, written to storage first.

    A signed peer believes 16 senders; `hub=True` is the hub shape, 256, for the host that believes a fleet. The line past
    either raises `Full`. `cipher` is a `CIPHER_` constant. Not thread-safe: one thread drives a peer. The bytes from `save()` contain the key; store them as you store the key.
    """

    def __init__(self, name: str, key: bytes, sender: int, epoch: int, seed: Optional[bytes] = None, budget: int = 1000, hub: bool = False, cipher: int = CIPHER_DEFAULT, _mem=None):
        self._lib = load()
        self._size = self._lib.mud_hub_size() if hub else self._lib.mud_size()
        words = (self._size + 7) // 8
        self._mem = _mem if _mem is not None else (C.c_uint64 * words)()   # 8-byte aligned, as mud_align() requires
        self._scratch = C.create_string_buffer(MAX_TOPIC + MSG)
        self._plain = C.create_string_buffer(MSG)
        if _mem is None:
            if len(key) != 32:
                raise ValueError("the key is 32 bytes")
            nb, keep = _buf(name.encode())
            kb, keep_key = _buf(bytes(key))
            if seed is not None and len(seed) != 32:
                raise ValueError("the seed is 32 bytes")
            sb, keep_seed = _buf(bytes(seed) if seed else b"")
            cfg = _Config(nb, kb, sender, epoch, sb, budget, int(hub), cipher)
            self._check(self._lib.mud_new(self._mem, self._size, C.byref(cfg)))

    @classmethod
    def restore(cls, saved: bytes, epoch: int) -> "Peer":
        """A peer from `save()` bytes, from this same build of the library, with a higher epoch from the host."""
        lib = load()
        if len(saved) not in (lib.mud_size(), lib.mud_hub_size()):
            raise Corrupt(-4)
        mem = (C.c_uint64 * ((len(saved) + 7) // 8))()
        C.memmove(mem, saved, len(saved))
        p = cls("", b"", 0, 0, hub=len(saved) == lib.mud_hub_size(), _mem=mem)
        p._check(lib.mud_resume(mem, len(saved), epoch))
        return p

    def roster(self, sender: int, key: bytes, next: Optional[bytes] = None) -> None:
        """Signed peers: a line for `sender`, its public key and the next one during a rotation."""
        kb, keep = _buf(bytes(key))
        nb, keep2 = _buf(bytes(next) if next else b"")
        self._check(self._lib.mud_roster(self._mem, self._size, sender, kb, nb))

    def revoke(self, sender: int) -> None: self._check(self._lib.mud_revoke(self._mem, self._size, sender))

    @staticmethod
    def public_key(seed: bytes) -> bytes:
        """The 32-byte public key of a seed, for rosters."""
        sb, keep = _buf(bytes(seed))
        out = (C.c_uint8 * 32)()
        Peer._check(load().mud_public_key(sb, out))
        return bytes(out)

    def save(self) -> bytes:
        """The peer's bytes, to write to flash. They contain the key, and cannot send until `restore` has resumed them."""
        if self._mem is None: raise MudError(-1)
        out = (C.c_uint64 * ((self._size + 7) // 8))()
        self._check(self._lib.mud_save(self._mem, self._size, out, self._size))
        return bytes(out)[: self._size]

    def close(self) -> None:
        """Ends the peer: its block is zeroed, keys included. Every later call raises."""
        if getattr(self, "_mem", None) is not None:
            self._lib.mud_drop(self._mem, self._size); self._mem = None

    def __del__(self): self.close()

    def renew(self, epoch: int) -> None:
        """A higher epoch for a live peer; clears `MudError('counter exhausted')`."""
        self._check(self._lib.mud_renew(self._mem, self._size, epoch))

    def stage(self, key: bytes) -> None:
        """The next fleet key, before the flag day."""
        if len(key) != 32:
            raise ValueError("the key is 32 bytes")
        kb, keep = _buf(bytes(key))
        self._check(self._lib.mud_stage(self._mem, self._size, kb))

    def rotate(self) -> None: self._check(self._lib.mud_rotate(self._mem, self._size))
    def retire(self) -> None: self._check(self._lib.mud_retire(self._mem, self._size))

    @property
    def opened(self) -> str:
        """Which key slot opened the last accepted wire: "current", "previous" or "next"."""
        return SLOT[self._check(self._lib.mud_opened(self._mem, self._size))]

    @staticmethod
    def _check(code):
        if code < 0:
            _raise(code)
        return code

    def call(self, now: int, service: str, method: str, req: bytes, ttl_secs: int) -> bytes:
        out = _Id()
        s, ks = _buf(service.encode()); m, km = _buf(method.encode()); r, kr = _buf(req)
        self._check(self._lib.mud_call(self._mem, self._size, now, s, m, r, ttl_secs, C.byref(out)))
        return bytes(out.bytes)

    def publish(self, now: int, topic: str, payload: bytes, ttl_secs: int) -> None:
        t, kt = _buf(topic.encode()); p, kp = _buf(payload)
        self._check(self._lib.mud_publish(self._mem, self._size, now, t, p, ttl_secs))

    def listen(self, topic: str) -> None:
        t, kt = _buf(topic.encode())
        self._check(self._lib.mud_listen(self._mem, self._size, t))

    def reply(self, now: int, call: Incoming, status: int, payload: bytes, end: bool = True, seq: int = 0, detail: str = "") -> None:
        if call.kind != "call":
            raise MudError(-1)
        keep = [_buf(call.topic.encode()), _buf(call.method.encode()), _buf(call.reply_to.encode()), _buf(call.bytes)]
        r = _Incoming(1, _Id((C.c_uint8 * 12)(*call.id)), keep[0][0], keep[1][0], keep[2][0], _Buf(), keep[3][0], call.ttl_secs, call.seq, 0, int(call.end), 0, 0)
        p, kp = _buf(payload)
        d, kd = _buf(detail.encode())
        self._check(self._lib.mud_reply(self._mem, self._size, now, C.byref(r), seq, status, d, p, int(end)))

    def deliver(self, now: int, topic: str, wire: bytes) -> Incoming:
        """Every wire gets a verdict; a `dropped` Incoming names why."""
        out = _Incoming()
        t, kt = _buf(topic.encode()); w, kw = _buf(wire)
        scratch = _Buf(C.cast(self._plain, C.POINTER(C.c_uint8)), MSG)
        self._check(self._lib.mud_deliver(self._mem, self._size, now, t, w, scratch, C.byref(out)))
        return _incoming(out)

    def tick(self, now: int) -> Optional[Incoming]:
        out = _Incoming()
        return _incoming(out) if self._check(self._lib.mud_tick(self._mem, self._size, now, C.byref(out))) else None

    def outgoing(self, now: int) -> Optional[Outgoing]:
        """Next item for the transport, left in place until `sent()`."""
        out = _Out()
        scratch = _Buf(C.cast(self._scratch, C.POINTER(C.c_uint8)), MAX_TOPIC + MSG)
        kind = self._check(self._lib.mud_outgoing(self._mem, self._size, now, scratch, C.byref(out)))
        if kind == 0:
            return None
        return Outgoing("send" if kind == 1 else "listen", _text(out.topic), _bytes(out.wire), out.ttl_secs)

    def sent(self) -> None:
        self._check(self._lib.mud_sent(self._mem, self._size))

    @property
    def pressure(self) -> int:
        return self._lib.mud_pressure(self._mem, self._size)

    @property
    def pending(self) -> int:
        """Calls in flight, of 16: at 16 a call is Full."""
        return self._lib.mud_pending(self._mem, self._size)

    @property
    def queued(self) -> int:
        """Items queued for the transport, of 16: at 16 every producer is Full."""
        return self._lib.mud_queued(self._mem, self._size)

    @property
    def counts(self) -> dict:
        """The operator's counters, kept across restores: managed (sealer taken for a change), rollbacks, floods, dropped."""
        return {k: self._lib.mud_count(self._mem, self._size, i) for i, k in enumerate(("managed", "rollbacks", "floods", "dropped"))}


class Udp:
    """A product's UDP: each wire to every listed peer, on private addresses alone unless the library was built with the
    cargo feature `internet`, which no release is. A channel's pump and the owner's close never meet inside the library."""

    def __init__(self, bind: str, *peers: str):
        """Binds "ip:port" and sends to each peer's, at least one; a bad or public address, or no peer, is MudError -1
        ("0.0.0.0" is public), a bind the system refuses -2."""
        self._lib, self._held = load(), threading.Lock()
        code = C.c_int32(0)
        b, kb = _buf(bind.encode()); p, kp = _buf(" ".join(peers).encode())
        self._udp = self._lib.mud_udp_new(b, p, C.byref(code))
        if not self._udp:
            _raise(code.value)
        self._scratch = C.create_string_buffer(MAX_TOPIC + MSG)

    @staticmethod
    def internet() -> bool:
        """Whether this library may bind or send to a public address."""
        return load().mud_internet() == 1

    def _live(self):
        if not self._udp:
            raise RuntimeError("the socket is closed")
        return self._udp

    @property
    def port(self) -> int:
        """The bound port, the one chosen for port 0."""
        with self._held:
            return Peer._check(self._lib.mud_udp_port(self._live()))

    def send(self, item: "Outgoing") -> None:
        """One outgoing item: a listen keeps wires on its topic from then, every other topic dropped unread; a send raises
        when no peer took the wire, so the item stays queued."""
        t, kt = _buf(item.topic.encode()); w, kw = _buf(item.wire)
        with self._held:
            Peer._check(self._lib.mud_udp_listen(self._live(), t) if item.kind == "listen" else self._lib.mud_udp_send(self._live(), t, w))

    def poll(self) -> Optional[tuple]:
        """The next (topic, wire) from a listed peer, or None."""
        topic, wire = _Buf(), _Buf()
        with self._held:
            scratch = _Buf(C.cast(self._scratch, C.POINTER(C.c_uint8)), MAX_TOPIC + MSG)
            k = Peer._check(self._lib.mud_udp_poll(self._live(), scratch, C.byref(topic), C.byref(wire)))
            return (_text(topic), _bytes(wire)) if k == 1 else None

    def close(self) -> None:
        with self._held:
            if self._udp:
                self._lib.mud_udp_free(self._udp)
                self._udp = None

    def __del__(self):
        if getattr(self, "_udp", None):
            self._lib.mud_udp_free(self._udp)
            self._udp = None


class Sim:
    """The simulator: one world from scenario text, radios 0..peers-1. A development tool; it allocates."""

    def __init__(self, scenario_text: str):
        self._lib = load()
        line = C.c_int32(0)
        t, keep = _buf(scenario_text.encode())
        self._sim = self._lib.mud_sim_new(t, C.byref(line))
        if not self._sim:
            raise ValueError(f"scenario error at line {line.value}")
        self._scratch = C.create_string_buffer(MAX_TOPIC + MSG)
        self.peers = self._lib.mud_sim_peers(self._sim)
        self.last_event_ms = self._lib.mud_sim_last_event_ms(self._sim)

    def __del__(self):
        if getattr(self, "_sim", None):
            self._lib.mud_sim_free(self._sim); self._sim = None

    @property
    def now_ms(self) -> int: return self._lib.mud_sim_now_ms(self._sim)
    def step(self, dt_ms: int = 100) -> None: self._lib.mud_sim_step(self._sim, dt_ms)
    def note(self, text: str) -> None: t, k = _buf(text.encode()); self._lib.mud_sim_note(self._sim, t)
    def silent(self, radio: int, on: bool) -> None: self._lib.mud_sim_silent(self._sim, radio, int(on))

    def send(self, radio: int, topic: str, wire: bytes, ttl_secs: int) -> bool:
        """False when the radio is down (the item stays with the peer)."""
        t, kt = _buf(topic.encode()); w, kw = _buf(wire)
        code = self._lib.mud_sim_send(self._sim, radio, t, w, ttl_secs)
        if code == -1: raise MudError(code)
        return code == 0

    def listen(self, radio: int, topic: str) -> bool:
        t, kt = _buf(topic.encode())
        code = self._lib.mud_sim_listen(self._sim, radio, t)
        if code == -1: raise MudError(code)
        return code == 0

    def poll(self, radio: int):
        """None, or ("message", topic, wire), ("on_mesh",), ("off_mesh",)."""
        topic, wire = _Buf(), _Buf()
        scratch = _Buf(C.cast(self._scratch, C.POINTER(C.c_uint8)), MAX_TOPIC + MSG)
        k = self._lib.mud_sim_poll(self._sim, radio, scratch, C.byref(topic), C.byref(wire))
        if k < 0: raise MudError(k)
        return None if k == 0 else ("message", _text(topic), _bytes(wire)) if k == 1 else ("on_mesh",) if k == 2 else ("off_mesh",)

    def events(self):
        """The scenario's events as (at_ms, kind, a, b, text); kinds 11..17 and 19..22 are the host's to perform."""
        kinds = {1: "partition", 2: "heal", 3: "jam", 4: "unjam", 5: "malformed", 6: "replay", 7: "flood", 8: "bandwidth", 9: "peer_down", 10: "peer_up",
                 11: "clone", 12: "spam", 13: "stage", 14: "rotate", 15: "retire", 16: "warp", 17: "hostile", 18: "noise", 19: "stale_reboot", 20: "torn_save", 21: "revoke", 22: "telemetry", 23: "hold"}
        out = []
        for i in range(self._lib.mud_sim_events(self._sim)):
            e = _Event()
            Peer._check(self._lib.mud_sim_event(self._sim, i, C.byref(e)))
            out.append((e.at_ms, kinds.get(e.kind, "?"), e.a, e.b, _text(e.text)))
        return out

    def log(self) -> str:
        need = self._lib.mud_sim_log(self._sim, _Buf())
        out = C.create_string_buffer(need)
        self._lib.mud_sim_log(self._sim, _Buf(C.cast(out, C.POINTER(C.c_uint8)), need))
        return out.raw.decode("utf-8", "replace")


def load_proto(text_or_path: str):
    """A module of dataclasses and service classes from a .proto, parsed and generated by the library at runtime."""
    import types
    text = open(text_or_path, encoding="utf-8").read() if text_or_path.endswith(".proto") else text_or_path
    lib = load()
    line = C.c_int32(0)
    p, kp = _buf(text.encode())
    out = C.create_string_buffer(1 << 20)
    n = lib.mud_gen(_buf(b"python")[0], p, _Buf(C.cast(out, C.POINTER(C.c_uint8)), 1 << 20), C.byref(line))
    if n == -1: raise ProtoError(line.value)
    if n < 0: raise MudError(n)
    module = types.ModuleType("mud_proto")
    module.__name__ += f"_{id(module):x}"
    sys.modules[module.__name__] = module   # registered as an import registers one, so its annotations resolve
    exec(compile(out.raw[:n].decode(), "<mud gen>", "exec"), module.__dict__)
    return module
