// The drop-in under Kotlin: grpc-kotlin's coroutine stub and service base for drone.proto, unchanged, over the same
// MudChannel the Java test uses. Building the two channels is the only mudRPC code here.
import io.grpc.Status
import io.grpc.StatusException
import java.util.concurrent.TimeUnit
import kotlin.system.exitProcess
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.runBlocking
import mudrpc.Mud
import mudrpc.MudChannel
import test.ArmReply
import test.ArmRequest
import test.DroneGrpcKt
import test.Sample
import test.TelemetryRequest

class Drone : DroneGrpcKt.DroneCoroutineImplBase() {
    override suspend fun arm(request: ArmRequest): ArmReply =
        if (request.id == 0) throw StatusException(Status.FAILED_PRECONDITION.withDescription("battery low"))
        else ArmReply.newBuilder().setArmed(true).setId(request.id).build()

    override fun telemetry(request: TelemetryRequest): Flow<Sample> = flow { for (i in 0 until request.count) emit(Sample.newBuilder().setSeq(i).build()) }
}

@Volatile var cut = false

suspend fun fails(what: String, f: suspend () -> Unit) {
    try { f(); println("$what: no error") } catch (e: StatusException) { println("$what: ${e.status.code} ${e.status.description}") }
}

fun main() {
    runBlocking { run() }
    exitProcess(0)
}

suspend fun run() {
    val key = ByteArray(32) { (it + 1).toByte() }
    lateinit var gcs: MudChannel
    lateinit var drone: MudChannel
    gcs = MudChannel(Mud.Peer("gcs", key, 1, 1)) { o -> if (!o.listen() && !cut) drone.deliver(o.topic(), o.wire()) }
    drone = MudChannel(Mud.Peer("drone", key, 2, 1)) { o -> if (!o.listen() && !cut) gcs.deliver(o.topic(), o.wire()) }
    drone.addService(Drone())

    // From here, the code a grpc-kotlin product already has.
    val client = DroneGrpcKt.DroneCoroutineStub(gcs)
    println("kotlin arm: armed ${client.arm(ArmRequest.newBuilder().setId(7).build()).armed}")
    println("kotlin telemetry: ${client.telemetry(TelemetryRequest.newBuilder().setCount(5).build()).toList().joinToString(" ") { it.seq.toString() }}")
    fails("kotlin refused") { client.arm(ArmRequest.newBuilder().setId(0).build()) }
    fails("kotlin upload") { client.upload(flowOf(Sample.getDefaultInstance())) }
    cut = true
    fails("kotlin deadline") { client.withDeadlineAfter(1, TimeUnit.SECONDS).arm(ArmRequest.newBuilder().setId(9).build()) }
    cut = false
    println("kotlin after the cut: armed ${client.arm(ArmRequest.newBuilder().setId(11).build()).armed}")
    gcs.close()
    drone.close()
}
