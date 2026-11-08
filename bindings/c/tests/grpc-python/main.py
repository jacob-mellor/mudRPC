"""The gRPC drop-in under test: the stub and servicer grpcio-tools generated from drone.proto, written as grpcio code is,
over two mudRPC peers in this process. Building the channels is the only mudRPC code above them."""
import collections
import socket
import sys
import threading
import time

import grpc

import drone_pb2 as pb
import drone_pb2_grpc as rpc
import mud
import mud_grpc

KEY = bytes(range(1, 33))
HEARD = threading.Event()
STOPPED = threading.Event()


def say(s):
    print(s, flush=True)


def told(f):
    try:
        f()
        return "no error"
    except grpc.RpcError as e:
        return f"{e.code().name} {e.details()}"
    except Exception as e:  # noqa: BLE001
        return type(e).__name__


def fails(what, f):
    say(f"{what}: {told(f)}")


def thrown(f):
    try:
        f()
        return "no error"
    except Exception as e:  # noqa: BLE001
        return type(e).__name__


def read(it, partial=None):
    seen = []
    try:
        for s in it:
            seen.append(str(s.seq))
    except grpc.RpcError:
        if partial:
            say(f"{partial} after: {' '.join(seen)}")
        raise
    return " ".join(seen)


def free():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def udp_pair():
    """Two sockets on loopback that list each other: ports chosen free, chosen again while another test takes one between the choosing and the binding."""
    for i in range(50):
        a, b = free(), free()
        x = None
        try:
            x = mud.Udp(f"127.0.0.1:{a}", f"127.0.0.1:{b}")
            return x, mud.Udp(f"127.0.0.1:{b}", f"127.0.0.1:{a}"), b
        except mud.MudError as e:
            if x:
                x.close()
            if e.code != -2 or i == 49:
                raise


class Link:
    """The radio between the channels: drops nothing unless cut, and can hold the drone's wires to deliver them reversed."""

    def __init__(self):
        self.ends, self.native, self.cut, self.hold, self.held, self.refuse_listen, self.last_ttl = [], None, False, 0, [], False, 0
        self.lock = threading.Lock()

    def send(self, frm, o):
        if o.kind == "listen":
            if self.refuse_listen:
                raise OSError("this radio has no listen")
            return
        self.last_ttl = o.ttl_secs
        if self.cut:
            return
        if self.native:
            self.native.inbox.append((o.topic, o.wire))
            return
        to = self.ends[1 - frm]
        if frm == 1 and self.hold:
            with self.lock:
                self.held.append((o.topic, o.wire))
                if len(self.held) < self.hold:
                    return
                self.hold = 0
                for t, w in reversed(self.held):
                    to.deliver(t, w)
                self.held.clear()
            return
        to.deliver(o.topic, o.wire)


class DroneService(rpc.DroneServicer):
    def Arm(self, r, context):
        if r.id == 0:
            context.abort(grpc.StatusCode.FAILED_PRECONDITION, "battery low")
        if r.id == 13:
            raise ValueError("thirteen")
        if r.id == 21:
            context.set_code(grpc.StatusCode.PERMISSION_DENIED)
            context.set_details("not yours")
            return pb.ArmReply(armed=True)
        if r.id == 22:
            context.set_code(grpc.StatusCode.INTERNAL)
            return pb.ArmReply()
        if r.id == 23:
            return pb.ArmReply(armed=True, by="x" * 1000)
        if r.id == 25:
            context.abort(grpc.StatusCode.FAILED_PRECONDITION, b"battery low")
        if r.id == 26:
            context.abort(grpc.StatusCode.FAILED_PRECONDITION, "\U0001F525" * 128)
        if r.id == 24:
            while context.is_active():
                time.sleep(0.05)
            HEARD.set()
            return pb.ArmReply()
        return pb.ArmReply(armed=True, id=r.id, by=context.peer())

    def Telemetry(self, r, context):
        for i in range(r.count):
            if r.fail_after and i == r.fail_after:
                context.abort(grpc.StatusCode.ABORTED, "lost the camera")
            yield pb.Sample(seq=i)


def big(r, context):
    """A stream whose third item is past the wire: it hears when it is asked for no more."""
    try:
        for i in range(40):
            yield bytes(1000) if i == 2 else pb.Sample(seq=i).SerializeToString()
            time.sleep(0.03)
    except GeneratorExit:
        STOPPED.set()
        raise


class Native:
    """A mudRPC server with no gRPC: Rover's Telemetry sends count items, 2 down to 0, end on the last; Arm refuses."""

    def __init__(self, peer):
        self.peer, self.inbox, self.stop, self.back = peer, collections.deque(), False, None
        peer.listen("test.Rover")
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        start = time.monotonic()
        while not self.stop:
            now = int(time.monotonic() - start)
            while self.inbox:
                t, w = self.inbox.popleft()
                c = self.peer.deliver(now, t, w)
                if c.kind != "call":
                    continue
                if c.method == "Arm":
                    self.peer.reply(now, c, 9, b"", detail="native says no")
                    continue
                n = pb.TelemetryRequest.FromString(c.bytes).count
                for i in range(n):
                    self.peer.reply(now, c, 0, pb.Sample(seq=n - 1 - i).SerializeToString(), end=i == n - 1, seq=i)
            while (o := self.peer.outgoing(now)) is not None:
                if o.kind != "listen" and self.back:
                    self.back.deliver(o.topic, o.wire)
                self.peer.sent()
            time.sleep(0.01)


def main():
    link = Link()
    gcs = mud_grpc.Channel(mud.Peer("gcs", KEY, 1, 1), lambda o: link.send(0, o))
    drone = mud_grpc.Channel(mud.Peer("drone", KEY, 2, 1), lambda o: link.send(1, o))
    link.ends = [gcs, drone]
    rpc.add_DroneServicer_to_server(DroneService(), drone)

    # From here, the code a grpcio product already has.
    client = rpc.DroneStub(gcs)
    r = client.Arm(pb.ArmRequest(id=7))
    say(f"arm: armed {r.armed} id {r.id} by {r.by}")
    reply, call = client.Arm.with_call(pb.ArmRequest(id=8))
    say(f"with_call: armed {reply.armed}, code {call.code().name}")
    say(f"future: armed {client.Arm.future(pb.ArmRequest(id=8)).result().armed}")
    fails("refused", lambda: client.Arm(pb.ArmRequest(id=0)))
    fails("handler throws", lambda: client.Arm(pb.ArmRequest(id=13)))
    fly = gcs.unary_unary("/test.Drone/Fly", request_serializer=pb.ArmRequest.SerializeToString, response_deserializer=pb.ArmReply.FromString)
    fails("unknown method", lambda: fly(pb.ArmRequest(id=1)))
    def named(names):
        return " / ".join(sorted({told(lambda: gcs.unary_unary(n, request_serializer=pb.ArmRequest.SerializeToString, response_deserializer=pb.ArmReply.FromString)(pb.ArmRequest(id=1))) for n in names}))
    say(f"a service name empty, past 64 bytes or not UTF-8, a method name not UTF-8: {named(['//Arm', '/' + 'x' * 65 + '/Arm', '/test.\udcff/Arm', '/test.Drone/\udcff'])}")
    say(f"a method name empty or of 65 bytes, carried: {named(['/test.Drone/', '/test.Drone/' + 'y' * 65])}, then armed {client.Arm(pb.ArmRequest(id=1)).armed}")

    link.hold = 6  # five items and the stream's end, delivered newest first
    say(f"telemetry reversed: {read(client.Telemetry(pb.TelemetryRequest(count=5)))}")
    say(f"telemetry empty: {read(client.Telemetry(pb.TelemetryRequest(count=0)))}")
    fails("telemetry fails", lambda: read(client.Telemetry(pb.TelemetryRequest(count=5, fail_after=2)), "telemetry fails"))
    fails("upload", lambda: client.Upload(iter([pb.Sample(seq=1)])))
    fails("too large", lambda: client.Arm(pb.ArmRequest(id=1, note=bytes(1000))))

    link.cut = True
    t0 = time.monotonic()
    fails("deadline", lambda: client.Arm(pb.ArmRequest(id=9), timeout=2))
    took = time.monotonic() - t0
    say(f"deadline in time: {2.0 <= took < 3.5}")
    f = client.Arm.future(pb.ArmRequest(id=10))
    say(f"future cancelled: {f.cancel()} {f.cancelled()} {thrown(f.result)}")
    fails("past the deadline", lambda: client.Arm(pb.ArmRequest(id=1), timeout=0))
    link.cut = False
    say(f"after the cut: armed {client.Arm(pb.ArmRequest(id=11)).armed}")

    fails("status set", lambda: client.Arm(pb.ArmRequest(id=21)))
    bad = gcs.unary_unary("/test.Drone/Arm", request_serializer=lambda r: b"\xff", response_deserializer=pb.ArmReply.FromString)
    fails("undecodable request", lambda: bad(pb.ArmRequest()))
    fails("a reply past the wire", lambda: client.Arm(pb.ArmRequest(id=23)))
    fails("a code with no details", lambda: client.Arm(pb.ArmRequest(id=22)))

    finished = 0
    for _ in range(30):
        fut = client.Arm.future(pb.ArmRequest(id=16))
        canceller = threading.Thread(target=fut.cancel)
        gcs.use(lambda p: (time.sleep(0.03), canceller.start(), time.sleep(0.02)))  # the reply waits in the inbox while the cancel waits for the pump
        try:
            fut.result(timeout=3)
            finished += 1
        except grpc.FutureCancelledError:
            finished += 1
        except grpc.FutureTimeoutError:
            pass
        canceller.join()
    say(f"cancel racing a reply: {finished} of 30 finished")

    answered = []
    caller = threading.Thread(target=lambda: answered.extend(client.Arm(pb.ArmRequest(id=1)).armed for _ in range(50) if not time.sleep(0.04)))  # a client keeps under 32 calls a second
    caller.start()
    saves = 0
    while caller.is_alive():
        gcs.use(lambda p: p.save())
        saves += 1
        time.sleep(0.001)
    say(f"saved while calling: {sum(answered)} calls answered, saves {'made' if saves else 'none'}")

    say(f"two full streams back to back: {len(read(client.Telemetry(pb.TelemetryRequest(count=63))).split())} {len(read(client.Telemetry(pb.TelemetryRequest(count=63))).split())}")
    fails("a 64th item", lambda: read(client.Telemetry(pb.TelemetryRequest(count=64))))
    drone.add_generic_rpc_handlers([grpc.method_handlers_generic_handler("test.Big", {"Telemetry": grpc.unary_stream_rpc_method_handler(big)})])
    wide = gcs.unary_stream("/test.Big/Telemetry", request_serializer=pb.TelemetryRequest.SerializeToString, response_deserializer=pb.Sample.FromString)
    seen = []
    try:
        for s in wide(pb.TelemetryRequest()):
            seen.append(str(s.seq))
    except grpc.RpcError as e:
        say(f"an item past the wire mid-stream: {' '.join(seen)}, {e.code().name} {e.details()}, its handler stopped: {STOPPED.wait(2)}")

    lone_peer = mud.Peer("lone", KEY, 9, 1)
    lone = mud_grpc.Channel(lone_peer, lambda o: None)
    lone_peer.close()
    time.sleep(0.2)  # the pump's next round finds the peer gone and closes the channel
    fails("the peer closed first", lambda: rpc.DroneStub(lone).Arm(pb.ArmRequest(id=1), timeout=2))

    free_slots = 16 - gcs.use(lambda p: p.pending)  # a call cancelled after sending keeps its slot until its ttl
    futures = [fly.future(pb.ArmRequest(id=1), timeout=5) for _ in range(free_slots)]
    codes = {fu.exception(timeout=10).code() for fu in futures}
    say(f"unknown method, every free slot at once: {'all UNIMPLEMENTED' if codes == {grpc.StatusCode.UNIMPLEMENTED} else codes}")
    say(f"a null wire: {thrown(lambda: gcs.deliver('gcs', None))}")
    link.cut = True
    client.Arm.future(pb.ArmRequest(id=1), timeout=8 * 86400)
    time.sleep(0.2)
    link.cut = False
    say(f"a deadline eight days out: ttl {link.last_ttl}")
    gcs.use(lambda p: [gcs.deliver("gcs", bytes(40)) for _ in range(5000)])
    say(f"five thousand junk wires: then armed {client.Arm(pb.ArmRequest(id=1)).armed}")

    stream = client.Telemetry(pb.TelemetryRequest(count=5))
    time.sleep(0.5)  # every item has landed
    next(stream)
    stream.cancel()
    fails("cancelled with items unread", lambda: next(stream))
    t0 = time.monotonic()
    fails("a slow reader past its deadline", lambda: [time.sleep(0.6) for _ in client.Telemetry(pb.TelemetryRequest(count=5), timeout=1)])
    say(f"the slow reader's deadline in time: {time.monotonic() - t0 < 2.0}")

    link3 = Link()
    link3.refuse_listen = True
    faults = []
    gcs3 = mud_grpc.Channel(mud.Peer("gcs3", KEY, 7, 1), lambda o: link3.send(0, o), fault=faults.append)
    drone3 = mud_grpc.Channel(mud.Peer("drone3", KEY, 8, 1), lambda o: link3.send(1, o))
    link3.ends = [gcs3, drone3]
    rpc.add_DroneServicer_to_server(DroneService(), drone3)
    say(f"a transport refusing a listen: armed {rpc.DroneStub(gcs3).Arm(pb.ArmRequest(id=1), timeout=10).armed}, faults heard {'yes' if faults else 'no'}")

    holder = []
    selfish = mud_grpc.Channel(mud.Peer("selfish", KEY, 10, 1), lambda o: holder[0].close())
    holder.append(selfish)
    stopper = threading.Thread(target=lambda: (time.sleep(0.3), selfish.close()))
    stopper.start()
    stopper.join(3)
    say(f"closed from its own send: {'hangs' if stopper.is_alive() else 'returns'}")

    gu, du, pd = udp_pair()
    gcs_u = mud_grpc.Channel(mud.Peer("gcsu", KEY, 11, 1), gu)
    drone_u = mud_grpc.Channel(mud.Peer("droneu", KEY, 12, 1), du)
    rpc.add_DroneServicer_to_server(DroneService(), drone_u)
    time.sleep(0.2)  # a server answers once its listen has reached its socket
    over_udp = rpc.DroneStub(gcs_u)
    say(f"over UDP: armed {over_udp.Arm(pb.ArmRequest(id=1), timeout=5).armed}, telemetry {read(over_udp.Telemetry(pb.TelemetryRequest(count=5)))}")
    lost_socket = mud.Udp("127.0.0.1:0", f"127.0.0.1:{pd}")
    lost = mud_grpc.Channel(mud.Peer("lost", KEY, 13, 1), lost_socket)
    lost_socket.close()
    time.sleep(0.1)
    fails("its socket closed", lambda: rpc.DroneStub(lost).Arm(pb.ArmRequest(id=1), timeout=2))
    say(f"a public bind: {thrown(lambda: mud.Udp('0.0.0.0:0', '127.0.0.1:9'))}, a public peer: {thrown(lambda: mud.Udp('127.0.0.1:0', '8.8.8.8:53'))}, this library public: {mud.Udp.internet()}")

    fails("a server that never answers", lambda: client.Arm(pb.ArmRequest(id=24), timeout=1))
    say(f"the server saw its ttl pass: {HEARD.wait(5)}")

    fails("a detail in bytes", lambda: client.Arm(pb.ArmRequest(id=25)))
    say(f"then the next caller: armed {client.Arm(pb.ArmRequest(id=1)).armed}")
    try:
        client.Arm(pb.ArmRequest(id=26))
    except grpc.RpcError as e:
        say(f"a detail of 128 emoji: {e.code().name}, {len(e.details())} kept, {len(e.details().encode())} bytes")
    say(f"code() waits for the end: {client.Arm.future(pb.ArmRequest(id=1)).code().name}")
    say(f"an infinite timeout: armed {client.Arm(pb.ArmRequest(id=1), timeout=float('inf')).armed}")
    fails("a timeout of NaN", lambda: client.Arm(pb.ArmRequest(id=1), timeout=float("nan")))
    say(f"a service name past 64 bytes: {thrown(lambda: drone.add_registered_method_handlers('a' * 70, {}))}")

    class Counter(grpc.UnaryUnaryClientInterceptor):
        def __init__(self):
            self.n = 0

        def intercept_unary_unary(self, continuation, details, request):
            self.n += 1
            return continuation(details, request)

    counter = Counter()
    say(f"through an interceptor: armed {rpc.DroneStub(grpc.intercept_channel(gcs, counter)).Arm(pb.ArmRequest(id=1)).armed}, intercepted {counter.n}")

    import gc
    before = len(gcs._open)
    for _ in range(40):
        next(client.Telemetry(pb.TelemetryRequest(count=3)))
        time.sleep(0.04)
    gc.collect()
    say(f"forty streams abandoned unread: let go {len(gcs._open) == before}")

    inner = mud_grpc.Channel(mud.Peer("inner", KEY, 14, 1), lambda o: None)
    t = threading.Thread(target=lambda: inner.use(lambda p: inner.close()))
    t.start()
    t.join(3)
    say(f"closed from inside use(): {'hangs' if t.is_alive() else 'returns'}")
    heard = []
    inner.subscribe(heard.append)
    time.sleep(0.1)
    say(f"subscribed after close: {heard[0].name if heard else 'nothing'}")

    third_ports = [free(), free(), free()]
    for _ in range(50):
        try:
            g, d, i = (mud.Udp(f"127.0.0.1:{p}", *[f"127.0.0.1:{q}" for q in third_ports if q != p]) for p in third_ports)
            break
        except mud.MudError:
            third_ports = [free(), free(), free()]
    gcs_t, drone_t, idle_t = mud_grpc.Channel(mud.Peer("gcst", KEY, 21, 1), g), mud_grpc.Channel(mud.Peer("dronet", KEY, 22, 1), d), mud_grpc.Channel(mud.Peer("idlet", KEY, 23, 1), i)
    rpc.add_DroneServicer_to_server(DroneService(), drone_t)
    time.sleep(0.2)  # a server answers once its listen has reached its socket
    three = rpc.DroneStub(gcs_t)
    say(f"three on UDP, one serving nothing: {sum(three.Arm(pb.ArmRequest(id=1), timeout=5).armed for _ in range(10))} of 10 armed")
    for c in (gcs_t, drone_t, idle_t):
        c.close()

    link4 = Link()
    gcs4 = mud_grpc.Channel(mud.Peer("gcs4", KEY, 15, 1), lambda o: link4.send(0, o))
    drone4 = mud_grpc.Channel(mud.Peer("drone4", KEY, 16, 1), lambda o: link4.send(1, o), max_workers=1)
    link4.ends = [gcs4, drone4]
    rpc.add_DroneServicer_to_server(DroneService(), drone4)
    stub4 = rpc.DroneStub(gcs4)
    HEARD.clear()
    running, queued = (stub4.Arm.future(pb.ArmRequest(id=24), timeout=30) for _ in range(2))
    time.sleep(0.3)
    stopped = drone4.stop(0.2)
    say(f"a stop past its grace: running {told(running.result)}, waiting {told(queued.result)}, the servicer heard it: {HEARD.wait(5)}, stopped {stopped.wait(5)}")
    fails("a call after the stop", lambda: stub4.Arm(pb.ArmRequest(id=1), timeout=1))

    # A native peer answers the Rover service with the plain API: a stream ends on its last item, and a refusal has a detail.
    native = Native(mud.Peer("rover", KEY, 3, 1))
    link2 = Link()
    gcs2 = mud_grpc.Channel(mud.Peer("gcs2", KEY, 4, 1), lambda o: link2.send(0, o))
    link2.ends, link2.native, native.back = [gcs2], native, gcs2
    rover = rpc.RoverStub(gcs2)
    say(f"native stream: {read(rover.Telemetry(pb.TelemetryRequest(count=3)))}")
    fails("native refusal", lambda: rover.Arm(pb.ArmRequest(id=1)))

    crowd = [mud.Peer(f"crowd{i}", KEY, 100 + i, 1) for i in range(48)]  # 48 callers owed a stream each, past the 32 a second: the next caller waits its turn, not behind them all
    for p in crowd:
        p.call(0, "test.Drone", "Telemetry", pb.TelemetryRequest(count=20).SerializeToString(), 5)
        while (o := p.outgoing(0)) is not None:
            if o.kind != "listen":
                drone.deliver(o.topic, o.wire)
            p.sent()
    time.sleep(0.3)
    try:
        armed = client.Arm(pb.ArmRequest(id=1), timeout=3).armed
    except grpc.RpcError:
        armed = False
    say(f"a caller behind 48 owed a stream each: armed {armed}")
    native.stop = True
    for c in (gcs, drone, gcs2, gcs3, drone3, gcs4, drone4, gcs_u, drone_u):
        c.close()
    sys.stdout.flush()


if __name__ == "__main__":
    main()
