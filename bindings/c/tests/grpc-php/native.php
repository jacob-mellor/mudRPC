<?php
/* The server under the test: a mudRPC peer with no gRPC on its side, answering test.Drone with Mud\Peer::reply. A stream
   ends on its last item, or with the detail `end` when it has none; a refusal has a detail; Arm 24 is never answered. */
declare(strict_types=1);

use Mud\Incoming;
use Mud\Peer;

final class Native
{
    public array $out = [];       // Outgoing items for the radio
    private array $owed = [];     // replies the peer's send cap left for the next round
    private int $start;

    public function __construct(public readonly Peer $peer)
    {
        $peer->listen('test.Drone');
        $this->start = hrtime(true);
    }

    public function now(): int
    {
        return intdiv(hrtime(true) - $this->start, 1_000_000_000);
    }

    public function take(string $topic, string $wire): void
    {
        $c = $this->peer->deliver($this->now(), $topic, $wire);
        if ($c->kind === 'call') {
            $this->answer($c);
        }
    }

    private function answer(Incoming $c): void
    {
        if ($c->method === 'Arm') {
            $r = new Test\ArmRequest();
            $r->mergeFromString($c->bytes);
            match (true) {
                $r->getId() === 0 => $this->owe($c, 9, '', 'battery low'),
                $r->getId() === 22 => $this->owe($c, 13, ''),
                $r->getId() === 24 => null,
                $r->getId() === 30 => $this->owe($c, 0, "\x0f"),
                default => $this->owe($c, 0, (new Test\ArmReply())->setArmed(true)->setId($r->getId())->setBy("mudrpc:$c->sender")->serializeToString()),
            };
        } elseif ($c->method === 'Telemetry') {
            $r = new Test\TelemetryRequest();
            $r->mergeFromString($c->bytes);
            $n = $r->getCount();
            if ($n === 0) {
                $this->owe($c, 0, '', Mud\STREAM_END);
            }
            for ($i = 0; $i < $n; $i++) {
                if ($r->getFailAfter() && $i === $r->getFailAfter()) {
                    $this->owe($c, 10, '', 'lost the camera', $i);
                    break;
                }
                $this->owe($c, 0, (new Test\Sample())->setSeq($i)->serializeToString(), '', $i, $i === $n - 1);
            }
        } else {
            $this->owe($c, 12, '', "no method $c->topic/$c->method");
        }
    }

    private function owe(Incoming $c, int $status, string $payload, string $detail = '', int $seq = 0, bool $end = true): void
    {
        $this->owed[] = [$c, $status, $payload, $detail, $seq, $end];
    }

    /** Sends what is owed, 32 a second as the peer allows, and moves the peer's items to $out. */
    public function pump(): void
    {
        $now = $this->now();
        while ($this->owed) {
            [$c, $status, $payload, $detail, $seq, $end] = $this->owed[0];
            try {
                $this->peer->reply($now, $c, $status, $payload, $end, $seq, $detail);
            } catch (Mud\Error $e) {
                if (in_array($e->getCode(), [-2, -9], true)) {
                    break;   // the outbox or this second's sends: the next round
                }
            }
            array_shift($this->owed);
        }
        while (($o = $this->peer->outgoing($now)) !== null) {
            $this->out[] = $o;
            $this->peer->sent();
        }
    }
}
