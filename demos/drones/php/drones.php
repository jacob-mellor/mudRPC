<?php
/* gRPC drone telemetry in PHP: the client grpc_php_plugin generates from drone.proto, written as gRPC PHP code is, over
   Mud\Channel to a mock drone in this process that answers with Mud\Peer::reply. PHP has one thread, so two peers in one
   process do not meet over UDP: the station's radio hands each wire to the drone and queues its replies for the poll. */
declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';
require __DIR__ . '/../../../bindings/php/Mud.php';
require __DIR__ . '/../../../bindings/php/MudGrpc.php';

use Demo\Drones\ArmReply;
use Demo\Drones\ArmRequest;
use Demo\Drones\DroneClient;
use Demo\Drones\LandReply;
use Demo\Drones\LandRequest;
use Demo\Drones\Sample;
use Demo\Drones\TelemetryRequest;
use Mud\Channel;
use Mud\Incoming;
use Mud\Outgoing;
use Mud\Peer;
use Mud\Radio;

define('KEY', implode(array_map('chr', range(1, 32))));   // the fleet key: a product provisions 32 random bytes to every peer

/** The mock drone: a peer with no gRPC on its side, answering demo.drones.Drone; its wires for the station wait in $wires. */
final class Drone
{
    public array $wires = [];
    private bool $armed = false;
    private int $start;

    public function __construct(public readonly Peer $peer)
    {
        $peer->listen('demo.drones.Drone');
        $this->start = hrtime(true);
    }

    public function take(string $topic, string $wire): void
    {
        $now = intdiv(hrtime(true) - $this->start, 1_000_000_000);
        $c = $this->peer->deliver($now, $topic, $wire);
        if ($c->kind === 'call') {
            $this->answer($now, $c);
        }
        while (($o = $this->peer->outgoing($now)) !== null) {
            if ($o->kind === 'send') {
                $this->wires[] = [$o->topic, $o->wire];
            }
            $this->peer->sent();
        }
    }

    private function answer(int $now, Incoming $c): void
    {
        if ($c->method === 'Arm') {
            $this->armed = true;
            $this->peer->reply($now, $c, 0, (new ArmReply())->setArmed(true)->setDrone('drone-1')->serializeToString());
        } elseif ($c->method === 'Telemetry') {
            $request = new TelemetryRequest();
            $request->mergeFromString($c->bytes);
            for ($i = 0, $n = $request->getSamples(); $i < $n; $i++) {
                $sample = (new Sample())->setSeq($i)->setAltitudeM(10 * ($i + 1))->setBatteryPct(100 - 2 * $i);
                $this->peer->reply($now, $c, 0, $sample->serializeToString(), end: $i === $n - 1, seq: $i);
            }
        } elseif (!$this->armed) {
            $this->peer->reply($now, $c, 9, '', detail: 'not armed');   // FAILED_PRECONDITION
        } else {
            $this->armed = false;
            $this->peer->reply($now, $c, 0, (new LandReply())->setLanded(true)->serializeToString());
        }
    }
}

/** The station's radio: each wire straight to the drone, the drone's wires on the next poll. */
final class Link implements Radio
{
    public function __construct(private readonly Drone $drone)
    {
    }

    public function send(Outgoing $item): void
    {
        if ($item->kind === 'send') {
            $this->drone->take($item->topic, $item->wire);
        }
    }

    public function poll(): ?array
    {
        return array_shift($this->drone->wires);
    }
}

$drone = new Drone(new Peer('drone', KEY, 2, 1));
$gcs = new Channel(new Peer('gcs', KEY, 1, 1), new Link($drone));

// From here, the code a gRPC PHP product already has.
$client = new DroneClient('drone', ['credentials' => null, 'grpc_call_invoker' => $gcs]);
$within = ['timeout' => 10_000_000];

[, $status] = $client->Land(new LandRequest(), [], $within)->wait();
echo 'land: ', Mud\statusName($status->code), " $status->details\n";
[$reply] = $client->Arm((new ArmRequest())->setPilot('gcs'), [], $within)->wait();
echo "armed {$reply->getDrone()} for gcs\n";
$line = 'telemetry';
foreach ($client->Telemetry((new TelemetryRequest())->setSamples(3), [], $within)->responses() as $sample) {
    $line .= " {$sample->getSeq()}:{$sample->getAltitudeM()}m {$sample->getBatteryPct()}%";
}
echo $line, "\n";
[$reply] = $client->Land(new LandRequest(), [], $within)->wait();
echo $reply->getLanded() ? 'landed' : 'still flying', "\n";

$client->close();
$gcs->peer->close();
$drone->peer->close();
