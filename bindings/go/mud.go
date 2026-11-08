// mudRPC for Go: one package over the C ABI through cgo, with no dependency beyond the standard library; mud_grpc.go
// beside it is the drop-in for gRPC. Copy the folder into your module as package mud, mudrpc.h beside it, and link the
// library: CGO_LDFLAGS="-L<dir>" and, for the final link alone, go build -ldflags="-extldflags=-Wl,-rpath,<dir>".
//
// A call is a message; a reply may come hours later or expire; nothing blocks. You poll: drain Outgoing into your
// transport and call Sent, feed what arrives to Deliver, call Tick. now is your uptime in seconds.
package mud

/*
#cgo CFLAGS: -I${SRCDIR} -I${SRCDIR}/../c
#cgo LDFLAGS: -lmudrpc
#include <stdlib.h>
#include <string.h>
#include "mudrpc.h"
*/
import "C"

import (
	"errors"
	"fmt"
	"runtime"
	"strings"
	"sync"
	"unsafe"
)

const MaxTopic, Msg = 64, 512

// Cipher is the fleet's: the zero value is the default, AES-256-GCM, the NIST algorithm buyers ask for; ChaCha20-Poly1305
// is smaller and faster without an AES instruction. A fleet runs one: a peer drops the other's wires by version.
type Cipher int32

const (
	CipherDefault Cipher = iota
	CipherAES256GCM
	CipherChaCha20Poly1305
)

// Kind is a verdict's, from Deliver or Tick.
type Kind int32

const (
	None     Kind = iota // nothing: no verdict, or nothing queued
	Call                 // a call for this peer to answer with Reply
	Reply                // a reply to a call of ours; End closes the stream
	Event                // a publish on a topic this peer listens to
	Expired              // a call of ours whose ttl passed with the stream open
	Dropped              // a wire refused: Dropped and Status say why
	Flood                // a sender over its share of a second, reported once a second
	Rollback             // a sender whose epoch went back: a cloned or rolled-back device
)

var kinds = [...]string{"none", "call", "reply", "event", "expired", "dropped", "flood", "rollback"}

func (k Kind) String() string {
	if int(k) < len(kinds) {
		return kinds[k]
	}
	return fmt.Sprint(int32(k))
}

// Error is a code from mudrpc.h, the same number in every binding.
type Error struct{ Code int32 }

var names = map[int32]string{-1: "invalid argument", -2: "full", -3: "too large", -4: "corrupt", -5: "bad memory", -6: "epoch not higher", -7: "unprovisioned (sender 0)", -8: "counter exhausted", -9: "this second's sends spent", -10: "busy: another thread holds this peer", -99: "panic"}

func (e *Error) Error() string {
	if s, ok := names[e.Code]; ok {
		return "mudRPC: " + s
	}
	return fmt.Sprintf("mudRPC: %d", e.Code)
}

// Incoming is a verdict; fields a kind does not use are zero or empty.
type Incoming struct {
	Kind                           Kind
	Id                             [12]byte // the call's nonce: sender ‖ epoch ‖ counter, on Call, Reply and Expired
	Topic, Method, ReplyTo, Detail string
	Bytes                          []byte
	TTL, Seq, Status               uint32 // TTL a call's; Seq a call's or a reply's stream; Status a reply's, gRPC's number, or the detail of a drop
	End                            bool   // the last of a call's or a reply's stream
	Dropped                        int32  // 1 version, 2 open, 3 decode, 4 duplicate, 5 no pending call, 6 topic, 7 seq, 8 id, 9 replay, 10 rate
	Sender                         uint32 // the sender's roster number: what it wrote, or proven under a signed fleet
}

// Outgoing is an item for the transport: a wire to send on Topic, kept for TTL seconds, or a topic to Listen to.
type Outgoing struct {
	Listen bool
	Topic  string
	Wire   []byte
	TTL    uint32
}

// Config is what New builds a peer from beyond its name, key, sender and epoch. Seed is empty for an unsigned peer, this
// sender's 32-byte Ed25519 seed for a signed one; Budget the signature checks per second a signed receiver affords,
// 1000 when 0; Hub believes 256 senders in place of 16, a seed required.
type Config struct {
	Seed   []byte
	Budget uint32
	Hub    bool
	Cipher Cipher
}

// Peer is one mudRPC peer in a block the library owns until Close. Its methods serialize on a mutex, so any goroutine may
// call them, one at a time; the drop-in's channel owns its peer and lends it out through Use.
type Peer struct {
	mu      sync.Mutex
	mem     unsafe.Pointer
	size    C.size_t
	scratch unsafe.Pointer
}

func buf(pin *runtime.Pinner, b []byte) C.mud_buf {
	if len(b) == 0 {
		return C.mud_buf{}
	}
	p := unsafe.SliceData(b)
	pin.Pin(p)
	return C.mud_buf{ptr: (*C.uint8_t)(unsafe.Pointer(p)), len: C.size_t(len(b))}
}

func text(b C.mud_buf) string  { return C.GoStringN((*C.char)(unsafe.Pointer(b.ptr)), C.int(b.len)) }
func bytes(b C.mud_buf) []byte { return C.GoBytes(unsafe.Pointer(b.ptr), C.int(b.len)) }

func check(code C.int32_t) error {
	if code < 0 {
		return &Error{int32(code)}
	}
	return nil
}

// New builds a peer: name its inbox topic, 1 to 64 bytes; key the fleet key, 32 bytes and not 32 equal ones; sender its
// roster number, 0 cannot send; epoch persisted beside the key, written first and higher every boot.
func New(name string, key []byte, sender, epoch uint32, cfg ...Config) (*Peer, error) {
	c := Config{}
	if len(cfg) > 0 {
		c = cfg[0]
	}
	size := C.mud_size()
	if c.Hub {
		size = C.mud_hub_size()
	}
	p := alloc(size)
	var pin runtime.Pinner
	defer pin.Unpin()
	hub, cipher, budget := C.uint32_t(0), C.uint32_t(C.MUD_CIPHER_AES_256_GCM), c.Budget
	if c.Hub {
		hub = 1
	}
	if c.Cipher == CipherChaCha20Poly1305 {
		cipher = C.MUD_CIPHER_CHACHA20_POLY1305
	}
	if budget == 0 {
		budget = 1000
	}
	conf := C.mud_config{name: buf(&pin, []byte(name)), key: buf(&pin, key), sender: C.uint32_t(sender), epoch: C.uint32_t(epoch), seed: buf(&pin, c.Seed), budget: C.uint32_t(budget), hub: hub, cipher: cipher}
	if err := check(C.mud_new(p.mem, p.size, &conf)); err != nil {
		p.free()
		return nil, err
	}
	return p, nil
}

// Restore rebuilds a peer from the bytes Save gave, at a higher epoch written to storage first; bytes of neither shape's
// size are -4 before the library is touched.
func Restore(saved []byte, epoch uint32) (*Peer, error) {
	if n := C.size_t(len(saved)); n != C.mud_size() && n != C.mud_hub_size() {
		return nil, &Error{-4}
	}
	p := alloc(C.size_t(len(saved)))
	C.memcpy(p.mem, unsafe.Pointer(unsafe.SliceData(saved)), p.size)
	if err := check(C.mud_resume(p.mem, p.size, C.uint32_t(epoch))); err != nil {
		p.free()
		return nil, err
	}
	return p, nil
}

func alloc(size C.size_t) *Peer {
	p := &Peer{size: size}
	if C.posix_memalign(&p.mem, C.mud_align(), size) != 0 || C.posix_memalign(&p.scratch, 16, MaxTopic+Msg) != 0 {
		panic("mudRPC: out of memory")
	}
	return p
}

func (p *Peer) free() {
	C.free(p.mem)
	C.free(p.scratch)
	p.mem = nil
}

// do runs one entry on the live block; a closed peer is -4 at every one.
func (p *Peer) do(f func() C.int32_t) (int32, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.mem == nil {
		return 0, &Error{-4}
	}
	r := f()
	return int32(r), check(r)
}

func (p *Peer) entry(f func() C.int32_t) error { _, err := p.do(f); return err }

// Save gives the peer's bytes for storage: they contain the key, so store them as the key; the copy cannot send until Restore.
func (p *Peer) Save() ([]byte, error) {
	var out unsafe.Pointer
	if C.posix_memalign(&out, C.mud_align(), p.size) != 0 {
		panic("mudRPC: out of memory")
	}
	defer C.free(out)
	if err := p.entry(func() C.int32_t { return C.mud_save(p.mem, p.size, out, p.size) }); err != nil {
		return nil, err
	}
	return C.GoBytes(out, C.int(p.size)), nil
}

// Renew gives a live peer a higher epoch; clears -8.
func (p *Peer) Renew(epoch uint32) error {
	return p.entry(func() C.int32_t { return C.mud_renew(p.mem, p.size, C.uint32_t(epoch)) })
}

// Close ends the peer: the block is zeroed, keys included, and freed; every later call is -4.
func (p *Peer) Close() {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.mem != nil {
		C.mud_drop(p.mem, p.size)
		p.free()
	}
}

// Stage sets the next fleet key, the first of a rotation's three steps: Stage on every peer, Rotate on every peer, Retire.
func (p *Peer) Stage(key []byte) error {
	var pin runtime.Pinner
	defer pin.Unpin()
	return p.entry(func() C.int32_t { return C.mud_stage(p.mem, p.size, buf(&pin, key)) })
}

func (p *Peer) Rotate() error {
	return p.entry(func() C.int32_t { return C.mud_rotate(p.mem, p.size) })
}
func (p *Peer) Retire() error {
	return p.entry(func() C.int32_t { return C.mud_retire(p.mem, p.size) })
}

// Roster adds a signed peer's line: sender, its public key and the next one during a rotation, nil otherwise.
func (p *Peer) Roster(sender uint32, key, next []byte) error {
	var pin runtime.Pinner
	defer pin.Unpin()
	return p.entry(func() C.int32_t {
		return C.mud_roster(p.mem, p.size, C.uint32_t(sender), buf(&pin, key), buf(&pin, next))
	})
}

func (p *Peer) Revoke(sender uint32) error {
	return p.entry(func() C.int32_t { return C.mud_revoke(p.mem, p.size, C.uint32_t(sender)) })
}

// PublicKey is a seed's, for building rosters.
func PublicKey(seed []byte) ([32]byte, error) {
	var pin runtime.Pinner
	defer pin.Unpin()
	var out [32]byte
	err := check(C.mud_public_key(buf(&pin, seed), (*C.uint8_t)(unsafe.Pointer(&out[0]))))
	return out, err
}

// Opened is the key slot that opened the last accepted wire: 0 current, 1 previous, 2 next.
func (p *Peer) Opened() (int, error) {
	r, err := p.do(func() C.int32_t { return C.mud_opened(p.mem, p.size) })
	return int(r), err
}

// Call sends a request to service's method and returns the id its replies carry; the call expires after ttl seconds.
func (p *Peer) Call(now uint32, service, method string, req []byte, ttl uint32) ([12]byte, error) {
	var pin runtime.Pinner
	defer pin.Unpin()
	var id C.mud_id
	err := p.entry(func() C.int32_t {
		return C.mud_call(p.mem, p.size, C.uint32_t(now), buf(&pin, []byte(service)), buf(&pin, []byte(method)), buf(&pin, req), C.uint32_t(ttl), &id)
	})
	return *(*[12]byte)(unsafe.Pointer(&id)), err
}

// Publish sends payload to every listener on topic, with no reply; the mesh keeps it for ttl seconds.
func (p *Peer) Publish(now uint32, topic string, payload []byte, ttl uint32) error {
	var pin runtime.Pinner
	defer pin.Unpin()
	return p.entry(func() C.int32_t {
		return C.mud_publish(p.mem, p.size, C.uint32_t(now), buf(&pin, []byte(topic)), buf(&pin, payload), C.uint32_t(ttl))
	})
}

// Listen subscribes to a topic: its events arrive as Event once Outgoing has handed the transport the listen item.
func (p *Peer) Listen(topic string) error {
	var pin runtime.Pinner
	defer pin.Unpin()
	return p.entry(func() C.int32_t { return C.mud_listen(p.mem, p.size, buf(&pin, []byte(topic))) })
}

// Reply answers a call with one OK item that ends it: a unary reply.
func (p *Peer) Reply(now uint32, call *Incoming, payload []byte) error {
	return p.ReplyItem(now, call, 0, 0, "", payload, true)
}

// ReplyItem answers a call as Deliver gave it: seq 0..63, status gRPC's number and detail its text, end once, on the last
// item and on a unary reply. The caller completes when every seq up to the lowest end it has seen has arrived.
func (p *Peer) ReplyItem(now uint32, call *Incoming, seq, status uint32, detail string, payload []byte, end bool) error {
	var pin runtime.Pinner
	defer pin.Unpin()
	in := C.mud_incoming{kind: C.int32_t(call.Kind), topic: buf(&pin, []byte(call.Topic)), method: buf(&pin, []byte(call.Method)), reply_to: buf(&pin, []byte(call.ReplyTo)), detail: buf(&pin, []byte(call.Detail)), bytes: buf(&pin, call.Bytes), ttl_secs: C.uint32_t(call.TTL), seq: C.uint32_t(call.Seq), status: C.uint32_t(call.Status), dropped: C.int32_t(call.Dropped), sender: C.uint32_t(call.Sender)}
	*(*[12]byte)(unsafe.Pointer(&in.id)) = call.Id
	if call.End {
		in.end = 1
	}
	e := C.int32_t(0)
	if end {
		e = 1
	}
	return p.entry(func() C.int32_t {
		return C.mud_reply(p.mem, p.size, C.uint32_t(now), &in, C.uint32_t(seq), C.uint32_t(status), buf(&pin, []byte(detail)), buf(&pin, payload), e)
	})
}

func (p *Peer) read(out *C.mud_incoming) *Incoming {
	in := &Incoming{Kind: Kind(out.kind), Topic: text(out.topic), Method: text(out.method), ReplyTo: text(out.reply_to), Detail: text(out.detail), Bytes: bytes(out.bytes), TTL: uint32(out.ttl_secs), Seq: uint32(out.seq), Status: uint32(out.status), End: out.end != 0, Dropped: int32(out.dropped), Sender: uint32(out.sender)}
	in.Id = *(*[12]byte)(unsafe.Pointer(&out.id))
	return in
}

// verdict runs an entry that fills a mud_incoming, and copies the verdict out of the scratch before the lock is let go.
func (p *Peer) verdict(f func(out *C.mud_incoming) C.int32_t) (in *Incoming, err error) {
	_, err = p.do(func() C.int32_t {
		var out C.mud_incoming
		k := f(&out)
		if k > 0 {
			in = p.read(&out)
		}
		return k
	})
	return in, err
}

// Deliver hands the peer a wire the transport received on topic. It never fails for bad bytes: those are a Dropped verdict with a reason.
func (p *Peer) Deliver(now uint32, topic string, wire []byte) (*Incoming, error) {
	var pin runtime.Pinner
	defer pin.Unpin()
	return p.verdict(func(out *C.mud_incoming) C.int32_t {
		return C.mud_deliver(p.mem, p.size, C.uint32_t(now), buf(&pin, []byte(topic)), buf(&pin, wire), C.mud_mutbuf{ptr: (*C.uint8_t)(p.scratch), len: MaxTopic + Msg}, out)
	})
}

// Tick reports the next call of ours whose ttl passed, or nil.
func (p *Peer) Tick(now uint32) (*Incoming, error) {
	return p.verdict(func(out *C.mud_incoming) C.int32_t { return C.mud_tick(p.mem, p.size, C.uint32_t(now), out) })
}

// Outgoing shows the next item for the transport, or nil; Sent takes it once the transport has.
func (p *Peer) Outgoing(now uint32) (o *Outgoing, err error) {
	_, err = p.do(func() C.int32_t {
		var out C.mud_out
		k := C.mud_outgoing(p.mem, p.size, C.uint32_t(now), C.mud_mutbuf{ptr: (*C.uint8_t)(p.scratch), len: MaxTopic + Msg}, &out)
		if k > 0 {
			o = &Outgoing{Listen: out.kind == C.MUD_LISTEN, Topic: text(out.topic), Wire: bytes(out.wire), TTL: uint32(out.ttl_secs)}
		}
		return k
	})
	return o, err
}

func (p *Peer) Sent() error { return p.entry(func() C.int32_t { return C.mud_sent(p.mem, p.size) }) }

func (p *Peer) gauge(f func() C.uint32_t) uint32 {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.mem == nil {
		return 0
	}
	return uint32(f())
}

// Pressure counts live entries evicted for want of room: rising under load means replay protection is degraded.
func (p *Peer) Pressure() uint32 {
	return p.gauge(func() C.uint32_t { return C.mud_pressure(p.mem, p.size) })
}

// Pending is the calls in flight, of 16; Queued the items waiting for the transport, of 16.
func (p *Peer) Pending() uint32 {
	return p.gauge(func() C.uint32_t { return C.mud_pending(p.mem, p.size) })
}
func (p *Peer) Queued() uint32 {
	return p.gauge(func() C.uint32_t { return C.mud_queued(p.mem, p.size) })
}

// Counts are the operator's counters, kept across restores: Managed rising when nobody ordered a change is something on the host rekeying the unit.
type Counts struct{ Managed, Rollbacks, Floods, Dropped uint32 }

func (p *Peer) Counts() Counts {
	n := func(which C.int32_t) uint32 {
		return p.gauge(func() C.uint32_t { return C.mud_count(p.mem, p.size, which) })
	}
	return Counts{n(0), n(1), n(2), n(3)}
}

// Udp is a product's UDP: each wire to every listed peer, on private addresses alone unless the library was built with
// the cargo feature `internet`, which no release is. Its methods serialize on a mutex, so a channel's pump and the
// owner's Close never meet inside the library.
type Udp struct {
	mu      sync.Mutex
	udp     *C.mud_udp
	scratch unsafe.Pointer
}

// ErrClosed is every call on a closed Udp.
var ErrClosed = errors.New("the socket is closed")

// NewUdp binds "ip:port" and sends to each peer's, at least one; a bad or public address, or no peer, is -1 ("0.0.0.0"
// is public), a bind the system refuses -2.
func NewUdp(bind string, peers ...string) (*Udp, error) {
	var pin runtime.Pinner
	defer pin.Unpin()
	var code C.int32_t
	u := &Udp{udp: C.mud_udp_new(buf(&pin, []byte(bind)), buf(&pin, []byte(strings.Join(peers, " "))), &code)}
	if u.udp == nil {
		return nil, check(code)
	}
	if C.posix_memalign(&u.scratch, 16, MaxTopic+Msg) != 0 {
		panic("mudRPC: out of memory")
	}
	return u, nil
}

// Internet is whether this library may bind or send to a public address.
func Internet() bool { return C.mud_internet() == 1 }

func (u *Udp) do(f func() C.int32_t) (int32, error) {
	u.mu.Lock()
	defer u.mu.Unlock()
	if u.udp == nil {
		return 0, ErrClosed
	}
	r := f()
	return int32(r), check(r)
}

// Port is the bound port, the one chosen for port 0.
func (u *Udp) Port() (int, error) {
	r, err := u.do(func() C.int32_t { return C.mud_udp_port(u.udp) })
	return int(r), err
}

// Send takes one outgoing item: a listen keeps wires on its topic from then, every other topic dropped unread; a send
// fails -2 when no peer took the wire, so the item stays queued.
func (u *Udp) Send(o *Outgoing) error {
	var pin runtime.Pinner
	defer pin.Unpin()
	_, err := u.do(func() C.int32_t {
		if o.Listen {
			return C.mud_udp_listen(u.udp, buf(&pin, []byte(o.Topic)))
		}
		return C.mud_udp_send(u.udp, buf(&pin, []byte(o.Topic)), buf(&pin, o.Wire))
	})
	return err
}

// Poll gives the next wire from a listed peer on a topic listened to; wire is nil when none waits.
func (u *Udp) Poll() (topic string, wire []byte, err error) {
	_, err = u.do(func() C.int32_t {
		var t, w C.mud_buf
		k := C.mud_udp_poll(u.udp, C.mud_mutbuf{ptr: (*C.uint8_t)(u.scratch), len: MaxTopic + Msg}, &t, &w)
		if k == 1 {
			topic, wire = text(t), bytes(w)
		}
		return k
	})
	return topic, wire, err
}

func (u *Udp) Close() {
	u.mu.Lock()
	defer u.mu.Unlock()
	if u.udp != nil {
		C.mud_udp_free(u.udp)
		C.free(u.scratch)
		u.udp = nil
	}
}
