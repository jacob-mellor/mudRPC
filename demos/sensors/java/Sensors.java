// Sensor telemetry over UDP in Java: three mudRPC peers in one loop, the binding's own API with sensor.proto's messages, no gRPC.
import demo.sensors.SensorOuterClass.Ack;
import demo.sensors.SensorOuterClass.Offset;
import demo.sensors.SensorOuterClass.Reading;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.util.Map;
import java.util.TreeMap;
import java.util.stream.IntStream;
import mudrpc.Mud.*;

public final class Sensors {
    static final byte[] KEY = new byte[32]; // the fleet key; a product provisions 32 random bytes to every peer
    static { for (int i = 0; i < 32; i++) KEY[i] = (byte) (i + 1); }
    static final String[] NAMES = { "gateway", "sensor-1", "sensor-2" };

    public static void main(String[] args) throws Exception {
        int[] ports = freePorts(3);
        Peer[] peers = new Peer[3];
        Udp[] radios = new Udp[3];
        for (int i = 0; i < 3; i++) {
            peers[i] = new Peer(NAMES[i], KEY, i + 1, 1);
            int me = i;
            radios[i] = new Udp("127.0.0.1:" + ports[i], IntStream.range(0, 3).filter(j -> j != me).mapToObj(j -> "127.0.0.1:" + ports[j]).toArray(String[]::new));
        }
        peers[0].listen("sensors.Readings");
        peers[2].listen("demo.sensors.Sensor");
        peers[1].publish(0, "sensors.Readings", Reading.newBuilder().setSensor("sensor-1").setCentiDegrees(2150).build().toByteArray(), 60);
        peers[2].publish(0, "sensors.Readings", Reading.newBuilder().setSensor("sensor-2").setCentiDegrees(1900).build().toByteArray(), 60);

        Map<String, Integer> readings = new TreeMap<>();
        boolean calibrating = false, done = false;
        long start = System.nanoTime();
        while (!done) {
            int now = (int) ((System.nanoTime() - start) / 1_000_000_000L);
            for (int i = 0; i < 3; i++) {
                Peer p = peers[i];
                Outgoing o;
                while ((o = p.outgoing(now)) != null) {
                    try { radios[i].send(o); } catch (MudException refused) { break; }
                    p.sent();
                }
                for (Udp.Datagram d; (d = radios[i].poll()) != null; ) {
                    Incoming in = p.deliver(now, d.topic(), d.wire());
                    switch (in.kind()) {
                        case EVENT -> { Reading r = Reading.parseFrom(in.bytes()); readings.put(r.getSensor(), r.getCentiDegrees()); }
                        case CALL -> p.reply(now, in, 0, Ack.newBuilder().setSensor(NAMES[i]).setOffsetCentiDegrees(Offset.parseFrom(in.bytes()).getCentiDegrees()).build().toByteArray());
                        case REPLY -> { Ack a = Ack.parseFrom(in.bytes()); System.out.println("calibrated " + a.getSensor() + " by " + celsius(a.getOffsetCentiDegrees())); done = true; }
                        default -> { }
                    }
                }
                while (p.tick(now) != null) { }
            }
            if (readings.size() == 2 && !calibrating) {
                readings.forEach((sensor, centi) -> System.out.println("reading " + sensor + " " + celsius(centi)));
                peers[0].call(now, "demo.sensors.Sensor", "Calibrate", Offset.newBuilder().setCentiDegrees(50).build().toByteArray(), 10);
                calibrating = true;
            }
            Thread.sleep(5);
        }
        for (Peer p : peers) p.close();
        for (Udp r : radios) r.close();
    }

    static String celsius(int centi) { return String.format("%d.%02dC", centi / 100, centi % 100); }

    /** Free loopback ports, held together so they differ. */
    static int[] freePorts(int n) throws Exception {
        DatagramSocket[] held = new DatagramSocket[n];
        try { for (int i = 0; i < n; i++) held[i] = new DatagramSocket(0, InetAddress.getLoopbackAddress()); return IntStream.range(0, n).map(i -> held[i].getLocalPort()).toArray(); }
        finally { for (DatagramSocket s : held) if (s != null) s.close(); }
    }
}
