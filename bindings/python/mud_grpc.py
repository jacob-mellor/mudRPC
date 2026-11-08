"""mudRPC as gRPC's transport in Python: the stub and the servicer grpcio-tools generates from a .proto run over mudRPC
unchanged; building the channel is the one line that differs. Beside mud.py, with grpcio, whose Channel, RpcError,
Call, Future and ServicerContext this implements (grpcio, Copyright The gRPC Authors, Apache License 2.0).

Not a drop-in where gRPC needs a connection: a message of about 460 bytes, a service's full name of at most 64 bytes, no
metadata, unary and server-streaming calls only, at most 63 items a stream, the first reply wins, and 16 calls in
flight. A server sends 32 replies a second, half what a receiver takes from one sender, its callers in turn, and a
client keeps under 32 calls a second. A timeout is the call's ttl, a week at most; a call cancelled after it was sent
keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
"""
from __future__ import annotations

import collections
import concurrent.futures
import heapq
import itertools
import math
import threading
import time
import weakref
from typing import Any, Callable, Optional

import grpc

import mud

STREAM_END = "end"  # the detail of the reply that ends a server stream and carries no item
WEEK, MAX_ITEMS, INBOX, PACE, PATIENCE, DETAIL = 604800, 63, 1024, 32, 1.0, 128  # a message's longest life, a stream's items, wires or calls waiting, replies a second, seconds a refused item is tried, a detail's bytes
_CODES = {c.value[0]: c for c in grpc.StatusCode}


def _status(code: int) -> grpc.StatusCode:
    return _CODES.get(code, grpc.StatusCode.UNKNOWN)


def _clip(detail) -> str:
    """A detail as text, at most 128 bytes of UTF-8, cut on a character: grpcio lets a detail be bytes."""
    if isinstance(detail, (bytes, bytearray)):
        detail = bytes(detail).decode("utf-8", "replace")
    return str(detail or "").encode()[:DETAIL].decode("utf-8", "ignore")


def _named(s: str, bounded: bool = True) -> bool:
    """UTF-8, and 1 to 64 bytes if bounded, as a service's full name is; core carries a method's name of any length."""
    try:
        n = len(s.encode())
    except UnicodeEncodeError:
        return False
    return not bounded or 0 < n <= 64


def _refused(e: mud.MudError) -> tuple:
    code = grpc.StatusCode.RESOURCE_EXHAUSTED if e.code in (-2, -3, -9) else grpc.StatusCode.INVALID_ARGUMENT if e.code == -1 else grpc.StatusCode.INTERNAL
    return code, str(e)


class Channel(grpc.Channel):
    """A gRPC channel over one peer, and the server for the servicers added to it: it pumps on its own thread.

    The peer is the channel's while it lives: reach it only through use(), and close the channel before the peer.
    `transport` is a mud.Udp, which the pump sends through and reads in its own round, or a callable taking each outgoing
    item, with what arrives handed to deliver(). `ttl_secs` is the life of a call made without a timeout; `fault`, run on
    the pump, hears every exception the transport raises, and the item it raised on is tried for a second, then let go.
    Servicers run on a pool of `max_workers` threads, as grpc.server's executor; the channel's own callbacks on another.
    """

    def __init__(self, peer: mud.Peer, transport, ttl_secs: int = 60, fault: Optional[Callable[[BaseException], None]] = None, max_workers: int = 16):
        self._peer = peer
        self._udp = transport if isinstance(transport, mud.Udp) else None
        self._send = transport.send if self._udp else transport
        self._fault = fault or (lambda e: None)
        self._ttl = max(1, min(int(ttl_secs), WEEK))
        self._gate = threading.RLock()  # one thread drives the peer at a time; no servicer, callback or serializer runs while it is held, the transport's send alone does
        self._using: Optional[int] = None  # the thread inside use(), which a close must not wait on
        self._start = time.monotonic()
        self._inbox: collections.deque = collections.deque()
        self._pending: weakref.WeakValueDictionary = weakref.WeakValueDictionary()  # a call the caller let go is let go here too
        self._open: weakref.WeakSet = weakref.WeakSet()
        self._kept: set = set()  # calls with done callbacks, held until they have run
        self._deadlines: list = []  # (when, n, weak call), a heap the pump reads each round: no thread per call
        self._order = itertools.count()
        self._registered: dict = {}
        self._generic: list = []
        self._unnamed = False  # a generic handler with no service name: every call's topic may be its
        self._services: set = set()
        self._to_listen: list = []
        self._answers: collections.OrderedDict = collections.OrderedDict()  # by caller, the next to send first
        self._serving: set = set()  # the calls waiting for a servicer or running, which a stop cancels
        self._stop_at, self._stopped = math.inf, threading.Event()
        self._later: list = []  # callbacks found under the gate, run after it
        self._waiting = 0  # calls waiting for a servicer
        self._pool = concurrent.futures.ThreadPoolExecutor(max_workers=max_workers, thread_name_prefix="mudrpc-call")
        self._tellers = concurrent.futures.ThreadPoolExecutor(max_workers=2, thread_name_prefix="mudrpc-callback")
        self._wake = threading.Event()
        self._closed = False
        self._pace_sec, self._paced, self._refused_since = -1, 0, 0.0
        self._pump = threading.Thread(target=self._run, name="mudrpc", daemon=True)
        self._pump.start()

    # The transport and the peer.

    def deliver(self, topic: str, wire: bytes) -> None:
        """A wire from the transport, from any thread. Past 1024 waiting for the pump, a wire is dropped, as core would drop a flood."""
        if topic is None or wire is None:
            raise TypeError("a wire has a topic and bytes")
        if self._closed or len(self._inbox) >= INBOX:
            return
        self._inbox.append((topic, bytes(wire)))
        self._wake.set()

    def use(self, act: Callable[[mud.Peer], Any]) -> Any:
        """The peer, between rounds of the pump: save, stage, rotate, roster, counts."""
        with self._gate:
            outer, self._using = self._using, threading.get_ident()
            try:
                return act(self._peer)
            finally:
                self._using = outer
                self._wake.set()

    def _now(self) -> int:
        return int(time.monotonic() - self._start)

    def _soon(self, f, *args) -> None:
        """A callback, on the callback pool, or here once the close has shut it."""
        try:
            self._tellers.submit(f, *args)
        except RuntimeError:
            f(*args)

    def _hand(self) -> None:
        with self._gate:
            later, self._later = self._later, []
        for f in later:
            self._soon(f)

    def _run(self) -> None:
        while True:
            wait = 0.01 if self._udp else 0.05
            try:
                wait = min(wait, max(0.0, self._deadlines[0][0] - time.monotonic()))
            except IndexError:
                pass
            self._wake.wait(wait)
            self._wake.clear()
            calls: list = []
            stop = self._round(calls)
            self._hand()
            if stop:
                return
            try:
                for c in calls:
                    self._pool.submit(c)
            except RuntimeError:  # the pool shut by a close between the round and here
                return

    def _round(self, calls: list) -> bool:
        with self._gate:
            if self._closed:
                return True
            now, t = self._now(), time.monotonic()
            while self._deadlines and self._deadlines[0][0] <= t:
                c = heapq.heappop(self._deadlines)[2]()
                if c is not None:
                    c._abort(grpc.StatusCode.DEADLINE_EXCEEDED, "Deadline Exceeded")
            if t >= self._stop_at:
                for ctx in list(self._serving):
                    self._owe(ctx, grpc.StatusCode.CANCELLED.value[0], "the server shut down", b"", True)
                    ctx._killed = True
            if len(self._deadlines) > 64 and len(self._deadlines) > 2 * len(self._open):  # ended calls let go, not held until their deadline
                self._deadlines = [d for d in self._deadlines if d[2]() is not None and not d[2]()._over]
                heapq.heapify(self._deadlines)
            try:
                for name in list(self._to_listen):
                    self._peer.listen(name)
                    self._to_listen.remove(name)
            except mud.MudError:
                pass  # the outbox is full: the next round listens
            try:
                while self._inbox:
                    topic, wire = self._inbox.popleft()
                    try:
                        e = self._peer.deliver(now, topic, wire)
                    except mud.MudError:
                        continue  # a wire the transport mangled: no verdict to act on
                    self._land(e, calls)
                if self._udp:
                    try:
                        for _ in range(INBOX):
                            w = self._udp.poll()
                            if w is None:
                                break
                            try:
                                self._land(self._peer.deliver(now, w[0], w[1]), calls)
                            except mud.MudError:
                                pass
                    except RuntimeError:
                        self._shut(grpc.StatusCode.UNAVAILABLE, "the socket is closed")
                        return True
                    except mud.MudError as e:
                        self._tell(e)
                while (e := self._peer.tick(now)) is not None:
                    self._land(e, calls)
                self._answer(now)
                while (o := self._peer.outgoing(now)) is not None:
                    try:
                        self._send(o)
                    except Exception as x:  # noqa: BLE001 - the transport's, told to fault
                        if self._udp and isinstance(x, RuntimeError):
                            self._shut(grpc.StatusCode.UNAVAILABLE, "the socket is closed")
                            return True
                        self._tell(x)
                        t = time.monotonic()
                        self._refused_since = self._refused_since or t
                        if t - self._refused_since < PATIENCE:
                            break  # the next round tries it again; past a second it is let go
                    self._refused_since = 0.0
                    self._peer.sent()
            except mud.MudError as e:
                if e.code in (-1, -4, -5):
                    self._shut(grpc.StatusCode.UNAVAILABLE, f"the peer is closed: {e}")
                    return True
            except Exception as e:  # noqa: BLE001 - nothing the pump meets ends it but a closed channel
                self._tell(e)
            return False

    def _tell(self, e: BaseException) -> None:
        try:
            self._fault(e)
        except Exception:  # noqa: BLE001 - the fault callback's own
            pass

    def _land(self, e: mud.Incoming, calls: list) -> None:
        if e.kind in ("reply", "expired"):
            c = self._pending.get(e.id)
            if c is not None:
                c._land(e)
        elif e.kind == "call" and self._stop_at == math.inf and (e.topic in self._services or self._unnamed):  # a call on a service this channel does not serve, or after its stop, is not its to answer
            ctx = _Context(self, e, time.monotonic() + e.ttl_secs)  # its ttl counts from its arrival
            if self._waiting >= INBOX:
                self._owe(ctx, grpc.StatusCode.RESOURCE_EXHAUSTED.value[0], "the server is behind", b"", True)
                return
            self._waiting += 1
            self._serving.add(ctx)
            calls.append(lambda: self._serve(ctx))

    # Server replies, paced.

    def _owe(self, ctx: "_Context", status: int, detail, payload: bytes, end: bool) -> None:
        """Under the gate: the call's next reply. One caller is owed 1024 at most, a flood's calls left to their ttl, and
        all of them 4096, past which the longest line gives way."""
        if self._closed or ctx._killed:
            return
        seq, ctx._seq, ctx._killed = ctx._seq, ctx._seq + 1, end  # its end owed, nothing more is: a stop answers it once
        line = self._answers.setdefault(ctx._call.sender, collections.deque())
        if len(line) >= INBOX:
            return
        if sum(map(len, self._answers.values())) >= 4 * INBOX:
            longest = max(self._answers, key=lambda s: len(self._answers[s]))
            if longest == ctx._call.sender:
                return
            self._answers[longest].pop()
        line.append((ctx, seq, status, _clip(detail), payload, end))
        self._wake.set()

    def _answer(self, now: int) -> None:
        """Under the gate: the server's replies, 32 a second, a turn each: a caller sends one and goes to the back of the
        line; a full outbox leaves the rest for the next round, each until its call's ttl."""
        if now != self._pace_sec:
            self._pace_sec, self._paced = now, 0
        t = time.monotonic()
        while self._paced < PACE and self._answers:
            sender, line = next(iter(self._answers.items()))
            if line:
                a = line.popleft()
                ctx, seq, status, detail, payload, end = a
                try:
                    if t <= ctx._until:
                        self._peer.reply(now, ctx._call, status, payload, end, seq, detail)
                        self._paced += 1
                except mud.MudError as x:
                    if x.code in (-2, -9):
                        line.appendleft(a)
                        return
                    if x.code == -3 and payload:
                        ctx._killed = True  # the stream is over: its servicer is asked for no more, and the caller hears why at once
                        line = self._answers[sender] = collections.deque(b for b in line if b[0] is not ctx)
                        line.appendleft((ctx, seq, grpc.StatusCode.RESOURCE_EXHAUSTED.value[0], "the reply is larger than a mudRPC wire", b"", True))
                except Exception as x:  # noqa: BLE001 - one bad answer is dropped, never the line
                    self._tell(x)
            if line:
                self._answers.move_to_end(sender)
            else:
                del self._answers[sender]

    # Closing.

    def close(self) -> None:
        """Stops the pump and ends every call in flight with UNAVAILABLE; from any thread, a callback, use() or the transport's send included."""
        with self._gate:
            self._shut(grpc.StatusCode.UNAVAILABLE, "the channel is closed")
        self._hand()
        self._wake.set()
        if threading.current_thread() is not self._pump and self._using != threading.get_ident():
            self._pump.join()
        self._pool.shutdown(wait=False)
        self._tellers.shutdown(wait=False)  # a later call's callbacks run where they are found

    def _shut(self, code: grpc.StatusCode, details: str) -> None:
        if self._closed:
            return
        self._closed = True
        for c in list(self._open):
            c._abort(code, details)
        self._answers.clear()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def subscribe(self, callback, try_to_connect=False):
        self._soon(callback, grpc.ChannelConnectivity.READY)  # no connection to watch: always ready to queue a call

    def unsubscribe(self, callback):
        pass

    # Client side.

    def unary_unary(self, method, request_serializer=None, response_deserializer=None, _registered_method=False):
        return _Multicallable(self, method, request_serializer, response_deserializer, stream=False)

    def unary_stream(self, method, request_serializer=None, response_deserializer=None, _registered_method=False):
        return _Multicallable(self, method, request_serializer, response_deserializer, stream=True)

    def stream_unary(self, method, request_serializer=None, response_deserializer=None, _registered_method=False):
        return _Refused()

    def stream_stream(self, method, request_serializer=None, response_deserializer=None, _registered_method=False):
        return _Refused()

    # Server side, as grpc.Server's.

    def add_generic_rpc_handlers(self, handlers) -> None:
        with self._gate:
            for h in handlers:
                self._generic.append(h)
                name = getattr(h, "_name", None)  # grpc.method_handlers_generic_handler's service name
                if isinstance(name, str):
                    self._listen(name)
                else:
                    self._unnamed = True
        self._wake.set()

    def add_registered_method_handlers(self, service_name: str, method_handlers: dict) -> None:
        with self._gate:
            self._listen(service_name)
            for name, h in method_handlers.items():
                self._registered[f"/{service_name}/{name}"] = h
        self._wake.set()

    def _listen(self, service: str) -> None:
        if not _named(service):
            raise ValueError(f"mudRPC carries a service's full name of 1 to 64 bytes of UTF-8: {service!r}")
        if service not in self._services:
            self._services.add(service)
            self._to_listen.append(service)

    def _handler(self, path: str):
        h = self._registered.get(path)
        if h is not None:
            return h
        details = _CallDetails(path)
        for g in list(self._generic):
            h = g.service(details)
            if h is not None:
                return h
        return None

    def stop(self, grace: Optional[float] = None) -> threading.Event:
        """grpc.Server's: no new calls from now; past `grace` seconds, at once for None, those running or waiting are
        answered CANCELLED, their servicers' is_active() false. The event is set once every servicer has returned."""
        with self._gate:
            self._stop_at = min(self._stop_at, time.monotonic() + (grace or 0))
            if not self._serving:
                self._stopped.set()
        self._wake.set()
        return self._stopped

    def _serve(self, ctx: "_Context") -> None:
        with self._gate:
            self._waiting -= 1
        if not ctx._killed and time.monotonic() < ctx._until:  # one its caller has given up on, or a stop cancelled, is shed unrun
            self._servicer(ctx)
        with self._gate:
            self._serving.discard(ctx)
            if self._stop_at < math.inf and not self._serving:
                self._stopped.set()

    def _servicer(self, ctx: "_Context") -> None:
        call = ctx._call
        try:
            h = self._handler(f"/{call.topic}/{call.method}")
        except Exception:  # noqa: BLE001 - a generic handler's own
            h = None
        if h is None:
            if call.topic in self._services:
                with self._gate:
                    self._owe(ctx, grpc.StatusCode.UNIMPLEMENTED.value[0], f"no method {call.topic}/{call.method}", b"", True)
            return
        status, detail, payload = 0, "", b""
        if h.request_streaming:
            status, detail = grpc.StatusCode.UNIMPLEMENTED.value[0], "mudRPC carries no client stream"
        else:
            try:
                req = h.request_deserializer(call.bytes) if h.request_deserializer else call.bytes
            except Exception:  # noqa: BLE001 - grpcio's answer to a request it cannot read
                req, status, detail = None, grpc.StatusCode.INTERNAL.value[0], "Exception deserializing request!"
            if status == 0:
                try:
                    if h.response_streaming:
                        for item in h.unary_stream(req, ctx):
                            if ctx._killed:
                                break  # the call is over: as grpcio's server, it asks the servicer for no more
                            if ctx._seq >= MAX_ITEMS:
                                raise _Abort(grpc.StatusCode.RESOURCE_EXHAUSTED, f"a stream carries at most {MAX_ITEMS} items")
                            b = h.response_serializer(item) if h.response_serializer else item
                            with self._gate:
                                self._owe(ctx, 0, "", b, False)
                        status, detail = (ctx._code.value[0], ctx._details) if ctx._code != grpc.StatusCode.OK else (0, STREAM_END)
                    else:
                        resp = h.unary_unary(req, ctx)
                        if ctx._code != grpc.StatusCode.OK:
                            status, detail = ctx._code.value[0], ctx._details
                        elif resp is None:
                            status, detail = grpc.StatusCode.INTERNAL.value[0], "Exception serializing response!"
                        else:
                            payload = h.response_serializer(resp) if h.response_serializer else resp
                except _Abort as a:
                    status, detail = a.code.value[0], a.details
                except Exception as e:  # noqa: BLE001 - grpcio's answer to a servicer's exception
                    status, detail = grpc.StatusCode.UNKNOWN.value[0], (f"Exception iterating responses: {e}" if h.response_streaming else f"Exception calling application: {e}")
        with self._gate:
            self._owe(ctx, status, detail, payload, True)
        ctx._done()


class _CallDetails(grpc.HandlerCallDetails):
    def __init__(self, method: str):
        self.method, self.invocation_metadata = method, ()


class _Abort(Exception):
    def __init__(self, code: grpc.StatusCode, details):
        super().__init__(details)
        self.code, self.details = code, details


class _Context(grpc.ServicerContext):
    """What a servicer can know of its call: the caller's roster number as its peer, and the time its ttl leaves, counted from its arrival."""

    def __init__(self, channel: Channel, call: mud.Incoming, until: float):
        self._channel, self._call, self._until = channel, call, until
        self._code, self._details, self._callbacks, self._over = grpc.StatusCode.OK, None, [], False
        self._seq, self._killed = 0, False  # its next reply's seq; killed: its end owed, by its servicer, a stop or a reply past the wire

    def peer(self) -> str: return f"mudrpc:{self._call.sender}"
    def peer_identities(self): return None
    def peer_identity_key(self): return None
    def auth_context(self): return {}
    def invocation_metadata(self): return ()
    def time_remaining(self) -> float: return max(0.0, self._until - time.monotonic())
    def is_active(self) -> bool: return not self._over and not self._killed and not self._channel._closed and time.monotonic() < self._until
    def cancel(self) -> None: self._over = True
    def add_callback(self, callback) -> bool: self._callbacks.append(callback); return True
    def send_initial_metadata(self, initial_metadata) -> None: pass
    def set_trailing_metadata(self, trailing_metadata) -> None: pass
    def trailing_metadata(self): return ()
    def set_compression(self, compression) -> None: pass
    def disable_next_message_compression(self) -> None: pass
    def set_code(self, code) -> None: self._code = code
    def set_details(self, details) -> None: self._details = details
    def code(self): return self._code
    def details(self): return self._details

    def abort(self, code, details):
        if code == grpc.StatusCode.OK:
            raise ValueError("abort with OK")
        raise _Abort(code, details)

    def abort_with_status(self, status):
        self.abort(status.code, status.details)

    def _done(self) -> None:
        self._over = True
        for cb in self._callbacks:
            try:
                cb()
            except Exception:  # noqa: BLE001 - a callback's own
                pass


class _Refused(grpc.UnaryUnaryMultiCallable):
    """A client or bidirectional stream: mudRPC carries neither."""

    def __call__(self, *args, **kwargs):
        raise _Failed(grpc.StatusCode.UNIMPLEMENTED, "mudRPC carries no client stream")

    def with_call(self, *args, **kwargs):
        self()

    def future(self, *args, **kwargs):
        return _Failed(grpc.StatusCode.UNIMPLEMENTED, "mudRPC carries no client stream")


class _Failed(grpc.RpcError, grpc.Call, grpc.Future):
    """A call that ended before anything was sent."""

    def __init__(self, code, details):
        super().__init__(details)
        self._code, self._details = code, details

    def code(self): return self._code
    def details(self): return self._details
    def initial_metadata(self): return ()
    def trailing_metadata(self): return ()
    def is_active(self): return False
    def time_remaining(self): return None
    def cancel(self): return False
    def cancelled(self): return False
    def running(self): return False
    def done(self): return True
    def add_callback(self, callback): return False
    def result(self, timeout=None): raise self
    def exception(self, timeout=None): return self
    def traceback(self, timeout=None): return None
    def add_done_callback(self, fn): fn(self)
    def __iter__(self): return self
    def __next__(self): raise self
    def debug_error_string(self): return self._details


class _Multicallable(grpc.UnaryUnaryMultiCallable, grpc.UnaryStreamMultiCallable):
    def __init__(self, channel: Channel, method: str, serializer, deserializer, stream: bool):
        self._channel, self._serialize, self._deserialize, self._stream = channel, serializer, deserializer, stream
        _, service, name = method.split("/", 2)
        self._service, self._name = service, name

    def _begin(self, request, timeout) -> "_Call":
        c = _Call(self._channel, self._deserialize, self._stream)
        if not (_named(self._service) and _named(self._name, False)):
            c._end_before(*_refused(mud.MudError(-1)))  # as the peer refuses an empty name
            return c
        try:
            payload = self._serialize(request) if self._serialize else request
        except Exception:  # noqa: BLE001 - grpcio's answer to a request it cannot write
            c._end_before(grpc.StatusCode.INTERNAL, "Exception serializing request!")
            return c
        c._begin(self._service, self._name, payload, timeout)
        return c

    def __call__(self, request, timeout=None, metadata=None, credentials=None, wait_for_ready=None, compression=None):
        c = self._begin(request, timeout)
        return c if self._stream else c.result()

    def with_call(self, request, timeout=None, metadata=None, credentials=None, wait_for_ready=None, compression=None):
        c = self._begin(request, timeout)
        return c.result(), c

    def future(self, request, timeout=None, metadata=None, credentials=None, wait_for_ready=None, compression=None):
        return self._begin(request, timeout)


class _Call(grpc.RpcError, grpc.Call, grpc.Future):
    """One call: grpcio's raised error, its Call and its Future at once, as grpcio's own call object is; a stream's is
    also its iterator. Replies land in any order and are taken in seq order; the server's status comes after the items
    before it. A local end, a cancel, a timeout or a close, ends it whatever the replies say, unread items cut off.
    Replies are decoded on the caller's thread, never the pump's."""

    def __init__(self, channel: Channel, deserialize, stream: bool):
        super().__init__()
        self._ch, self._deserialize, self._stream = channel, deserialize, stream
        self._cond = threading.Condition(channel._gate)
        self._early: dict = {}
        self._next, self._exchange_done, self._over, self._cancelled = 0, False, False, False
        self._items: collections.deque = collections.deque()  # raw bytes, decoded by the reader
        self._code: Optional[grpc.StatusCode] = None
        self._details = ""
        self._response: Optional[bytes] = None
        self._key: Optional[bytes] = None
        self._deadline: Optional[float] = None
        self._callbacks: list = []

    # Starting.

    def _end_before(self, code, details) -> None:
        with self._ch._gate:
            self._close(code, details)
        self._ch._hand()

    def _begin(self, service: str, name: str, payload: bytes, timeout: Optional[float]) -> None:
        ch = self._ch
        if timeout is not None and math.isinf(timeout) and timeout > 0:
            timeout = None  # grpcio's: no deadline
        with ch._gate:
            if timeout is not None and not timeout > 0:  # 0, negative or NaN
                self._close(grpc.StatusCode.DEADLINE_EXCEEDED, "Deadline Exceeded")
            elif ch._closed:
                self._close(grpc.StatusCode.UNAVAILABLE, "the channel is closed")
            else:
                life = ch._ttl if timeout is None else min(math.ceil(timeout) + 1, WEEK)  # a second over: the clock is whole seconds, the pump's check below is the deadline
                try:
                    self._key = ch._peer.call(ch._now(), service, name, payload, life)
                except mud.MudError as e:
                    self._close(*_refused(e))
                else:
                    ch._pending[self._key] = self
                    ch._open.add(self)
                    if timeout is not None and timeout <= WEEK:
                        self._deadline = time.monotonic() + timeout
                        heapq.heappush(ch._deadlines, (self._deadline, next(ch._order), weakref.ref(self)))  # the pump ends it then, a round at most after
        ch._hand()
        ch._wake.set()

    # Under the gate.

    def _land(self, e: mud.Incoming) -> None:
        if e.kind == "expired":
            self._abort(grpc.StatusCode.DEADLINE_EXCEEDED, "Deadline Exceeded")
            return
        if self._exchange_done or e.seq < self._next:
            return
        self._early.setdefault(e.seq, e)
        while not self._exchange_done and self._next in self._early:
            r = self._early.pop(self._next)
            self._next += 1
            if r.status != 0:
                self._finish_exchange(_status(r.status), r.detail)
            elif r.end and r.detail == STREAM_END:
                self._finish_exchange(grpc.StatusCode.OK, "")
            else:
                if self._stream:
                    self._items.append(r.bytes)
                elif self._response is None:
                    self._response = r.bytes
                if r.end:
                    self._finish_exchange(grpc.StatusCode.OK, "")
        self._cond.notify_all()

    def _finish_exchange(self, code, details) -> None:
        """The server's status: it comes after the items before it, and the call ends when they are read."""
        self._exchange_done = True
        self._early.clear()
        if self._key is not None:
            self._ch._pending.pop(self._key, None)
        if not self._stream or not self._items:
            self._close(code, details)
        else:
            self._code, self._details = code, details  # told when the reader reaches it

    def _abort(self, code, details) -> None:
        """A local end: whatever the exchange, the call ends now, unread items cut off."""
        if self._over:
            return
        self._items.clear()
        self._exchange_done = True
        if self._key is not None:
            self._ch._pending.pop(self._key, None)
        self._close(code, details)

    def _close(self, code, details) -> None:
        if self._over:
            return
        self._over = True
        self._code, self._details = code, details
        if code == grpc.StatusCode.OK and not self._stream and self._response is None:
            self._code, self._details = grpc.StatusCode.INTERNAL, "no response"
        self._ch._open.discard(self)
        self._ch._kept.discard(self)
        self._ch._later.extend(self._callbacks)  # each runs on the callback pool, after the gate
        self._cond.notify_all()

    # grpc.Future and grpc.Call.

    def cancel(self) -> bool:
        with self._ch._gate:
            if self._over and (not self._stream or not self._items):
                return False
            self._cancelled = True
            self._abort(grpc.StatusCode.CANCELLED, "Locally cancelled by application!")
        self._ch._hand()
        return True

    def cancelled(self) -> bool:
        return self._cancelled

    def running(self) -> bool:
        return not self._over

    def done(self) -> bool:
        return self._over

    def _wait(self, timeout) -> None:
        with self._cond:
            if not self._cond.wait_for(lambda: self._over, timeout):
                raise grpc.FutureTimeoutError()

    def result(self, timeout=None):
        self._wait(timeout)
        if self._cancelled:
            raise grpc.FutureCancelledError()
        if self._code != grpc.StatusCode.OK:
            raise self
        try:
            return self._deserialize(self._response) if self._deserialize else self._response
        except Exception:  # noqa: BLE001 - grpcio's answer to a reply it cannot read
            self._code, self._details = grpc.StatusCode.INTERNAL, "Exception deserializing response!"
            raise self from None

    def exception(self, timeout=None):
        self._wait(timeout)
        if self._cancelled:
            raise grpc.FutureCancelledError()
        return None if self._code == grpc.StatusCode.OK else self

    def traceback(self, timeout=None):
        e = self.exception(timeout)
        return e.__traceback__ if e else None

    def add_done_callback(self, fn) -> None:
        def call():
            fn(self)
        with self._ch._gate:
            if not self._over:
                self._callbacks.append(call)
                self._ch._kept.add(self)  # a call with a callback is held until it has run, whoever else holds it
                return
        fn(self)

    def add_callback(self, callback) -> bool:
        with self._ch._gate:
            if self._over:
                return False
            self._callbacks.append(callback)
            self._ch._kept.add(self)
            return True

    def is_active(self) -> bool:
        return not self._over

    def time_remaining(self):
        return None if self._deadline is None else max(0.0, self._deadline - time.monotonic())

    def _status_known(self) -> None:
        """grpcio's code() and details() wait for the server's status, or the call's end."""
        with self._cond:
            self._cond.wait_for(lambda: self._over or self._exchange_done)

    def code(self):
        self._status_known()
        return self._code

    def details(self):
        self._status_known()
        return self._details

    def initial_metadata(self):
        return ()

    def trailing_metadata(self):
        return ()

    def debug_error_string(self):
        return f"{self._code}: {self._details}"

    def __str__(self):
        return f"<mudRPC call that terminated with: status = {self._code}, details = {self._details!r}>"

    __repr__ = __str__

    # A stream's iterator.

    def __iter__(self):
        return self

    def __next__(self):
        if not self._stream:
            raise TypeError("a unary call is not iterable")
        ch = self._ch
        with self._cond:
            self._cond.wait_for(lambda: self._items or self._over)
            raw = None
            if self._items:
                raw = self._items.popleft()
                if not self._items and self._exchange_done:
                    self._close(self._code or grpc.StatusCode.OK, self._details)
        ch._hand()
        if raw is not None:
            try:
                return self._deserialize(raw) if self._deserialize else raw
            except Exception:  # noqa: BLE001 - grpcio's answer to a reply it cannot read
                with ch._gate:
                    self._abort(grpc.StatusCode.INTERNAL, "Exception deserializing response!")
                ch._hand()
                raise self from None
        if self._code == grpc.StatusCode.OK:
            raise StopIteration
        raise self

    def next(self):
        return self.__next__()
