<?php
/* mudRPC for PHP: one file over the C ABI through FFI, no dependencies beyond PHP's own FFI extension.

   A call is a message; a reply may come hours later or expire; nothing blocks. You poll: drain outgoing() into your
   transport and call sent(), feed what arrives to deliver(), call tick(). $now is your uptime in seconds. The host
   contract every binding shares is the comment at the top of bindings/c/mudrpc.h. */
declare(strict_types=1);

namespace Mud;

use FFI;

const CIPHER_CHACHA20_POLY1305 = 0;   // smaller and faster without an AES instruction
const CIPHER_AES_256_GCM = 1;         // the NIST algorithm buyers ask for
const CIPHER_DEFAULT = CIPHER_AES_256_GCM;
const MAX_TOPIC = 64;
const MSG = 512;
const KINDS = ['none', 'call', 'reply', 'event', 'expired', 'dropped', 'flood', 'rollback'];
const DROPS = [1 => 'version', 'open', 'decode', 'duplicate', 'no_pending', 'topic', 'seq', 'id', 'replay', 'rate'];
const CODES = [-1 => 'invalid argument', -2 => 'full', -3 => 'too large', -4 => 'corrupt', -5 => 'bad memory', -6 => 'epoch not higher', -7 => 'unprovisioned (sender 0)', -8 => 'counter exhausted', -9 => "this second's sends spent", -10 => 'busy: another thread holds this peer', -99 => 'panic'];
const DECLS = <<<'C'
typedef struct { const uint8_t* ptr; size_t len; } mud_buf;
typedef struct { uint8_t* ptr; size_t len; } mud_mutbuf;
typedef struct { uint8_t bytes[12]; } mud_id;
typedef struct { mud_buf name; mud_buf key; uint32_t sender; uint32_t epoch; mud_buf seed; uint32_t budget; uint32_t hub; uint32_t cipher; } mud_config;
typedef struct { int32_t kind; mud_id id; mud_buf topic, method, reply_to, detail, bytes; uint32_t ttl_secs, seq, status; int32_t end; int32_t dropped; uint32_t sender; } mud_incoming;
typedef struct { int32_t kind; mud_buf topic, wire; uint32_t ttl_secs; } mud_out;
typedef struct mud_udp mud_udp;
size_t mud_size(void);
size_t mud_hub_size(void);
int32_t mud_new(void* mem, size_t len, const mud_config* config);
int32_t mud_save(void* mem, size_t len, void* out, size_t out_len);
int32_t mud_resume(void* mem, size_t len, uint32_t epoch);
int32_t mud_renew(void* mem, size_t len, uint32_t epoch);
int32_t mud_drop(void* mem, size_t len);
int32_t mud_stage(void* mem, size_t len, mud_buf key);
int32_t mud_rotate(void* mem, size_t len);
int32_t mud_retire(void* mem, size_t len);
int32_t mud_roster(void* mem, size_t len, uint32_t sender, mud_buf key, mud_buf next);
int32_t mud_revoke(void* mem, size_t len, uint32_t sender);
int32_t mud_public_key(mud_buf seed, uint8_t* out);
int32_t mud_opened(void* mem, size_t len);
int32_t mud_call(void* mem, size_t len, uint32_t now, mud_buf service, mud_buf method, mud_buf req, uint32_t ttl_secs, mud_id* out);
int32_t mud_publish(void* mem, size_t len, uint32_t now, mud_buf topic, mud_buf payload, uint32_t ttl_secs);
int32_t mud_listen(void* mem, size_t len, mud_buf topic);
int32_t mud_reply(void* mem, size_t len, uint32_t now, const mud_incoming* call, uint32_t seq, uint32_t status, mud_buf detail, mud_buf payload, int32_t end);
int32_t mud_deliver(void* mem, size_t len, uint32_t now, mud_buf topic, mud_buf wire, mud_mutbuf scratch, mud_incoming* out);
int32_t mud_tick(void* mem, size_t len, uint32_t now, mud_incoming* out);
int32_t mud_outgoing(void* mem, size_t len, uint32_t now, mud_mutbuf scratch, mud_out* out);
int32_t mud_sent(void* mem, size_t len);
uint32_t mud_pressure(void* mem, size_t len);
uint32_t mud_count(void* mem, size_t len, int32_t which);
uint32_t mud_pending(void* mem, size_t len);
uint32_t mud_queued(void* mem, size_t len);
int32_t mud_internet(void);
mud_udp* mud_udp_new(mud_buf bind, mud_buf peers, int32_t* code);
void mud_udp_free(mud_udp*);
int32_t mud_udp_port(mud_udp*);
int32_t mud_udp_listen(mud_udp*, mud_buf topic);
int32_t mud_udp_send(mud_udp*, mud_buf topic, mud_buf wire);
int32_t mud_udp_poll(mud_udp*, mud_mutbuf scratch, mud_buf* topic, mud_buf* wire);
C;

/** A code from the library, the same number in every binding: getCode() is it, -2 full, -3 too large, -4 corrupt. */
final class Error extends \RuntimeException
{
    public function __construct(int $code)
    {
        parent::__construct('mudRPC: ' . (CODES[$code] ?? (string) $code), $code);
    }
}

/** A socket used after its close(). */
final class Closed extends \RuntimeException
{
}

/** The library, loaded once from MUDRPC_LIB when set, else beside this file. */
final class Lib
{
    private static ?FFI $ffi = null;
    private static FFI\CData $arena;

    public static function get(): FFI
    {
        if (self::$ffi === null) {
            $name = ['Darwin' => 'libmudrpc.dylib', 'Windows' => 'mudrpc.dll'][PHP_OS_FAMILY] ?? 'libmudrpc.so';
            $path = getenv('MUDRPC_LIB') ?: __DIR__ . '/' . $name;
            if (!is_file($path)) {
                throw new \RuntimeException("libmudrpc not found at $path: build with `cargo build --release -p mudrpc-c`, then set MUDRPC_LIB or put it beside Mud.php");
            }
            self::$ffi = FFI::cdef(DECLS, $path);
            self::$arena = self::$ffi->new('uint8_t[4096]');
        }
        return self::$ffi;
    }

    /** Each string as a mud_buf in one arena, good until the next call here: an entry reads them before it returns. */
    public static function args(string ...$strings): array
    {
        $ffi = self::get();
        $out = [];
        $off = 0;
        foreach ($strings as $s) {
            $n = strlen($s);
            if ($off + $n >= 4096) {
                throw new Error(-3);
            }
            $b = $ffi->new('mud_buf');
            $b->ptr = FFI::addr(self::$arena[$off]);
            $b->len = $n;
            if ($n) {
                FFI::memcpy($b->ptr, $s, $n);
            }
            $out[] = $b;
            $off += $n;
        }
        return $out;
    }

    public static function check(int $code): int
    {
        if ($code < 0) {
            throw new Error($code);
        }
        return $code;
    }

    public static function str(FFI\CData $buf): string
    {
        return $buf->len ? FFI::string($buf->ptr, $buf->len) : '';
    }
}

/** A verdict from deliver() or tick(): kind call, reply, event, expired, dropped, flood or rollback; id is 12 bytes. */
final class Incoming
{
    public function __construct(public readonly string $kind, public readonly string $id = '', public readonly string $topic = '', public readonly string $method = '', public readonly string $replyTo = '', public readonly string $detail = '', public readonly string $bytes = '', public readonly int $ttl = 0, public readonly int $seq = 0, public readonly int $status = 0, public readonly bool $end = false, public readonly string $dropped = '', public readonly int $sender = 0)
    {
    }

    /** @internal */
    public static function read(FFI\CData $r): self
    {
        return new self(KINDS[$r->kind], FFI::string($r->id->bytes, 12), Lib::str($r->topic), Lib::str($r->method), Lib::str($r->reply_to), Lib::str($r->detail), Lib::str($r->bytes), $r->ttl_secs, $r->seq, $r->status, $r->end === 1, DROPS[$r->dropped] ?? '', $r->sender);
    }
}

/** An item for the transport: kind send, with topic, wire and ttl, or listen, with topic. */
final class Outgoing
{
    public function __construct(public readonly string $kind, public readonly string $topic, public readonly string $wire = '', public readonly int $ttl = 0)
    {
    }
}

/** A transport a channel pumps: send() takes each outgoing item, poll() gives the next [topic, wire] or null. */
interface Radio
{
    public function send(Outgoing $item): void;

    public function poll(): ?array;
}

/**
 * A mudRPC peer. $name is its inbox topic; $key the 32-byte fleet key; $sender the roster number (0 cannot send);
 * $epoch a number the host persists beside the key and raises on every boot and restore, written to storage first.
 *
 * A signed peer believes 16 senders; $hub is the hub shape, 256, for the host that believes a fleet. The line past either
 * is Error -2. The bytes from save() contain the key; store them as you store the key.
 */
final class Peer
{
    private ?FFI\CData $mem = null;
    private int $size;
    private FFI\CData $scratch;

    public function __construct(string $name, string $key, int $sender, int $epoch, ?string $seed = null, int $budget = 1000, bool $hub = false, int $cipher = CIPHER_DEFAULT)
    {
        $ffi = Lib::get();
        if (strlen($key) !== 32 || ($seed !== null && strlen($seed) !== 32)) {
            throw new \InvalidArgumentException('the key and the seed are 32 bytes');
        }
        $this->alloc($hub ? $ffi->mud_hub_size() : $ffi->mud_size());
        $cfg = $ffi->new('mud_config');
        [$cfg->name, $cfg->key, $cfg->seed] = Lib::args($name, $key, $seed ?? '');
        $cfg->sender = $sender;
        $cfg->epoch = $epoch;
        $cfg->budget = $budget;
        $cfg->hub = (int) $hub;
        $cfg->cipher = $cipher;
        Lib::check($ffi->mud_new($this->mem, $this->size, FFI::addr($cfg)));
    }

    /** A peer from save() bytes, from this same build of the library, with a higher epoch from the host. */
    public static function restore(string $saved, int $epoch): self
    {
        $ffi = Lib::get();
        if (!in_array(strlen($saved), [$ffi->mud_size(), $ffi->mud_hub_size()], true)) {
            throw new Error(-4);
        }
        $p = (new \ReflectionClass(self::class))->newInstanceWithoutConstructor();
        $p->alloc(strlen($saved));
        FFI::memcpy($p->mem, $saved, strlen($saved));
        Lib::check($ffi->mud_resume($p->mem, $p->size, $epoch));
        return $p;
    }

    private function alloc(int $size): void
    {
        $this->size = $size;
        $this->mem = Lib::get()->new('uint64_t[' . intdiv($size + 7, 8) . ']');   // 8-byte aligned, as mud_align() requires
        $this->scratch = Lib::get()->new('uint8_t[' . (MAX_TOPIC + MSG) . ']');
    }

    private function mem(): FFI\CData
    {
        return $this->mem ?? throw new Error(-4);
    }

    private function run(string $entry, mixed ...$args): int
    {
        return Lib::check(Lib::get()->$entry($this->mem(), $this->size, ...$args));
    }

    private function scratch(): FFI\CData
    {
        $b = Lib::get()->new('mud_mutbuf');
        $b->ptr = FFI::addr($this->scratch[0]);
        $b->len = MAX_TOPIC + MSG;
        return $b;
    }

    /** The peer's bytes, to write to flash. They contain the key, and cannot send until restore() has resumed them. */
    public function save(): string
    {
        $out = Lib::get()->new('uint64_t[' . intdiv($this->size + 7, 8) . ']');
        $this->run('mud_save', $out, $this->size);
        return FFI::string($out, $this->size);
    }

    /** Ends the peer: its block is zeroed, keys included. Every later call throws Error -4. */
    public function close(): void
    {
        if ($this->mem !== null) {
            Lib::get()->mud_drop($this->mem, $this->size);
            $this->mem = null;
        }
    }

    public function __destruct()
    {
        $this->close();
    }

    /** A higher epoch for a live peer; clears Error -8, counter exhausted. */
    public function renew(int $epoch): void
    {
        $this->run('mud_renew', $epoch);
    }

    /** The next fleet key, before the flag day: stage, then rotate, then retire once every peer has rotated. */
    public function stage(string $key): void
    {
        $this->run('mud_stage', ...Lib::args($key));
    }

    public function rotate(): void
    {
        $this->run('mud_rotate');
    }

    public function retire(): void
    {
        $this->run('mud_retire');
    }

    /** Signed peers: a line for $sender, its public key and the next one during a rotation. */
    public function roster(int $sender, string $key, string $next = ''): void
    {
        $this->run('mud_roster', $sender, ...Lib::args($key, $next));
    }

    public function revoke(int $sender): void
    {
        $this->run('mud_revoke', $sender);
    }

    /** The 32-byte public key of a seed, for rosters. */
    public static function publicKey(string $seed): string
    {
        $out = Lib::get()->new('uint8_t[32]');
        Lib::check(Lib::get()->mud_public_key(Lib::args($seed)[0], $out));
        return FFI::string($out, 32);
    }

    /** Which key slot opened the last accepted wire: current, previous or next. */
    public function opened(): string
    {
        return ['current', 'previous', 'next'][$this->run('mud_opened')];
    }

    /** The 12-byte id its replies will carry; the call expires after $ttl seconds. */
    public function call(int $now, string $service, string $method, string $req, int $ttl): string
    {
        $id = Lib::get()->new('mud_id');
        [$s, $m, $r] = Lib::args($service, $method, $req);
        $this->run('mud_call', $now, $s, $m, $r, $ttl, FFI::addr($id));
        return FFI::string($id->bytes, 12);
    }

    public function publish(int $now, string $topic, string $payload, int $ttl): void
    {
        [$t, $p] = Lib::args($topic, $payload);
        $this->run('mud_publish', $now, $t, $p, $ttl);
    }

    public function listen(string $topic): void
    {
        $this->run('mud_listen', ...Lib::args($topic));
    }

    /** Answers a call as deliver() gave it: $status gRPC's number; $seq 0..63 and $end once, on the last item. */
    public function reply(int $now, Incoming $call, int $status, string $payload, bool $end = true, int $seq = 0, string $detail = ''): void
    {
        if ($call->kind !== 'call') {
            throw new Error(-1);
        }
        $c = Lib::get()->new('mud_incoming');
        [$c->topic, $c->method, $c->reply_to, $c->bytes, $d, $p] = Lib::args($call->topic, $call->method, $call->replyTo, $call->bytes, $detail, $payload);
        $c->kind = 1;
        $id = $call->id;   // memcpy takes its source by reference, which a readonly property is not
        FFI::memcpy($c->id->bytes, $id, 12);
        $c->ttl_secs = $call->ttl;
        $c->seq = $call->seq;
        $c->end = (int) $call->end;
        $this->run('mud_reply', $now, FFI::addr($c), $seq, $status, $d, $p, (int) $end);
    }

    /** Every wire gets a verdict; a dropped Incoming names why. */
    public function deliver(int $now, string $topic, string $wire): Incoming
    {
        $out = Lib::get()->new('mud_incoming');
        [$t, $w] = Lib::args($topic, $wire);
        $this->run('mud_deliver', $now, $t, $w, $this->scratch(), FFI::addr($out));
        return Incoming::read($out);
    }

    public function tick(int $now): ?Incoming
    {
        $out = Lib::get()->new('mud_incoming');
        return $this->run('mud_tick', $now, FFI::addr($out)) ? Incoming::read($out) : null;
    }

    /** The next item for the transport, left in place until sent(). */
    public function outgoing(int $now): ?Outgoing
    {
        $out = Lib::get()->new('mud_out');
        $kind = $this->run('mud_outgoing', $now, $this->scratch(), FFI::addr($out));
        return $kind ? new Outgoing($kind === 1 ? 'send' : 'listen', Lib::str($out->topic), Lib::str($out->wire), $out->ttl_secs) : null;
    }

    public function sent(): void
    {
        $this->run('mud_sent');
    }

    public function pressure(): int
    {
        return $this->mem ? Lib::get()->mud_pressure($this->mem, $this->size) : 0;
    }

    /** Calls in flight, of 16: at 16 a call is Error -2. */
    public function pending(): int
    {
        return $this->mem ? Lib::get()->mud_pending($this->mem, $this->size) : 0;
    }

    /** Items queued for the transport, of 16: at 16 every producer is Error -2. */
    public function queued(): int
    {
        return $this->mem ? Lib::get()->mud_queued($this->mem, $this->size) : 0;
    }

    /** The operator's counters, kept across restores: managed, rollbacks, floods, dropped. */
    public function counts(): array
    {
        $c = [];
        foreach (['managed', 'rollbacks', 'floods', 'dropped'] as $i => $k) {
            $c[$k] = $this->mem ? Lib::get()->mud_count($this->mem, $this->size, $i) : 0;
        }
        return $c;
    }
}

/**
 * A product's UDP: each wire to every listed peer, on private addresses alone unless the library was built with the
 * cargo feature `internet`, which no release is. Binds "ip:port" and sends to each peer's, at least one; a bad or public
 * address, or no peer, is Error -1 ("0.0.0.0" is public), a bind the system refuses -2.
 */
final class Udp implements Radio
{
    private ?FFI\CData $udp;
    private FFI\CData $scratch;

    public function __construct(string $bind, string ...$peers)
    {
        $ffi = Lib::get();
        $code = $ffi->new('int32_t');
        [$b, $p] = Lib::args($bind, implode(' ', $peers));
        $this->udp = $ffi->mud_udp_new($b, $p, FFI::addr($code));
        if ($this->udp === null) {
            throw new Error($code->cdata);
        }
        $this->scratch = $ffi->new('uint8_t[' . (MAX_TOPIC + MSG) . ']');
    }

    /** Whether this library may bind or send to a public address. */
    public static function internet(): bool
    {
        return Lib::get()->mud_internet() === 1;
    }

    private function live(): FFI\CData
    {
        return $this->udp ?? throw new Closed('the socket is closed');
    }

    /** The bound port, the one chosen for port 0. */
    public function port(): int
    {
        return Lib::check(Lib::get()->mud_udp_port($this->live()));
    }

    /** A listen keeps wires on its topic from then, every other topic dropped unread; a send throws Error -2 when no peer took the wire, so the item stays queued. */
    public function send(Outgoing $item): void
    {
        [$t, $w] = Lib::args($item->topic, $item->wire);
        Lib::check($item->kind === 'listen' ? Lib::get()->mud_udp_listen($this->live(), $t) : Lib::get()->mud_udp_send($this->live(), $t, $w));
    }

    /** The next [topic, wire] from a listed peer, or null. */
    public function poll(): ?array
    {
        $ffi = Lib::get();
        $t = $ffi->new('mud_buf');
        $w = $ffi->new('mud_buf');
        $s = $ffi->new('mud_mutbuf');
        $s->ptr = FFI::addr($this->scratch[0]);
        $s->len = MAX_TOPIC + MSG;
        return Lib::check($ffi->mud_udp_poll($this->live(), $s, FFI::addr($t), FFI::addr($w))) === 1 ? [Lib::str($t), Lib::str($w)] : null;
    }

    public function close(): void
    {
        if ($this->udp !== null) {
            Lib::get()->mud_udp_free($this->udp);
            $this->udp = null;
        }
    }

    public function __destruct()
    {
        $this->close();
    }
}
