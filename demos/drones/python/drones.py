"""gRPC drone telemetry in Python: a ground station and a mock drone, two peers over UDP in this process, the stub and
servicer grpcio-tools generated from drone.proto run over mudRPC's drop-in channel."""
import socket
import time

import grpc

import drone_pb2 as pb
import drone_pb2_grpc as rpc
import mud
import mud_grpc

KEY = bytes(range(1, 33))  # the fleet key: a product provisions 32 random bytes to every peer
EPOCH = 1


def free():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class MockDrone(rpc.DroneServicer):
    armed = False

    def Arm(self, request, context):
        self.armed = True
        return pb.ArmReply(armed=True, drone="drone-1")

    def Telemetry(self, request, context):
        for i in range(request.samples):
            yield pb.Sample(seq=i, altitude_m=10 * (i + 1), battery_pct=100 - 2 * i)

    def Land(self, request, context):
        if not self.armed:
            context.abort(grpc.StatusCode.FAILED_PRECONDITION, "not armed")
        self.armed = False
        return pb.LandReply(landed=True)


def main():
    a, b = free(), free()
    peers = [mud.Peer("gcs", KEY, 1, EPOCH), mud.Peer("drone", KEY, 2, EPOCH)]
    links = [mud.Udp(f"127.0.0.1:{a}", f"127.0.0.1:{b}"), mud.Udp(f"127.0.0.1:{b}", f"127.0.0.1:{a}")]
    gcs, drone = (mud_grpc.Channel(p, u) for p, u in zip(peers, links))
    rpc.add_DroneServicer_to_server(MockDrone(), drone)
    time.sleep(0.2)  # the drone answers once its listen has reached its socket

    station = rpc.DroneStub(gcs)
    try:
        station.Land(pb.LandRequest(), timeout=10)
    except grpc.RpcError as e:
        print(f"land: {e.code().name} {e.details()}")
    arm = pb.ArmRequest(pilot="gcs")
    print(f"armed {station.Arm(arm, timeout=10).drone} for {arm.pilot}")
    samples = station.Telemetry(pb.TelemetryRequest(samples=3), timeout=10)
    print("telemetry " + " ".join(f"{s.seq}:{s.altitude_m}m {s.battery_pct}%" for s in samples))
    if station.Land(pb.LandRequest(), timeout=10).landed:
        print("landed")
    for x in (gcs, drone, *peers, *links):
        x.close()


if __name__ == "__main__":
    main()
