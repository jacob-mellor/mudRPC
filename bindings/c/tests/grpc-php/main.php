<?php
/* The gRPC drop-in under test: the client grpc_php_plugin generated from drone.proto, written as gRPC PHP code is, over a
   mudRPC peer in this process to a native server behind a radio of the test's own, then over UDP to a server process.
   Building the channel is the only mudRPC code above the stub. */
declare(strict_types=1);

require __DIR__ . '/vendor/autoload.php';
require __DIR__ . '/mud/Mud.php';
require __DIR__ . '/mud/MudGrpc.php';
require __DIR__ . '/native.php';

use Mud\Channel;
use Mud\Outgoing;
use Mud\Peer;
use Mud\Radio;
use Mud\ServerStreamingCall;
use Mud\Udp;
use Test\ArmRequest;
use Test\DroneClient;
use Test\Sample;
use Test\TelemetryRequest;

define('KEY', implode(array_map('chr', range(1, 32))));
const NAMES = ['OK', 'CANCELLED', 'UNKNOWN', 'INVALID_ARGUMENT', 'DEADLINE_EXCEEDED', 'NOT_FOUND', 'ALREADY_EXISTS', 'PERMISSION_DENIED', 'RESOURCE_EXHAUSTED', 'FAILED_PRECONDITION', 'ABORTED', 'OUT_OF_RANGE', 'UNIMPLEMENTED', 'INTERNAL', 'UNAVAILABLE', 'DATA_LOSS', 'UNAUTHENTICATED'];

function say(string $s): void
{
    echo $s, "\n";
}

function status(object $s): string
{
    return NAMES[$s->code] . ($s->details === '' ? ($s->code ? ' ""' : '') : " $s->details");
}

function thrown(callable $f): string
{
    try {
        $f();
        return 'ok';
    } catch (Throwable $e) {
        return $e->getMessage();
    }
}

function stub(Channel $channel): DroneClient
{
    return new DroneClient('drone', ['credentials' => null, 'grpc_call_invoker' => $channel]);
}

function arm(DroneClient $c, int $id, array $options = [], string $note = ''): string
{
    [$r, $s] = $c->Arm((new ArmRequest())->setId($id)->setNote($note), [], $options)->wait();
    return $s->code === 0 ? 'armed ' . var_export($r->getArmed(), true) . " id {$r->getId()} by {$r->getBy()}" : status($s);
}

function raw(Channel $channel, string $method): string
{
    $c = $channel->UnaryCall($channel->createChannelFactory('drone', []), $method, ['\Test\ArmReply', 'decode'], []);
    $c->start((new ArmRequest())->setId(1));
    return status($c->wait()[1]);
}

function telemetry(DroneClient $c, int $count, int $failAfter = 0, array $options = []): ServerStreamingCall
{
    return $c->Telemetry((new TelemetryRequest())->setCount($count)->setFailAfter($failAfter), [], $options);
}

function items(ServerStreamingCall $call, int $sleepUs = 0): array
{
    $seen = [];
    foreach ($call->responses() as $s) {
        $seen[] = $s->getSeq();
        usleep($sleepUs);
    }
    return $seen;
}

function read(ServerStreamingCall $call): string
{
    return implode(' ', items($call)) . ', ' . status($call->getStatus());
}

function port(): int
{
    $s = stream_socket_server('udp://127.0.0.1:0', $code, $message, STREAM_SERVER_BIND);
    $port = (int) substr(stream_socket_get_name($s, false), 10);
    fclose($s);
    return $port;
}

/** The radio between the client and the native server: drops nothing unless cut, and can hold the server's wires to deliver them reversed. */
final class Link implements Radio
{
    public array $inbox = [];
    public array $held = [];
    public ?Native $native = null;
    public bool $cut = false;
    public bool $refuseListen = false;
    public int $hold = 0;
    public int $lastTtl = 0;

    public function send(Outgoing $o): void
    {
        if ($o->kind === 'listen') {
            if ($this->refuseListen) {
                throw new RuntimeException('this radio has no listen');
            }
            return;
        }
        $this->lastTtl = $o->ttl;
        if (!$this->cut) {
            $this->native?->take($o->topic, $o->wire);
        }
    }

    public function poll(): ?array
    {
        if ($this->native) {
            $this->native->pump();
            foreach ($this->native->out as $o) {
                if ($o->kind !== 'send') {
                    continue;
                }
                $w = [$o->topic, $o->wire];
                if (!$this->hold) {
                    $this->inbox[] = $w;
                    continue;
                }
                $this->held[] = $w;
                if (count($this->held) >= $this->hold) {
                    array_push($this->inbox, ...array_reverse($this->held));
                    [$this->held, $this->hold] = [[], 0];
                }
            }
            $this->native->out = [];
        }
        return array_shift($this->inbox);
    }
}

$link = new Link();
$link->native = new Native(new Peer('drone', KEY, 2, 1));
$gcs = new Channel(new Peer('gcs', KEY, 1, 1), $link);

// From here, the code a gRPC PHP product already has.
$client = stub($gcs);
say('arm: ' . arm($client, 7));
$call = $client->Arm((new ArmRequest())->setId(8));
$call->wait();
say("peer: {$call->getPeer()}, metadata " . json_encode($call->getMetadata()) . ' ' . json_encode($call->getTrailingMetadata()) . ", target {$client->getTarget()}, state {$client->getConnectivityState()}");
say('refused: ' . arm($client, 0));
say('unknown method: ' . raw($gcs, '/test.Drone/Fly'));
say('a service name empty, past 64 bytes or not UTF-8, a method name not UTF-8: ' . implode(' / ', array_unique([raw($gcs, '//Arm'), raw($gcs, '/' . str_repeat('x', 65) . '/Arm'), raw($gcs, "/test.\xff/Arm"), raw($gcs, "/test.Drone/\xff")])));
say('a method name of 65 bytes, carried: ' . raw($gcs, '/test.Drone/' . str_repeat('y', 65)));
$link->hold = 5;   // five items, delivered newest first
say('telemetry reversed: ' . read(telemetry($client, 5)));
say('telemetry empty: ' . read(telemetry($client, 0)));
say('telemetry fails after: ' . read(telemetry($client, 5, 2)));
$upload = $client->Upload();
$upload->write(new Sample());
$upload->writesDone();
say('upload: ' . status($upload->wait()[1]));
$chat = $client->Chat();
$chat->write(new Sample());
$chat->writesDone();
say('chat: ' . var_export($chat->read(), true) . ', ' . status($chat->getStatus()));
say('too large: ' . arm($client, 1, [], str_repeat('x', 1000)));

$link->cut = true;
$t0 = hrtime(true);
say('deadline: ' . arm($client, 9, ['timeout' => 2_000_000]));
$took = (hrtime(true) - $t0) / 1e9;
say('deadline in time: ' . var_export($took >= 2.0 && $took < 3.5, true));
$cancelled = $client->Arm((new ArmRequest())->setId(10), [], ['timeout' => 3_000_000]);
$cancelled->cancel();
say('cancelled: ' . status($cancelled->wait()[1]));
say('past the deadline: ' . arm($client, 1, ['timeout' => 0]));
say('a timeout of NaN: ' . arm($client, 1, ['timeout' => NAN]));
$link->cut = false;
say('an infinite timeout: ' . arm($client, 1, ['timeout' => INF]));
say('a timeout that is no number: ' . arm($client, 1, ['timeout' => 'soon']));
say('after the cut: ' . arm($client, 11));
say('a code with no details: ' . arm($client, 22));
say('a reply that does not decode: ' . thrown(fn () => arm($client, 30)));
say('two full streams back to back: ' . count(items(telemetry($client, 63))) . ' ' . count(items(telemetry($client, 63))));

$lonePeer = new Peer('lone', KEY, 9, 1);
$lone = new Channel($lonePeer, new Link());
$lonePeer->close();
say('the peer closed first: ' . arm(stub($lone), 1, ['timeout' => 2_000_000]));

usleep(1_200_000);   // the cancelled call's ttl passes: its slot is free again
$gcs->pump();
$target = $gcs->createChannelFactory('drone', []);
$calls = [];
$link->hold = 16 - $gcs->peer->pending();   // every reply held until the last call is in flight
for ($i = $gcs->peer->pending(); $i < 16; $i++) {
    $calls[] = $c = $gcs->UnaryCall($target, '/test.Drone/Fly', ['\Test\ArmReply', 'decode'], []);
    $c->start(new ArmRequest());
}
say('a seventeenth call: ' . raw($gcs, '/test.Drone/Arm'));
say('unknown method, every free slot at once: ' . count($calls) . ' ' . implode(' / ', array_unique(array_map(fn ($c) => status($c->wait()[1]), $calls))));

$link->cut = true;
$far = $client->Arm((new ArmRequest())->setId(1), [], ['timeout' => 8 * 86400 * 1e6]);
say('a deadline eight days out: ttl ' . $link->lastTtl);
$far->cancel();
$link->cut = false;
for ($i = 0; $i < 5000; $i++) {
    $link->inbox[] = ['gcs', str_repeat("\0", 40)];
}
say('five thousand junk wires: then ' . arm($client, 1));

$stream = telemetry($client, 5);
$reader = $stream->responses();
$first = $reader->current()->getSeq();
$stream->cancel();
$reader->next();
say("cancelled with items unread: $first, more " . var_export($reader->valid(), true) . ', ' . status($stream->getStatus()));
$t0 = hrtime(true);
$slow = telemetry($client, 5, 0, ['timeout' => 1_000_000]);
say('a slow reader past its deadline: ' . implode(' ', items($slow, 600_000)) . ', ' . status($slow->getStatus()));
say('the slow reader\'s deadline in time: ' . var_export((hrtime(true) - $t0) / 1e9 < 2.0, true));
$saves = 0;
$seen = [];
foreach (telemetry($client, 5)->responses() as $s) {
    $seen[] = $s->getSeq();
    $gcs->peer->save();
    $saves++;
}
say('saved mid-stream: ' . implode(' ', $seen) . ', saves ' . ($saves ? 'made' : 'none'));

$link3 = new Link();
$link3->refuseListen = true;
$link3->native = new Native(new Peer('drone3', KEY, 8, 1));
$faults = [];
$gcs3 = new Channel(new Peer('gcs3', KEY, 7, 1), $link3, fault: function (Throwable $x) use (&$faults): void {
    $faults[] = $x->getMessage();
});
say('a radio refusing a listen: ' . arm(stub($gcs3), 1, ['timeout' => 10_000_000]) . ', faults heard ' . ($faults ? 'yes' : 'no'));

[$mine, $theirs] = [port(), port()];
$server = proc_open([PHP_BINARY, __DIR__ . '/server.php', (string) $theirs, (string) $mine], [['pipe', 'r'], ['pipe', 'w'], ['pipe', 'w']], $pipes);
fgets($pipes[1]);   // ready
$udp = new Udp("127.0.0.1:$mine", "127.0.0.1:$theirs");
$gcsUdp = new Channel(new Peer('gcsu', KEY, 11, 1), $udp);
$overUdp = stub($gcsUdp);
say('over UDP: ' . arm($overUdp, 1, ['timeout' => 5_000_000]) . ', telemetry ' . read(telemetry($overUdp, 5, 0, ['timeout' => 5_000_000])));
$lostSocket = new Udp('127.0.0.1:0', "127.0.0.1:$theirs");
$lost = new Channel(new Peer('lost', KEY, 13, 1), $lostSocket);
$lostSocket->close();
say('its socket closed: ' . arm(stub($lost), 1, ['timeout' => 2_000_000]));
say('a public bind: ' . thrown(fn () => new Udp('0.0.0.0:0', '127.0.0.1:9')) . ', a public peer: ' . thrown(fn () => new Udp('127.0.0.1:0', '8.8.8.8:53')) . ', no peer: ' . thrown(fn () => new Udp('127.0.0.1:0')) . ', this library public: ' . var_export(Udp::internet(), true));
say('a server that never answers: ' . arm($client, 24, ['timeout' => 1_000_000]));

$saved = $gcs->peer->save();
say('restored at a higher epoch: ' . thrown(fn () => Peer::restore($saved, 2)) . ', at the same: ' . thrown(fn () => Peer::restore($saved, 1)) . ', a short copy: ' . thrown(fn () => Peer::restore(substr($saved, 0, 100), 3)));
$closed = new Peer('x', KEY, 5, 1);
$closed->close();
say('a closed peer: ' . thrown(fn () => $closed->call(0, 'a', 'b', '', 1)) . ', pending ' . $closed->pending() . ', a short key: ' . thrown(fn () => new Peer('x', 'short', 1, 1)));

$client->close();
say('after the stub closed: ' . arm($client, 1));
fclose($pipes[0]);
proc_close($server);
foreach ([$gcs3, $gcsUdp, $lost, $lone] as $channel) {
    $channel->close();
}
say('every channel closed');
