<?php
/* mudRPC as gRPC's transport in PHP: the client grpc_php_plugin generates from a .proto runs over mudRPC unchanged; the
   options that build the stub are the one line that differs. Beside Mud.php, with the composer package grpc/grpc, whose
   CallInvoker this implements and whose call objects these stand in for (gRPC PHP, Copyright The gRPC Authors, Apache
   License 2.0). A client, as gRPC's PHP is; a PHP server answers with Mud\Peer::reply.

   Not a drop-in where gRPC needs a connection: a message of about 460 bytes, a service's full name of at most 64 bytes, no
   metadata, unary and server-streaming calls only, the first reply wins, and 16 calls in flight. PHP has one thread, so
   the channel pumps while a call waits, and a client keeps under 32 calls a second. A timeout is the call's ttl, a week at
   most; a call cancelled after it was sent keeps its place among the 16 until its ttl, since nothing reaches the server to
   say so. */
declare(strict_types=1);

namespace Grpc {
    // gRPC's numbers and the credentials class its BaseStub asks about, where the grpc extension is not loaded.
    (static function (): void {
        foreach (['OK', 'CANCELLED', 'UNKNOWN', 'INVALID_ARGUMENT', 'DEADLINE_EXCEEDED', 'NOT_FOUND', 'ALREADY_EXISTS', 'PERMISSION_DENIED', 'RESOURCE_EXHAUSTED', 'FAILED_PRECONDITION', 'ABORTED', 'OUT_OF_RANGE', 'UNIMPLEMENTED', 'INTERNAL', 'UNAVAILABLE', 'DATA_LOSS', 'UNAUTHENTICATED'] as $i => $name) {
            defined("Grpc\\STATUS_$name") || define("Grpc\\STATUS_$name", $i);
        }
        foreach (['IDLE', 'CONNECTING', 'READY', 'TRANSIENT_FAILURE', 'FATAL_FAILURE'] as $i => $name) {
            defined("Grpc\\CHANNEL_$name") || define("Grpc\\CHANNEL_$name", $i);
        }
    })();
    if (!class_exists(ChannelCredentials::class, false)) {
        final class ChannelCredentials
        {
            public static function isDefaultRootsPemSet(): bool
            {
                return true;
            }
        }
    }
}

namespace Mud {
    const STREAM_END = 'end';   // the detail of the reply that ends a server stream and carries no item
    const WEEK = 604800;
    const INBOX = 1024;         // wires taken from the radio in one round
    const PATIENCE = 1e9;       // nanoseconds a refused item is tried again before it is let go

    /**
     * A gRPC call invoker over one peer: `new DroneClient('drone', ['credentials' => null, 'grpc_call_invoker' => $channel])`.
     *
     * $radio is a Mud\Udp, or your own Radio. $ttl is the life of a call made without a timeout; $fault hears every
     * exception the radio raises, and the item send() raised on is tried for a second, then let go. The channel pumps the
     * peer while a call waits, and in pump() when your own loop calls it; the peer is yours between those.
     */
    final class Channel implements \Grpc\CallInvoker
    {
        private array $pending = [];   // id => WeakReference to a Call not yet over: a call the caller let go is let go here too
        private ?\Closure $fault;
        private int $start;
        private int $refusedSince = 0;
        private bool $closed = false;

        public function __construct(public readonly Peer $peer, private readonly Radio $radio, private readonly int $ttl = 60, ?callable $fault = null)
        {
            $this->fault = $fault ? $fault(...) : null;
            $this->start = hrtime(true);
        }

        public function createChannelFactory($hostname, $opts): Target
        {
            return new Target($this, (string) $hostname);
        }

        public function UnaryCall($channel, $method, $deserialize, $options): UnaryCall
        {
            return new UnaryCall($this, $channel, $method, $deserialize, $options, false);
        }

        public function ServerStreamingCall($channel, $method, $deserialize, $options): ServerStreamingCall
        {
            return new ServerStreamingCall($this, $channel, $method, $deserialize, $options, true);
        }

        public function ClientStreamingCall($channel, $method, $deserialize, $options): ClientStreamingCall
        {
            return new ClientStreamingCall();
        }

        public function BidiStreamingCall($channel, $method, $deserialize, $options): BidiStreamingCall
        {
            return new BidiStreamingCall();
        }

        public function now(): int
        {
            return intdiv(hrtime(true) - $this->start, 1_000_000_000);
        }

        /** One round: what the radio has, the clock, then what the peer has for the radio. */
        public function pump(): void
        {
            if ($this->closed) {
                return;
            }
            try {
                $now = $this->now();
                try {
                    for ($i = 0; $i < INBOX && ($w = $this->radio->poll()) !== null; $i++) {
                        try {
                            $this->land($this->peer->deliver($now, $w[0], $w[1]));
                        } catch (Error $e) {
                            if ($e->getCode() <= -4) {
                                throw $e;
                            }   // else a wire the radio mangled: no verdict to act on
                        }
                    }
                } catch (Error | Closed $x) {
                    throw $x;
                } catch (\Throwable $x) {   // the radio's, told to fault
                    $this->tell($x);
                }
                while (($e = $this->peer->tick($now)) !== null) {
                    $this->land($e);
                }
                $t = hrtime(true);
                foreach ($this->pending as $id => $ref) {
                    $c = $ref->get();
                    if ($c === null) {
                        unset($this->pending[$id]);   // the caller let the call go: nothing to land on
                    } elseif ($c->deadline && $t >= $c->deadline) {
                        $c->abort(\Grpc\STATUS_DEADLINE_EXCEEDED, 'Deadline Exceeded');
                    }
                }
                while (($o = $this->peer->outgoing($now)) !== null) {
                    try {
                        $this->radio->send($o);
                    } catch (Closed $x) {
                        throw $x;
                    } catch (\Throwable $x) {   // the radio's, told to fault
                        $this->tell($x);
                        $this->refusedSince = $this->refusedSince ?: $t;
                        if ($t - $this->refusedSince < PATIENCE) {
                            return;   // the next round tries it again; past a second it is let go
                        }
                    }
                    $this->refusedSince = 0;
                    $this->peer->sent();
                }
            } catch (Error $e) {
                $this->shut(\Grpc\STATUS_UNAVAILABLE, 'the peer is closed: ' . $e->getMessage());
            } catch (Closed $e) {
                $this->shut(\Grpc\STATUS_UNAVAILABLE, $e->getMessage());
            }
        }

        private function land(Incoming $e): void
        {
            if ($e->kind === 'reply' || $e->kind === 'expired') {
                ($this->pending[$e->id] ?? null)?->get()?->land($e);
            }
        }

        private function tell(\Throwable $x): void
        {
            try {
                $this->fault?->__invoke($x);
            } catch (\Throwable) {
            }
        }

        /** Pumps until $until() holds: a call waiting for its reply, its next item or its status. */
        public function waitFor(callable $until): void
        {
            while (true) {
                $this->pump();   // first, so a deadline is seen before an item that landed under it is read
                if ($until()) {
                    return;
                }
                usleep(1000);
            }
        }

        /** @internal */
        public function begin(\Grpc\AbstractCall $c, string $service, string $method, string $payload, mixed $timeout): void
        {
            if ($this->closed) {
                $c->close(\Grpc\STATUS_UNAVAILABLE, 'the channel is closed');
                return;
            }
            if (!self::named($service) || preg_match('//u', $method) !== 1) {
                $c->close(\Grpc\STATUS_INVALID_ARGUMENT, 'mudRPC: invalid argument');   // as the peer refuses an empty name
                return;
            }
            $life = $this->ttl;
            if (is_numeric($timeout) && !is_infinite((float) $timeout)) {   // microseconds, as gRPC PHP's
                $secs = (float) $timeout / 1e6;
                if (!($secs > 0)) {   // 0, negative or NaN
                    $c->close(\Grpc\STATUS_DEADLINE_EXCEEDED, 'Deadline Exceeded');
                    return;
                }
                $life = (int) min(ceil($secs) + 1, WEEK);   // a second over: the clock is whole seconds, the pump's check is the deadline
                if ($secs <= WEEK) {
                    $c->deadline = hrtime(true) + (int) ($secs * 1e9);
                }
            }
            try {
                $c->id = $this->peer->call($this->now(), $service, $method, $payload, $life);
            } catch (Error $e) {
                if ($e->getCode() <= -4) {
                    $this->shut(\Grpc\STATUS_UNAVAILABLE, 'the peer is closed: ' . $e->getMessage());
                }
                $c->close(match (true) {
                    $this->closed => \Grpc\STATUS_UNAVAILABLE,
                    in_array($e->getCode(), [-2, -3, -9], true) => \Grpc\STATUS_RESOURCE_EXHAUSTED,
                    $e->getCode() === -1 => \Grpc\STATUS_INVALID_ARGUMENT,
                    default => \Grpc\STATUS_INTERNAL,
                }, ($this->closed ? 'the peer is closed: ' : '') . $e->getMessage());
                return;
            }
            $this->pending[$c->id] = \WeakReference::create($c);
            $this->pump();
        }

        /** @internal */
        public function forget(\Grpc\AbstractCall $c): void
        {
            unset($this->pending[$c->id]);
        }

        /** Ends every call in flight with UNAVAILABLE; the peer is yours to close after. */
        public function close(): void
        {
            $this->shut(\Grpc\STATUS_UNAVAILABLE, 'the channel is closed');
        }

        private function shut(int $code, string $details): void
        {
            if ($this->closed) {
                return;
            }
            $this->closed = true;
            foreach ($this->pending as $ref) {
                $ref->get()?->abort($code, $details);
            }
            $this->pending = [];
        }

        /** UTF-8 of 1 to 64 bytes, as a service's full name is; core carries a method's name of any length. */
        private static function named(string $s): bool
        {
            return strlen($s) > 0 && strlen($s) <= MAX_TOPIC && preg_match('//u', $s) === 1;
        }
    }

    /** What BaseStub holds as its channel: the stub's target name, and the invoker's channel behind it. */
    final class Target
    {
        public function __construct(public readonly Channel $channel, public readonly string $hostname)
        {
        }

        public function getTarget(): string
        {
            return $this->hostname;
        }

        public function getConnectivityState($try_to_connect = false): int
        {
            return \Grpc\CHANNEL_READY;   // no connection to watch: always ready to queue a call
        }

        public function watchConnectivityState($last_state, $deadline): bool
        {
            return false;
        }

        public function close(): void
        {
            $this->channel->close();
        }
    }

    /**
     * One call, as gRPC PHP's UnaryCall and ServerStreamingCall answer: replies land in any order and are taken in seq
     * order; the server's status comes after the items before it. A local end, a cancel, a timeout or a close, ends it
     * whatever the replies say, unread items cut off.
     */
    trait Call
    {
        public string $id = '';
        public int $deadline = 0;
        public bool $over = false;
        private bool $started = false;
        protected bool $exchangeDone = false;
        protected array $items = [];
        protected ?string $response = null;
        protected int $code = 0;
        protected string $details = '';
        private array $early = [];
        private int $next = 0;
        private int $sender = 0;

        public function __construct(protected readonly Channel $ch, private readonly Target $target, private readonly string $method, private readonly array $decoder, private readonly array $options, private readonly bool $stream)
        {
        }

        public function start($data, array $metadata = [], array $options = []): void
        {
            $this->started = true;
            $parts = explode('/', $this->method, 3);   // "/package.Service/Method"
            $this->ch->begin($this, $parts[1] ?? '', $parts[2] ?? '', $data->serializeToString(), $this->options['timeout'] ?? null);
        }

        /** Pumps until $until() holds; a call never started ends at once instead of waiting forever. */
        private function until(callable $until): void
        {
            if (!$this->started) {
                $this->close(\Grpc\STATUS_INTERNAL, 'the call was not started');
            }
            $this->ch->waitFor($until);
        }

        /** @internal */
        public function land(Incoming $e): void
        {
            if ($e->kind === 'expired') {
                $this->abort(\Grpc\STATUS_DEADLINE_EXCEEDED, 'Deadline Exceeded');
                return;
            }
            if ($this->exchangeDone || $e->seq < $this->next) {
                return;
            }
            $this->early[$e->seq] ??= $e;
            $this->sender = $e->sender;
            while (!$this->exchangeDone && isset($this->early[$this->next])) {
                $r = $this->early[$this->next];
                unset($this->early[$this->next++]);
                if ($r->status !== 0) {
                    $this->finish($r->status, $r->detail);
                } elseif ($r->end && $r->detail === STREAM_END) {
                    $this->finish(0, '');
                } else {
                    if ($this->stream) {
                        $this->items[] = $r->bytes;
                    } elseif ($this->response === null) {
                        $this->response = $r->bytes;
                    }
                    if ($r->end) {
                        $this->finish(0, '');
                    }
                }
            }
        }

        /** The server's status: it comes after the items before it, and the call ends when they are read. */
        private function finish(int $code, string $details): void
        {
            $this->exchangeDone = true;
            $this->early = [];
            if (!$this->stream || !$this->items) {
                $this->close($code, $details);
            } else {
                [$this->code, $this->details] = [$code, $details];
            }
        }

        /** A local end: whatever the exchange, the call ends now, unread items cut off. */
        public function abort(int $code, string $details): void
        {
            if (!$this->over) {
                $this->items = [];
                $this->exchangeDone = true;
                $this->close($code, $details);
            }
        }

        /** @internal */
        public function close(int $code, string $details): void
        {
            if ($this->over) {
                return;
            }
            $this->over = true;
            [$this->code, $this->details] = $code === 0 && !$this->stream && $this->response === null ? [\Grpc\STATUS_INTERNAL, 'no response'] : [$code, $details];
            $this->ch->forget($this);
        }

        protected function status(): object
        {
            return (object) ['code' => $this->code, 'details' => $this->details, 'metadata' => []];
        }

        protected function decode(string $raw): object
        {
            [$class] = $this->decoder;
            $message = new $class();
            $message->mergeFromString($raw);
            return $message;
        }

        public function cancel(): void
        {
            $this->abort(\Grpc\STATUS_CANCELLED, 'Cancelled');
        }

        public function getMetadata(): array
        {
            return [];
        }

        public function getTrailingMetadata(): array
        {
            return [];
        }

        /** The answering peer's roster number as mudrpc:N once a reply has landed, the stub's target before. */
        public function getPeer(): string
        {
            return $this->sender ? "mudrpc:$this->sender" : $this->target->hostname;
        }

        public function setCallCredentials($credentials): void
        {
        }
    }

    final class UnaryCall extends \Grpc\UnaryCall
    {
        use Call;

        /** [$response, $status]: the response null unless the status is OK. */
        public function wait(): array
        {
            $this->until(fn () => $this->over);
            return [$this->code === 0 ? $this->decode($this->response) : null, $this->status()];
        }
    }

    final class ServerStreamingCall extends \Grpc\ServerStreamingCall
    {
        use Call;

        public function responses(): \Generator
        {
            while (true) {
                $this->until(fn () => $this->items || $this->over);
                if (!$this->items) {
                    return;
                }
                $raw = array_shift($this->items);
                if (!$this->items && $this->exchangeDone) {
                    $this->close($this->code, $this->details);
                }
                yield $this->decode($raw);
            }
        }

        /** The server's status, waited for; after a local end, that end's. */
        public function getStatus(): object
        {
            $this->until(fn () => $this->over || $this->exchangeDone);
            return $this->status();
        }
    }

    /** A client or bidirectional stream: mudRPC carries neither, so every read is empty and the status UNIMPLEMENTED. */
    trait Refused
    {
        public function __construct()
        {
        }

        public function start(array $metadata = [], array $options = []): void
        {
        }

        public function write($data, array $options = []): void
        {
        }

        public function writesDone(): void
        {
        }

        public function read(): ?object
        {
            return null;
        }

        public function wait(): array
        {
            return [null, $this->getStatus()];
        }

        public function getStatus(): object
        {
            return (object) ['code' => \Grpc\STATUS_UNIMPLEMENTED, 'details' => 'mudRPC carries no client stream', 'metadata' => []];
        }

        public function getMetadata(): array
        {
            return [];
        }

        public function getTrailingMetadata(): array
        {
            return [];
        }

        public function getPeer(): string
        {
            return 'mudrpc';
        }

        public function cancel(): void
        {
        }

        public function setCallCredentials($credentials): void
        {
        }
    }

    final class ClientStreamingCall extends \Grpc\ClientStreamingCall
    {
        use Refused;
    }

    final class BidiStreamingCall extends \Grpc\BidiStreamingCall
    {
        use Refused;
    }
}
