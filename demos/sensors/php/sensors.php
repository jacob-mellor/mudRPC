<?php
/* Sensor telemetry over UDP in PHP: a gateway and two sensors, three peers on 127.0.0.1 pumped by one loop, the binding's
   own API with the messages protoc generates from sensor.proto. Each sensor publishes a Reading on a topic the gateway
   listens to; with both in hand the gateway calibrates sensor-2 by a call, which sensor-2 answers. */
declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';
require __DIR__ . '/../../../bindings/php/Mud.php';

use Demo\Sensors\Ack;
use Demo\Sensors\Offset;
use Demo\Sensors\Reading;
use Mud\Error;
use Mud\Peer;
use Mud\Udp;

define('KEY', implode(array_map('chr', range(1, 32))));   // the fleet key: a product provisions 32 random bytes to every peer

/** A free UDP port: bind one on port 0 and read the number the system chose. */
function port(): int
{
    $s = stream_socket_server('udp://127.0.0.1:0', $code, $message, STREAM_SERVER_BIND);
    $port = (int) substr(stream_socket_get_name($s, false), 10);
    fclose($s);
    return $port;
}

function reading(string $sensor, int $centiDegrees): string
{
    return (new Reading())->setSensor($sensor)->setCentiDegrees($centiDegrees)->serializeToString();
}

$names = ['gateway', 'sensor-1', 'sensor-2'];
$addresses = array_map(fn () => '127.0.0.1:' . port(), $names);
$peers = [];
$radios = [];
foreach ($names as $i => $name) {
    $peers[$i] = new Peer($name, KEY, $i + 1, 1);
    $radios[$i] = new Udp($addresses[$i], ...array_diff_key($addresses, [$i => 0]));   // the other two
}
[$gateway, $sensor1, $sensor2] = $peers;
$gateway->listen('sensors.Readings');
$sensor2->listen('demo.sensors.Sensor');
$sensor1->publish(0, 'sensors.Readings', reading('sensor-1', 2150), 60);
$sensor2->publish(0, 'sensors.Readings', reading('sensor-2', 1900), 60);

$start = hrtime(true);
$readings = [];
$done = false;
while (!$done) {
    $now = intdiv(hrtime(true) - $start, 1_000_000_000);
    foreach ($peers as $i => $peer) {
        while (($o = $peer->outgoing($now)) !== null) {
            try {
                $radios[$i]->send($o);
            } catch (Error $e) {
                if ($e->getCode() === -2) {
                    break;   // no peer took it: the next round
                }
                throw $e;
            }
            $peer->sent();
        }
        while (($w = $radios[$i]->poll()) !== null) {
            $in = $peer->deliver($now, $w[0], $w[1]);
            if ($in->kind === 'event') {   // the gateway: a reading
                $r = new Reading();
                $r->mergeFromString($in->bytes);
                $readings[$r->getSensor()] = $r->getCentiDegrees();
                if (count($readings) === 2) {
                    ksort($readings);
                    foreach ($readings as $sensor => $centi) {
                        printf("reading %s %.2fC\n", $sensor, $centi / 100);
                    }
                    $gateway->call($now, 'demo.sensors.Sensor', 'Calibrate', (new Offset())->setCentiDegrees(50)->serializeToString(), 10);
                }
            } elseif ($in->kind === 'call') {   // sensor-2: Calibrate
                $offset = new Offset();
                $offset->mergeFromString($in->bytes);
                $ack = (new Ack())->setSensor($names[$i])->setOffsetCentiDegrees($offset->getCentiDegrees());
                $peer->reply($now, $in, 0, $ack->serializeToString());
            } elseif ($in->kind === 'reply') {   // the gateway: the ack
                $ack = new Ack();
                $ack->mergeFromString($in->bytes);
                printf("calibrated %s by %.2fC\n", $ack->getSensor(), $ack->getOffsetCentiDegrees() / 100);
                $done = true;
            }
        }
        while ($peer->tick($now) !== null) {
        }
    }
    usleep(5000);
}

foreach ($radios as $radio) {
    $radio->close();
}
foreach ($peers as $peer) {
    $peer->close();
}
