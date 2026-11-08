// gRPC drone telemetry in Java: the stub and base grpc-java generated from drone.proto, over two mudRPC peers on UDP in one process.
import demo.drones.DroneGrpc;
import demo.drones.DroneOuterClass.ArmReply;
import demo.drones.DroneOuterClass.ArmRequest;
import demo.drones.DroneOuterClass.LandReply;
import demo.drones.DroneOuterClass.LandRequest;
import demo.drones.DroneOuterClass.Sample;
import demo.drones.DroneOuterClass.TelemetryRequest;
import io.grpc.Status;
import io.grpc.StatusRuntimeException;
import io.grpc.stub.StreamObserver;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.util.StringJoiner;
import java.util.concurrent.TimeUnit;
import mudrpc.Mud;
import mudrpc.MudChannel;

public final class Drones {
    static final byte[] KEY = new byte[32]; // the fleet key; a product provisions 32 random bytes to every peer
    static { for (int i = 0; i < 32; i++) KEY[i] = (byte) (i + 1); }

    public static void main(String[] args) throws Exception {
        int[] port = freePorts();
        Mud.Udp gcsUdp = new Mud.Udp("127.0.0.1:" + port[0], "127.0.0.1:" + port[1]), droneUdp = new Mud.Udp("127.0.0.1:" + port[1], "127.0.0.1:" + port[0]);
        Mud.Peer gcsPeer = new Mud.Peer("gcs", KEY, 1, 1), dronePeer = new Mud.Peer("drone", KEY, 2, 1);
        MudChannel gcs = new MudChannel(gcsPeer, gcsUdp), drone = new MudChannel(dronePeer, droneUdp);
        drone.addService(new MockDrone());

        // From here, the code a grpc-java product already has.
        DroneGrpc.DroneBlockingStub station = DroneGrpc.newBlockingStub(gcs).withDeadlineAfter(10, TimeUnit.SECONDS);
        try { station.land(LandRequest.getDefaultInstance()); }
        catch (StatusRuntimeException e) { System.out.println("land: " + e.getStatus().getCode() + " " + e.getStatus().getDescription()); }
        ArmReply armed = station.arm(ArmRequest.newBuilder().setPilot("gcs").build());
        System.out.println("armed " + armed.getDrone() + " for gcs");
        StringJoiner samples = new StringJoiner(" ", "telemetry ", "");
        station.telemetry(TelemetryRequest.newBuilder().setSamples(3).build()).forEachRemaining(s -> samples.add(s.getSeq() + ":" + s.getAltitudeM() + "m " + s.getBatteryPct() + "%"));
        System.out.println(samples);
        if (station.land(LandRequest.getDefaultInstance()).getLanded()) System.out.println("landed");

        gcs.close(); drone.close();
        gcsPeer.close(); dronePeer.close();
        gcsUdp.close(); droneUdp.close();
    }

    static final class MockDrone extends DroneGrpc.DroneImplBase {
        volatile boolean armed;

        @Override public void arm(ArmRequest r, StreamObserver<ArmReply> out) {
            armed = true;
            out.onNext(ArmReply.newBuilder().setArmed(true).setDrone("drone-1").build());
            out.onCompleted();
        }

        @Override public void telemetry(TelemetryRequest r, StreamObserver<Sample> out) {
            for (int i = 0; i < r.getSamples(); i++) out.onNext(Sample.newBuilder().setSeq(i).setAltitudeM(10 * (i + 1)).setBatteryPct(100 - 2 * i).build());
            out.onCompleted();
        }

        @Override public void land(LandRequest r, StreamObserver<LandReply> out) {
            if (!armed) { out.onError(Status.FAILED_PRECONDITION.withDescription("not armed").asRuntimeException()); return; }
            armed = false;
            out.onNext(LandReply.newBuilder().setLanded(true).build());
            out.onCompleted();
        }
    }

    /** Two free loopback ports, held together so they differ. */
    static int[] freePorts() throws Exception {
        try (DatagramSocket a = new DatagramSocket(0, InetAddress.getLoopbackAddress()); DatagramSocket b = new DatagramSocket(0, InetAddress.getLoopbackAddress())) {
            return new int[] { a.getLocalPort(), b.getLocalPort() };
        }
    }
}
