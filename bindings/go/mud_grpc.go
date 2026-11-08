// mudRPC as gRPC's transport in Go: the client and server code protoc-gen-go-grpc generates from a .proto run over mudRPC
// unchanged; building the channel is the one line that differs. Beside mud.go, with google.golang.org/grpc, whose
// ClientConnInterface, ClientStream, ServiceRegistrar and ServerStream this implements (grpc-go, Copyright gRPC authors,
// Apache License 2.0), and protobuf's proto package for the messages.
//
// Not a drop-in where gRPC needs a connection: a message of about 460 bytes, a service's full name of at most 64 bytes, no
// metadata, unary and server-streaming calls only, at most 63 items a stream, the first reply wins, and 16 calls in
// flight. A server sends 32 replies a second, half what a receiver takes from one sender, its callers in turn, and a
// client keeps under 32 calls a second. A deadline is the call's ttl, a week at most; a call cancelled after it was sent
// keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
package mud

import (
	"context"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"reflect"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unicode/utf8"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/peer"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"
)

// StreamEnd is the detail of the reply that ends a server stream and carries no item.
const StreamEnd = "end"

const (
	week, maxItems, inbox, pace, workers, detailBytes = 604800, 63, 1024, 32, 64, 128 // a message's longest life, a stream's items, wires or calls waiting, replies a second, handlers running, a detail's bytes
	patience                                          = time.Second                   // how long a refused item is tried
)

// Radio takes each outgoing item: a *Udp, which the channel also reads, or any transport, which hands what it receives to
// Channel.Deliver. Send runs on the pump's goroutine with no lock held, so it may wait on one that calls into the channel.
type Radio interface{ Send(*Outgoing) error }

// RadioFunc is a Radio from a function.
type RadioFunc func(*Outgoing) error

func (f RadioFunc) Send(o *Outgoing) error { return f(o) }

// Option configures a channel: TTL, the life in seconds of a call made with no deadline, 60 by default; Fault, run on
// the pump, hears every error the radio returns and every listen the peer refuses, writing to stderr by default.
type Option func(*Channel)

func TTL(secs uint32) Option     { return func(c *Channel) { c.ttl = secs } }
func Fault(f func(error)) Option { return func(c *Channel) { c.fault = f } }

type wire struct {
	topic string
	wire  []byte
}

type service struct {
	desc *grpc.ServiceDesc
	impl any
}

type answer struct {
	sc          *serverCall
	seq, status uint32
	detail      string
	payload     []byte
	end         bool
}

// Channel is a gRPC client connection over one peer, and the server for the services registered on it: it pumps on a
// goroutine of its own. The peer is the channel's while it lives: reach it only through Use, and Close the channel
// before the peer.
type Channel struct {
	peer                   *Peer
	udp                    *Udp
	send                   func(*Outgoing) error
	fault                  func(error)
	ttl                    uint32
	start                  time.Time
	sem                    chan struct{}
	wake, done, stopped    chan struct{}
	closing, inPump, inUse atomic.Bool

	mail   sync.Mutex
	inbox  []wire
	faults []error // heard under the gate, told after the round

	gate     sync.Mutex // the peer's and every table's below; no handler, caller or codec runs while it is held, and the radio's Send runs without it
	closed   bool
	pending  map[[12]byte]*call
	services map[string]service
	toListen []string
	lines    map[uint32][]answer // by caller, the next to send first
	turn     []uint32            // the callers owed, the next to send first
	serving  map[*serverCall]struct{}
	waiting  int
	stopAt   time.Time
	stopDone bool
	paceSec  uint32
	paced    int
	refused  time.Time
}

// NewChannel starts the pump over peer and radio; over the library's Udp it listens on the peer's name before it returns.
func NewChannel(peer *Peer, radio Radio, opts ...Option) *Channel {
	c := &Channel{peer: peer, send: radio.Send, fault: func(err error) { fmt.Fprintln(os.Stderr, "mudrpc:", err) }, ttl: 60, start: time.Now(), sem: make(chan struct{}, workers), wake: make(chan struct{}, 1), done: make(chan struct{}), stopped: make(chan struct{}), pending: map[[12]byte]*call{}, services: map[string]service{}, lines: map[uint32][]answer{}, serving: map[*serverCall]struct{}{}}
	c.udp, _ = radio.(*Udp)
	for _, o := range opts {
		o(c)
	}
	c.ttl = max(1, min(c.ttl, week))
	c.listen()
	go c.run()
	return c
}

// listen, under the gate or before the pump, puts the peer's listens on the library's Udp before the caller goes on, since
// it drops wires on a topic not yet listened to: the first wire, and any error, waits for the pump.
func (c *Channel) listen() {
	if c.udp == nil {
		return
	}
	for len(c.toListen) > 0 && c.peer.Listen(c.toListen[0]) == nil {
		c.toListen = c.toListen[1:]
	}
	for {
		o, err := c.peer.Outgoing(c.now())
		if err != nil || o == nil || !o.Listen || c.udp.Send(o) != nil || c.peer.Sent() != nil {
			return
		}
	}
}

// Deliver hands the channel a wire the radio received, from any goroutine. Past 1024 waiting for the pump, a wire is dropped, as core would drop a flood.
func (c *Channel) Deliver(topic string, w []byte) {
	c.mail.Lock()
	defer c.mail.Unlock()
	if c.closing.Load() || len(c.inbox) >= inbox {
		return
	}
	c.inbox = append(c.inbox, wire{topic, append([]byte(nil), w...)})
	c.poke()
}

// Use lends the peer out between rounds of the pump: Save, Stage, Rotate, Roster, Counts. f must not call the channel, but for Close.
func (c *Channel) Use(f func(*Peer)) {
	c.gate.Lock()
	defer c.gate.Unlock()
	defer c.poke()
	c.inUse.Store(true)
	defer c.inUse.Store(false)
	f(c.peer)
}

// Close stops the pump and ends every call in flight with Unavailable; from any goroutine, a handler, Use or the radio's Send included.
func (c *Channel) Close() {
	c.closing.Store(true)
	c.poke()
	if !c.inPump.Load() && !c.inUse.Load() { // the pump's own goroutine, in the radio or the fault function, and a Use cannot wait for the pump
		<-c.done
	}
}

func (c *Channel) poke() {
	select {
	case c.wake <- struct{}{}:
	default:
	}
}

func (c *Channel) now() uint32 { return uint32(time.Since(c.start) / time.Second) }

// tell, under the gate, keeps a fault for the fault function, which runs after the round with no lock held.
func (c *Channel) tell(err error) { c.faults = append(c.faults, err) }

func (c *Channel) run() {
	defer close(c.done)
	for over := false; !over; {
		wait := 50 * time.Millisecond
		if c.udp != nil {
			wait = 10 * time.Millisecond
		}
		select {
		case <-c.wake:
		case <-time.After(wait):
		}
		over = c.round()
		c.gate.Lock()
		faults := c.faults
		c.faults = nil
		c.gate.Unlock()
		for _, err := range faults {
			c.try(func() error { c.fault(err); return nil })
		}
	}
}

// lost is the peer closed under the channel: every call is Unavailable and the pump ends.
func (c *Channel) lost(err error) bool {
	var e *Error
	if errors.As(err, &e) && (e.Code == -4 || e.Code == -5) {
		c.shut(codes.Unavailable, "the peer is closed: "+err.Error())
		return true
	}
	return false
}

func (c *Channel) round() bool {
	c.gate.Lock()
	defer c.gate.Unlock()
	if c.closing.Load() {
		c.shut(codes.Unavailable, "the channel is closed")
	}
	if c.closed {
		return true
	}
	now := c.now()
	if !c.stopAt.IsZero() && !time.Now().Before(c.stopAt) {
		for sc := range c.serving {
			c.kill(sc, codes.Canceled, "the server shut down")
		}
	}
	for len(c.toListen) > 0 {
		if err := c.peer.Listen(c.toListen[0]); err != nil {
			if c.lost(err) {
				return true
			}
			break // the outbox is full: the next round listens
		}
		c.toListen = c.toListen[1:]
	}
	c.mail.Lock()
	in := c.inbox
	c.inbox = nil
	c.mail.Unlock()
	for _, w := range in {
		e, err := c.peer.Deliver(now, w.topic, w.wire) // an error is a wire the transport mangled: no verdict to act on
		if c.lost(err) {
			return true
		}
		c.land(e)
	}
	for i := 0; c.udp != nil && i < inbox; i++ {
		topic, w, err := c.udp.Poll()
		if errors.Is(err, ErrClosed) {
			c.shut(codes.Unavailable, "the socket is closed")
			return true
		}
		if err != nil {
			c.tell(err)
			break
		}
		if w == nil {
			break
		}
		e, err := c.peer.Deliver(now, topic, w)
		if c.lost(err) {
			return true
		}
		c.land(e)
	}
	for {
		e, err := c.peer.Tick(now)
		if c.lost(err) {
			return true
		}
		if e == nil {
			break
		}
		c.land(e)
	}
	c.answer(now)
	for {
		o, err := c.peer.Outgoing(now)
		if c.lost(err) {
			return true
		}
		if o == nil {
			break
		}
		if c.udp == nil { // a radio's Send runs with no lock held; the library's socket never calls back in, and under the gate no listen drained by RegisterService can pop the item shown here
			c.gate.Unlock()
		}
		err = c.try(func() error { return c.send(o) })
		if c.udp == nil {
			c.gate.Lock()
		}
		if c.closing.Load() {
			c.shut(codes.Unavailable, "the channel is closed")
			return true
		}
		if err != nil {
			if c.udp != nil && errors.Is(err, ErrClosed) {
				c.shut(codes.Unavailable, "the socket is closed")
				return true
			}
			if c.refused.IsZero() {
				c.refused = time.Now()
				c.tell(err)
			}
			if time.Since(c.refused) < patience {
				break // the next round tries it again; past a second it is let go
			}
		}
		c.refused = time.Time{}
		if c.lost(c.peer.Sent()) {
			return true
		}
	}
	return false
}

// try runs the radio's Send or the fault function on the pump with no lock held; a panic in it is its error.
func (c *Channel) try(f func() error) (err error) {
	c.inPump.Store(true)
	defer c.inPump.Store(false)
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("the radio panicked: %v", r)
		}
	}()
	return f()
}

func (c *Channel) shut(code codes.Code, msg string) {
	if c.closed {
		return
	}
	c.closed = true
	c.closing.Store(true)
	for _, k := range c.pending {
		k.abort(status.New(code, msg))
	}
	c.lines, c.turn = map[uint32][]answer{}, nil
	for sc := range c.serving {
		c.stopWrites(sc, codes.Unavailable, msg)
	}
}

// stopWrites, under the gate, ends a handler's call from outside: its context done and its writes failing with this status.
func (c *Channel) stopWrites(sc *serverCall, code codes.Code, detail string) {
	sc.killed, sc.err = true, status.Error(code, detail)
	sc.cancel()
}

func (c *Channel) land(e *Incoming) {
	if e == nil {
		return
	}
	switch e.Kind {
	case Reply, Expired:
		if k := c.pending[e.Id]; k != nil {
			k.land(e)
		}
	case Call: // a call on a service this channel does not serve, or after its stop, is not its to answer
		svc, ok := c.services[e.Topic]
		if !ok || !c.stopAt.IsZero() {
			return
		}
		sc := &serverCall{call: e, svc: svc, until: time.Now().Add(time.Duration(e.TTL) * time.Second)} // its ttl counts from its arrival
		sc.ctx, sc.cancel = context.WithDeadline(peer.NewContext(context.Background(), &peer.Peer{Addr: addr(e.Sender)}), sc.until)
		if c.waiting >= inbox {
			c.owe(sc, uint32(codes.ResourceExhausted), "the server is behind", nil, true)
			sc.cancel()
			return
		}
		c.waiting++
		c.serving[sc] = struct{}{}
		go c.serve(sc)
	}
}

// Server replies, paced.

// owe, under the gate, is the call's next reply. One caller is owed 1024 at most, a flood's calls left to their ttl, and
// all of them 4096, past which the longest line gives way.
func (c *Channel) owe(sc *serverCall, st uint32, detail string, payload []byte, end bool) {
	if c.closed || sc.killed {
		return
	}
	seq := sc.seq
	sc.seq++
	if end { // its end owed, nothing more is: a stop answers it once
		sc.killed, sc.err = true, io.EOF
	}
	line, total := c.lines[sc.call.Sender], 0
	if len(line) >= inbox { // the line is full: the call is over for its handler, and its caller waits out its ttl
		c.stopWrites(sc, codes.ResourceExhausted, "the server is behind")
		return
	}
	for _, l := range c.lines {
		total += len(l)
	}
	if total >= 4*inbox {
		longest := sc.call.Sender
		for s, l := range c.lines {
			if len(l) > len(c.lines[longest]) {
				longest = s
			}
		}
		if longest == sc.call.Sender {
			c.stopWrites(sc, codes.ResourceExhausted, "the server is behind")
			return
		}
		last := &c.lines[longest][len(c.lines[longest])-1] // the longest line's newest reply gives way to that call's end
		c.stopWrites(last.sc, codes.ResourceExhausted, "the server is behind")
		*last = answer{last.sc, last.seq, uint32(codes.ResourceExhausted), "the server is behind", nil, true}
	}
	if len(line) == 0 {
		c.turn = append(c.turn, sc.call.Sender)
	}
	c.lines[sc.call.Sender] = append(line, answer{sc, seq, st, clip(detail), payload, end})
	c.poke()
}

// cut leaves a caller's line as given, its turn with it when nothing is left.
func (c *Channel) cut(sender uint32, line []answer) {
	c.lines[sender] = line
	if len(line) > 0 {
		return
	}
	delete(c.lines, sender)
	for i, s := range c.turn {
		if s == sender {
			c.turn = append(c.turn[:i], c.turn[i+1:]...)
			return
		}
	}
}

// answer, under the gate, sends the server's replies, 32 a second, a turn each: a caller sends one and goes to the back
// of the line; a full outbox leaves the rest for the next round, each until its call's ttl.
func (c *Channel) answer(now uint32) {
	if now != c.paceSec {
		c.paceSec, c.paced = now, 0
	}
	t := time.Now()
	for c.paced < pace && len(c.turn) > 0 {
		sender := c.turn[0]
		line := c.lines[sender]
		a, line := line[0], line[1:]
		if !t.After(a.sc.until) {
			var e *Error
			err := c.peer.ReplyItem(now, a.sc.call, a.seq, a.status, a.detail, a.payload, a.end)
			switch {
			case err == nil:
				c.paced++
			case errors.As(err, &e) && (e.Code == -2 || e.Code == -9):
				c.lines[sender] = append([]answer{a}, line...)
				return
			case errors.As(err, &e) && e.Code == -3 && len(a.payload) > 0: // the stream is over: its handler is asked for no more, and the caller hears why at once
				c.stopWrites(a.sc, codes.ResourceExhausted, "the reply is larger than a mudRPC wire")
				kept := []answer{{a.sc, a.seq, uint32(codes.ResourceExhausted), "the reply is larger than a mudRPC wire", nil, true}}
				for _, b := range line {
					if b.sc != a.sc {
						kept = append(kept, b)
					}
				}
				line = kept
			default:
				c.tell(err) // one bad answer is dropped, never the line
			}
		}
		c.cut(sender, line)
		if len(line) > 0 {
			c.turn = append(c.turn[1:], sender)
		}
	}
}

// kill, under the gate, ends a call from the server's side: its end owed with this status, its handler's context done and its writes failing.
func (c *Channel) kill(sc *serverCall, code codes.Code, detail string) {
	c.owe(sc, uint32(code), detail, nil, true)
	c.stopWrites(sc, code, detail)
}

// Server side, as grpc.Server's.

type serverCall struct {
	call   *Incoming
	svc    service
	until  time.Time
	ctx    context.Context
	cancel context.CancelFunc
	seq    uint32 // its next reply's
	killed bool   // its end owed, by its handler, a stop or a reply past the wire
	err    error  // what its SendMsg returns from then
}

type addr uint32

func (a addr) Network() string { return "mudrpc" }
func (a addr) String() string  { return fmt.Sprintf("mudrpc:%d", uint32(a)) }

// RegisterService is grpc.ServiceRegistrar's, what the generated Register<Service>Server calls; the channel listens on
// the service's full name, which mudRPC carries at 1 to 64 bytes of UTF-8, and panics, as grpc does, for one it cannot.
// Over the library's Udp the listen is on the socket before it returns.
func (c *Channel) RegisterService(desc *grpc.ServiceDesc, impl any) {
	if desc.HandlerType != nil {
		if ht, st := reflect.TypeOf(desc.HandlerType).Elem(), reflect.TypeOf(impl); !st.Implements(ht) {
			panic(fmt.Sprintf("grpc: Server.RegisterService found the handler of type %v that does not satisfy %v", st, ht))
		}
	}
	if !named(desc.ServiceName, true) {
		panic(fmt.Sprintf("mudRPC carries a service's full name of 1 to 64 bytes of UTF-8: %q", desc.ServiceName))
	}
	c.gate.Lock()
	defer c.gate.Unlock()
	defer c.poke()
	if _, ok := c.services[desc.ServiceName]; !ok {
		c.toListen = append(c.toListen, desc.ServiceName)
	}
	c.services[desc.ServiceName] = service{desc, impl}
	c.listen()
}

// Stop is grpc.Server's Stop and GracefulStop in one: no new calls from now; past grace, at once for 0, those running or
// waiting are answered Canceled, their contexts done. The channel it returns is closed once every handler has returned.
func (c *Channel) Stop(grace time.Duration) <-chan struct{} {
	c.gate.Lock()
	defer c.gate.Unlock()
	defer c.poke()
	if at := time.Now().Add(max(grace, 0)); c.stopAt.IsZero() || at.Before(c.stopAt) {
		c.stopAt = at
		time.AfterFunc(grace, c.poke)
	}
	c.settle()
	return c.stopped
}

// settle, under the gate, tells a Stop that every handler has returned.
func (c *Channel) settle() {
	if !c.stopAt.IsZero() && len(c.serving) == 0 && !c.stopDone {
		c.stopDone = true
		close(c.stopped)
	}
}

func (c *Channel) serve(sc *serverCall) {
	select {
	case c.sem <- struct{}{}:
		defer func() { <-c.sem }()
	case <-sc.ctx.Done():
	}
	c.gate.Lock()
	c.waiting--
	run := !sc.killed && sc.ctx.Err() == nil // one its caller has given up on, or a stop cancelled, is shed unrun
	c.gate.Unlock()
	if run {
		c.handle(sc)
	}
	c.gate.Lock()
	delete(c.serving, sc)
	c.settle()
	c.gate.Unlock()
	sc.cancel()
}

func (c *Channel) handle(sc *serverCall) {
	desc := sc.svc.desc
	for _, m := range desc.Methods {
		if m.MethodName == sc.call.Method {
			c.unary(sc, m)
			return
		}
	}
	for _, s := range desc.Streams {
		if s.StreamName == sc.call.Method {
			c.streaming(sc, s)
			return
		}
	}
	c.finish(sc, status.Errorf(codes.Unimplemented, "unknown method %v for service %v", sc.call.Method, desc.ServiceName), nil)
}

// finish, from a handler's goroutine, owes the call's last reply: the handler's status, Unknown for an error that is none, OK with payload or, for a stream, the end.
func (c *Channel) finish(sc *serverCall, err error, payload []byte) {
	st, detail := uint32(codes.OK), ""
	if err != nil {
		s := status.Convert(err)
		st, detail = uint32(s.Code()), s.Message()
	} else if payload == nil {
		detail = StreamEnd
	}
	c.gate.Lock()
	defer c.gate.Unlock()
	c.owe(sc, st, detail, payload, true)
}

func (c *Channel) unary(sc *serverCall, m grpc.MethodDesc) {
	dec := func(v any) error {
		if err := unmarshal(sc.call.Bytes, v); err != nil {
			return status.Errorf(codes.Internal, "grpc: error unmarshalling request: %v", err)
		}
		return nil
	}
	resp, err := recovering(func() (any, error) { return m.Handler(sc.svc.impl, sc.ctx, dec, nil) })
	var payload []byte
	if err == nil {
		if payload, err = marshal(resp); err != nil {
			err = status.Errorf(codes.Internal, "grpc: error while marshaling: %v", err)
		} else if payload == nil {
			payload = []byte{}
		}
	}
	c.finish(sc, err, payload)
}

func (c *Channel) streaming(sc *serverCall, s grpc.StreamDesc) {
	if s.ClientStreams {
		c.finish(sc, status.Error(codes.Unimplemented, "mudRPC carries no client stream"), nil)
		return
	}
	_, err := recovering(func() (struct{}, error) { return struct{}{}, s.Handler(sc.svc.impl, &serverStream{c: c, sc: sc}) })
	c.finish(sc, err, nil)
}

// recovering runs a handler; a panic in it is the call's Unknown status, where grpc would end the process.
func recovering[T any](f func() (T, error)) (v T, err error) {
	defer func() {
		if r := recover(); r != nil {
			err = status.Errorf(codes.Unknown, "panic: %v", r)
		}
	}()
	return f()
}

type serverStream struct {
	c        *Channel
	sc       *serverCall
	received bool
}

func (s *serverStream) SetHeader(metadata.MD) error  { return nil }
func (s *serverStream) SendHeader(metadata.MD) error { return nil }
func (s *serverStream) SetTrailer(metadata.MD)       {}
func (s *serverStream) Context() context.Context     { return s.sc.ctx }

func (s *serverStream) SendMsg(m any) error {
	b, err := marshal(m)
	if err != nil {
		return status.Errorf(codes.Internal, "grpc: error while marshaling: %v", err)
	}
	s.c.gate.Lock()
	defer s.c.gate.Unlock()
	if err := s.sc.ctx.Err(); err != nil && !s.sc.killed { // its ttl passed
		return status.FromContextError(err).Err()
	}
	if !s.sc.killed && s.sc.seq >= maxItems {
		s.c.kill(s.sc, codes.ResourceExhausted, fmt.Sprintf("a stream carries at most %d items", maxItems))
	}
	if s.sc.killed {
		return s.sc.err
	}
	s.c.owe(s.sc, 0, "", b, false)
	return nil
}

func (s *serverStream) RecvMsg(m any) error {
	if s.received {
		return io.EOF
	}
	s.received = true
	if err := unmarshal(s.sc.call.Bytes, m); err != nil {
		return status.Errorf(codes.Internal, "grpc: error unmarshalling request: %v", err)
	}
	return nil
}

// Client side, as grpc.ClientConn's.

// Invoke is grpc.ClientConnInterface's unary call; call options are not carried.
func (c *Channel) Invoke(ctx context.Context, method string, args, reply any, _ ...grpc.CallOption) error {
	b, err := c.begin(ctx, method, args, false).result()
	if err != nil {
		return err
	}
	if err := unmarshal(b, reply); err != nil {
		return status.Errorf(codes.Internal, "grpc: failed to unmarshal the received message: %v", err)
	}
	return nil
}

// NewStream is grpc.ClientConnInterface's: a server stream, sent on its first SendMsg; a client or bidirectional stream is Unimplemented.
func (c *Channel) NewStream(ctx context.Context, desc *grpc.StreamDesc, method string, _ ...grpc.CallOption) (grpc.ClientStream, error) {
	if desc.ClientStreams {
		return nil, status.Error(codes.Unimplemented, "mudRPC carries no client stream")
	}
	return &clientStream{ctx: ctx, c: c, method: method}, nil
}

type clientStream struct {
	ctx    context.Context
	c      *Channel
	method string
	k      *call
}

func (s *clientStream) Header() (metadata.MD, error) { return nil, nil }
func (s *clientStream) Trailer() metadata.MD         { return nil }
func (s *clientStream) CloseSend() error             { return nil }
func (s *clientStream) Context() context.Context     { return s.ctx }

func (s *clientStream) SendMsg(m any) error {
	if s.k != nil {
		return io.EOF // one request a call
	}
	s.k = s.c.begin(s.ctx, s.method, m, true)
	return nil
}

func (s *clientStream) RecvMsg(m any) error {
	if s.k == nil {
		return status.Error(codes.Internal, "mudRPC: no request was sent")
	}
	b, err := s.k.item()
	if err != nil {
		return err
	}
	if err := unmarshal(b, m); err != nil {
		err = status.Errorf(codes.Internal, "grpc: failed to unmarshal the received message: %v", err)
		s.c.gate.Lock()
		s.k.abort(status.Convert(err))
		s.c.gate.Unlock()
		return err
	}
	return nil
}

// call is one call of ours: replies land in any order and are taken in seq order; the server's status comes after the
// items before it. A local end, a cancel, a deadline or a close, ends it whatever the replies say, unread items cut off.
type call struct {
	ch       *Channel
	ctx      context.Context
	stream   bool
	key      [12]byte
	early    map[uint32]*Incoming
	next     uint32
	items    [][]byte // raw bytes, decoded by the reader
	response []byte
	got      bool
	done     bool
	st       *status.Status
	wake     chan struct{}
	stop     func() bool
}

func (c *Channel) begin(ctx context.Context, method string, args any, stream bool) *call {
	k := &call{ch: c, ctx: ctx, stream: stream, wake: make(chan struct{}, 1)}
	svc, name, ok := strings.Cut(strings.TrimPrefix(method, "/"), "/")
	if !ok || !named(svc, true) || !utf8.ValidString(name) {
		k.end(status.New(codes.InvalidArgument, (&Error{-1}).Error())) // as the peer refuses an empty name
		return k
	}
	payload, err := marshal(args)
	if err != nil {
		k.end(status.Newf(codes.Internal, "grpc: error while marshaling: %v", err))
		return k
	}
	c.gate.Lock()
	defer c.gate.Unlock()
	life := c.ttl
	if d, ok := ctx.Deadline(); ok {
		left := time.Until(d)
		if left <= 0 {
			k.end(status.New(codes.DeadlineExceeded, context.DeadlineExceeded.Error()))
			return k
		}
		life = uint32(min(math.Ceil(left.Seconds())+1, week)) // a second over: the clock is whole seconds, the context is the deadline
	}
	if ctx.Err() != nil {
		k.end(status.FromContextError(ctx.Err()))
	} else if c.closed {
		k.end(status.New(codes.Unavailable, "the channel is closed"))
	} else if k.key, err = c.peer.Call(c.now(), svc, name, payload, life); err != nil {
		k.end(refused(err))
	} else {
		c.pending[k.key] = k
		k.stop = context.AfterFunc(ctx, func() { // the deadline or a cancel ends it then, from the context's own timer
			c.gate.Lock()
			defer c.gate.Unlock()
			k.abort(status.FromContextError(ctx.Err()))
		})
		c.poke()
	}
	return k
}

// Under the gate.

func (k *call) poke() {
	select {
	case k.wake <- struct{}{}:
	default:
	}
}

func (k *call) land(e *Incoming) {
	if e.Kind == Expired {
		k.abort(status.New(codes.DeadlineExceeded, context.DeadlineExceeded.Error()))
		return
	}
	if k.done || e.Seq < k.next {
		return
	}
	if k.early == nil {
		k.early = map[uint32]*Incoming{}
	}
	if _, dup := k.early[e.Seq]; !dup {
		k.early[e.Seq] = e
	}
	for r, ok := k.early[k.next]; ok && !k.done; r, ok = k.early[k.next] {
		delete(k.early, k.next)
		k.next++
		switch {
		case r.Status != 0:
			k.end(status.New(codes.Code(r.Status), r.Detail))
		case r.End && r.Detail == StreamEnd:
			k.end(status.New(codes.OK, ""))
		default:
			if k.stream {
				k.items = append(k.items, r.Bytes)
			} else if !k.got {
				k.response, k.got = r.Bytes, true
			}
			if r.End {
				k.end(status.New(codes.OK, ""))
			}
		}
	}
	k.poke()
}

// end is the server's status: the call ends, its items still to be read.
func (k *call) end(st *status.Status) {
	if k.done {
		return
	}
	k.done, k.st, k.early = true, st, nil
	delete(k.ch.pending, k.key)
	if st.Code() == codes.OK && !k.stream && !k.got {
		k.st = status.New(codes.Internal, "no response")
	}
	k.poke()
}

// abort is a local end: whatever the exchange, the call ends now, unread items cut off; a call over and read keeps its status.
func (k *call) abort(st *status.Status) {
	if k.done && (!k.stream || len(k.items) == 0) {
		return
	}
	k.items, k.done = nil, false
	k.end(st)
}

// On the caller's goroutine. The context's callback ends a call on its own goroutine, so a reader looks at the context
// first, and a cancel is seen before the items it cuts off; a call read to its end lets the callback go.

func (k *call) release() {
	if k.stop != nil {
		k.stop()
	}
}

func (k *call) result() ([]byte, error) {
	for {
		k.ch.gate.Lock()
		if !k.done && k.ctx.Err() != nil {
			k.abort(status.FromContextError(k.ctx.Err()))
		}
		done, st, b := k.done, k.st, k.response
		k.ch.gate.Unlock()
		if done {
			k.release()
			return b, st.Err()
		}
		<-k.wake
	}
}

func (k *call) item() ([]byte, error) {
	for {
		k.ch.gate.Lock()
		if len(k.items) > 0 && k.ctx.Err() != nil {
			k.abort(status.FromContextError(k.ctx.Err()))
		}
		if len(k.items) > 0 && k.ch.closed {
			k.items = nil
			k.st = status.New(codes.Unavailable, "the channel is closed")
		}
		if len(k.items) > 0 {
			b := k.items[0]
			k.items = k.items[1:]
			k.ch.gate.Unlock()
			return b, nil
		}
		done, st := k.done, k.st
		k.ch.gate.Unlock()
		if done {
			k.release()
			if st.Code() == codes.OK {
				return nil, io.EOF
			}
			return nil, st.Err()
		}
		<-k.wake
	}
}

// Helpers.

func marshal(m any) ([]byte, error) {
	pm, ok := m.(proto.Message)
	if !ok {
		return nil, fmt.Errorf("message is %T, want proto.Message", m)
	}
	return proto.Marshal(pm)
}

func unmarshal(b []byte, m any) error {
	pm, ok := m.(proto.Message)
	if !ok {
		return fmt.Errorf("message is %T, want proto.Message", m)
	}
	return proto.Unmarshal(b, pm)
}

// named is UTF-8, and 1 to 64 bytes if bounded, as a service's full name is; core carries a method's name of any length.
func named(s string, bounded bool) bool {
	return utf8.ValidString(s) && (!bounded || (len(s) > 0 && len(s) <= MaxTopic))
}

// clip is a detail as the wire carries it: UTF-8, at most 128 bytes, cut on a character.
func clip(s string) string {
	s = strings.ToValidUTF8(s, "")
	for len(s) > detailBytes {
		_, n := utf8.DecodeLastRuneInString(s)
		s = s[:len(s)-n]
	}
	return s
}

func refused(err error) *status.Status {
	var e *Error
	code := codes.Internal
	if errors.As(err, &e) {
		switch e.Code {
		case -2, -3, -9:
			code = codes.ResourceExhausted
		case -1:
			code = codes.InvalidArgument
		}
	}
	return status.New(code, err.Error())
}
