"""Sensor telemetry over UDP in Python: a gateway and two sensors, three peers over UDP in one loop, the binding's own
API with the messages protoc generated from sensor.proto."""
import socket
import time

import mud
import sensor_pb2 as pb

KEY = bytes(range(1, 33))  # the fleet key: a product provisions 32 random bytes to every peer
EPOCH = 1
TOPIC, SERVICE = "sensors.Readings", "demo.sensors.Sensor"
NAMES = ["gateway", "sensor-1", "sensor-2"]


def free():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def degrees(centi):
    return f"{centi / 100:.2f}C"


def main():
    ports = [free() for _ in NAMES]
    peers = [mud.Peer(n, KEY, i + 1, EPOCH) for i, n in enumerate(NAMES)]
    links = [mud.Udp(f"127.0.0.1:{p}", *(f"127.0.0.1:{q}" for q in ports if q != p)) for p in ports]
    gateway, sensor1, sensor2 = peers
    gateway.listen(TOPIC)
    sensor2.listen(SERVICE)
    sensor1.publish(0, TOPIC, pb.Reading(sensor="sensor-1", centi_degrees=2150).SerializeToString(), 60)
    sensor2.publish(0, TOPIC, pb.Reading(sensor="sensor-2", centi_degrees=1900).SerializeToString(), 60)

    readings, start, done = {}, time.monotonic(), False
    while not done:
        now = int(time.monotonic() - start)
        for name, p, u in zip(NAMES, peers, links):
            while (o := p.outgoing(now)) is not None:
                try:
                    u.send(o)
                except mud.MudError:
                    break
                p.sent()
            while (wire := u.poll()) is not None:
                heard = p.deliver(now, *wire)
                if heard.kind == "event":
                    r = pb.Reading.FromString(heard.bytes)
                    readings[r.sensor] = r.centi_degrees
                    if len(readings) == 2:
                        for sensor in sorted(readings):
                            print(f"reading {sensor} {degrees(readings[sensor])}")
                        p.call(now, SERVICE, "Calibrate", pb.Offset(centi_degrees=50).SerializeToString(), 10)
                elif heard.kind == "call":
                    offset = pb.Offset.FromString(heard.bytes).centi_degrees
                    p.reply(now, heard, 0, pb.Ack(sensor=name, offset_centi_degrees=offset).SerializeToString())
                elif heard.kind == "reply":
                    ack = pb.Ack.FromString(heard.bytes)
                    print(f"calibrated {ack.sensor} by {degrees(ack.offset_centi_degrees)}")
                    done = True
            p.tick(now)
        time.sleep(0.005)
    for x in (*peers, *links):
        x.close()


if __name__ == "__main__":
    main()
