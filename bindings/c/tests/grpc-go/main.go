// The gRPC drop-in under test: the client and server protoc-gen-go-grpc generated from drone.proto, written as grpc-go
// code is, over two mudRPC peers in this process. Building the channels is the only mudRPC code above them.
package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/peer"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	"mudrpc.test/grpcgo/mud"
	"mudrpc.test/grpcgo/pb"
)

var (
	key = func() []byte {
		k := make([]byte, 32)
		for i := range k {
			k[i] = byte(i + 1)
		}
		return k
	}()
	heard   atomic.Bool // a handler saw its context end
	stopped atomic.Bool // a stream's handler had its write refused
)

func say(format string, a ...any) { fmt.Printf(format+"\n", a...) }

func told(err error) string {
	if err == nil {
		return "no error"
	}
	s := status.Convert(err)
	return s.Code().String() + " " + s.Message()
}

func within(d time.Duration) context.Context {
	ctx, cancel := context.WithTimeout(context.Background(), d)
	time.AfterFunc(d+time.Minute, cancel)
	return ctx
}

func read(s grpc.ServerStreamingClient[pb.Sample], err error) (string, error) {
	var seen []string
	for err == nil {
		var item *pb.Sample
		if item, err = s.Recv(); err == nil {
			seen = append(seen, fmt.Sprint(item.Seq))
		}
	}
	if err == io.EOF {
		err = nil
	}
	return strings.Join(seen, " "), err
}

func waitFor(flag *atomic.Bool, d time.Duration) bool {
	for t := time.Now(); time.Since(t) < d; time.Sleep(10 * time.Millisecond) {
		if flag.Load() {
			return true
		}
	}
	return false
}

func free() int {
	l, err := net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		panic(err)
	}
	defer l.Close()
	return l.LocalAddr().(*net.UDPAddr).Port
}

// udpSet is n sockets on loopback that list each other: ports chosen free, chosen again while another test takes one between the choosing and the binding.
func udpSet(n int) []*mud.Udp {
	for {
		ports, out, ok := make([]string, n), make([]*mud.Udp, 0, n), true
		for i := range ports {
			ports[i] = fmt.Sprintf("127.0.0.1:%d", free())
		}
		for i := range ports {
			others := append(append([]string{}, ports[:i]...), ports[i+1:]...)
			u, err := mud.NewUdp(ports[i], others...)
			if err != nil {
				ok = false
				break
			}
			out = append(out, u)
		}
		if ok {
			return out
		}
		for _, u := range out {
			u.Close()
		}
	}
}

type wire struct {
	topic string
	wire  []byte
}

// Link is the radio between the channels: drops nothing unless cut, and can hold the drone's wires to deliver them reversed.
type Link struct {
	mu                sync.Mutex
	ends              [2]*mud.Channel
	native            *Native
	hold              int
	held              []wire
	cut, refuseListen atomic.Bool
	lastTTL           atomic.Uint32
}

func (l *Link) radio(from int) mud.RadioFunc {
	return func(o *mud.Outgoing) error {
		if o.Listen {
			if l.refuseListen.Load() {
				return errors.New("this radio has no listen")
			}
			return nil
		}
		l.lastTTL.Store(o.TTL)
		if l.cut.Load() {
			return nil
		}
		l.mu.Lock()
		to, native := l.ends[1-from], l.native
		if from == 1 && l.hold > 0 {
			l.held = append(l.held, wire{o.Topic, o.Wire})
			if len(l.held) < l.hold {
				l.mu.Unlock()
				return nil
			}
			held := l.held
			l.hold, l.held = 0, nil
			l.mu.Unlock()
			for i := len(held) - 1; i >= 0; i-- {
				to.Deliver(held[i].topic, held[i].wire)
			}
			return nil
		}
		l.mu.Unlock()
		if native != nil {
			native.push(o.Topic, o.Wire)
		} else {
			to.Deliver(o.Topic, o.Wire)
		}
		return nil
	}
}

func (l *Link) connect(a, b *mud.Channel) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.ends = [2]*mud.Channel{a, b}
}

func (l *Link) holdNext(n int) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.hold = n
}

// DroneService is the product's: every case the test needs, by request id.
type DroneService struct{ pb.UnimplementedDroneServer }

func (DroneService) Arm(ctx context.Context, r *pb.ArmRequest) (*pb.ArmReply, error) {
	switch r.Id {
	case 0:
		return nil, status.Error(codes.FailedPrecondition, "battery low")
	case 13:
		panic("thirteen")
	case 22:
		return nil, status.Error(codes.Internal, "")
	case 23:
		return &pb.ArmReply{Armed: true, By: strings.Repeat("x", 1000)}, nil
	case 24:
		<-ctx.Done()
		heard.Store(true)
		return nil, ctx.Err()
	case 26:
		return nil, status.Error(codes.FailedPrecondition, strings.Repeat("é", 100))
	case 27:
		return nil, status.Error(codes.FailedPrecondition, "bad \xff byte")
	case 28:
		return nil, errors.New("not a status")
	case 29:
		return nil, nil
	}
	p, _ := peer.FromContext(ctx)
	return &pb.ArmReply{Armed: true, Id: r.Id, By: p.Addr.String()}, nil
}

func (DroneService) Telemetry(r *pb.TelemetryRequest, s grpc.ServerStreamingServer[pb.Sample]) error {
	for i := uint32(0); i < r.Count; i++ {
		if r.FailAfter != 0 && i == r.FailAfter {
			return status.Error(codes.Aborted, "lost the camera")
		}
		item := &pb.Sample{Seq: i}
		if r.BigAt != 0 && i == r.BigAt {
			item.Pad = make([]byte, 1000)
		}
		if err := s.Send(item); err != nil {
			stopped.Store(r.BigAt != 0)
			return err
		}
		if r.BigAt != 0 {
			time.Sleep(30 * time.Millisecond)
		}
	}
	return nil
}

// Native is a mudRPC server with no gRPC: Rover's Telemetry sends count items, 2 down to 0, end on the last; Arm refuses, or answers bytes that are no message.
type Native struct {
	peer  *mud.Peer
	mu    sync.Mutex
	inbox []wire
	back  *mud.Channel
	stop  atomic.Bool
}

func (n *Native) push(topic string, w []byte) {
	n.mu.Lock()
	defer n.mu.Unlock()
	n.inbox = append(n.inbox, wire{topic, w})
}

func (n *Native) run() {
	start := time.Now()
	for !n.stop.Load() {
		now := uint32(time.Since(start) / time.Second)
		n.mu.Lock()
		in := n.inbox
		n.inbox = nil
		n.mu.Unlock()
		for _, w := range in {
			c, _ := n.peer.Deliver(now, w.topic, w.wire)
			if c == nil || c.Kind != mud.Call {
				continue
			}
			if c.Method == "Arm" {
				var r pb.ArmRequest
				_ = proto.Unmarshal(c.Bytes, &r)
				if r.Id == 9 {
					_ = n.peer.Reply(now, c, []byte{0xff})
				} else {
					_ = n.peer.ReplyItem(now, c, 0, 9, "native says no", nil, true)
				}
				continue
			}
			var r pb.TelemetryRequest
			_ = proto.Unmarshal(c.Bytes, &r)
			for i := uint32(0); i < r.Count; i++ {
				b, _ := proto.Marshal(&pb.Sample{Seq: r.Count - 1 - i})
				_ = n.peer.ReplyItem(now, c, i, 0, "", b, i == r.Count-1)
			}
		}
		for {
			o, _ := n.peer.Outgoing(now)
			if o == nil {
				break
			}
			if !o.Listen && n.back != nil {
				n.back.Deliver(o.Topic, o.Wire)
			}
			_ = n.peer.Sent()
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func must(p *mud.Peer, err error) *mud.Peer {
	if err != nil {
		panic(err)
	}
	return p
}

func main() {
	link := &Link{}
	gcs := mud.NewChannel(must(mud.New("gcs", key, 1, 1)), link.radio(0))
	drone := mud.NewChannel(must(mud.New("drone", key, 2, 1)), link.radio(1))
	link.connect(gcs, drone)
	pb.RegisterDroneServer(drone, DroneService{})

	// From here, the code a grpc-go product already has.
	bg := context.Background()
	client := pb.NewDroneClient(gcs)
	r, err := client.Arm(bg, &pb.ArmRequest{Id: 7})
	say("arm: armed %v id %d by %s, %v", r.Armed, r.Id, r.By, err)
	_, err = client.Arm(bg, &pb.ArmRequest{Id: 0})
	say("refused: %s", told(err))
	_, err = client.Arm(bg, &pb.ArmRequest{Id: 13})
	say("handler panics: %s", told(err))
	_, err = client.Fly(bg, &pb.ArmRequest{Id: 1})
	say("a method the service leaves out: %s", told(err))
	var reply pb.ArmReply
	say("unknown method: %s", told(gcs.Invoke(bg, "/test.drone.Drone/Land", &pb.ArmRequest{Id: 1}, &reply)))
	named := func(names ...string) string {
		set := map[string]bool{}
		for _, n := range names {
			set[told(gcs.Invoke(bg, n, &pb.ArmRequest{Id: 1}, &reply))] = true
		}
		var out []string
		for s := range set {
			out = append(out, s)
		}
		if len(out) == 1 {
			return out[0]
		}
		return fmt.Sprint(len(out), " answers")
	}
	say("a service name empty, past 64 bytes or not UTF-8, a method name not UTF-8: %s", named("//Arm", "/"+strings.Repeat("x", 65)+"/Arm", "/test.\xff/Arm", "/test.drone.Drone/\xff"))
	say("a method name of 65 bytes, carried: %s", named("/test.drone.Drone/"+strings.Repeat("y", 65)))
	r, _ = client.Arm(bg, &pb.ArmRequest{Id: 1})
	say("then armed %v", r.Armed)
	_, err = client.Arm(bg, &pb.ArmRequest{Id: 28})
	say("an error that is no status: %s", told(err))
	_, err = client.Arm(bg, &pb.ArmRequest{Id: 29})
	say("a nil reply: %s", status.Code(err))

	link.holdNext(6) // five items and the stream's end, delivered newest first
	items, err := read(client.Telemetry(bg, &pb.TelemetryRequest{Count: 5}))
	say("telemetry reversed: %s, %v", items, err)
	items, err = read(client.Telemetry(bg, &pb.TelemetryRequest{Count: 0}))
	say("telemetry empty: %q, %v", items, err)
	items, err = read(client.Telemetry(bg, &pb.TelemetryRequest{Count: 5, FailAfter: 2}))
	say("telemetry fails after: %s, %s", items, told(err))
	_, err = client.Upload(bg)
	say("upload: %s", told(err))
	_, err = client.Chat(bg)
	say("chat: %s", told(err))
	_, err = client.Arm(bg, &pb.ArmRequest{Id: 1, Note: make([]byte, 1000)})
	say("too large: %s", told(err))

	link.cut.Store(true)
	t0 := time.Now()
	_, err = client.Arm(within(2*time.Second), &pb.ArmRequest{Id: 9})
	took := time.Since(t0)
	say("deadline: %s", told(err))
	say("deadline in time: %v", 2*time.Second <= took && took < 3500*time.Millisecond)
	ctx, cancel := context.WithCancel(bg)
	time.AfterFunc(100*time.Millisecond, cancel)
	_, err = client.Arm(ctx, &pb.ArmRequest{Id: 10})
	say("cancelled: %s", told(err))
	_, err = client.Arm(ctx, &pb.ArmRequest{Id: 10})
	say("cancelled before it starts: %s", told(err))
	past, cancelPast := context.WithDeadline(bg, time.Now().Add(-time.Second))
	_, err = client.Arm(past, &pb.ArmRequest{Id: 1})
	cancelPast()
	say("past the deadline: %s", told(err))
	link.cut.Store(false)
	r, _ = client.Arm(bg, &pb.ArmRequest{Id: 11})
	say("after the cut: armed %v", r.Armed)

	_, err = client.Arm(bg, &pb.ArmRequest{Id: 23})
	say("a reply past the wire: %s", told(err))
	_, err = client.Arm(bg, &pb.ArmRequest{Id: 26})
	say("a detail of 100 two-byte characters: %s, %d kept, %d bytes", status.Code(err), len([]rune(status.Convert(err).Message())), len(status.Convert(err).Message()))
	_, err = client.Arm(bg, &pb.ArmRequest{Id: 27})
	say("a detail that is not UTF-8: %s", told(err))
	_, err = client.Arm(bg, &pb.ArmRequest{Id: 22})
	say("a code with no message: %s %q", status.Code(err), status.Convert(err).Message())

	finished := 0
	for range 30 {
		ctx, cancel := context.WithCancel(bg)
		result := make(chan error, 1)
		go func() { _, err := client.Arm(ctx, &pb.ArmRequest{Id: 16}); result <- err }()
		gcs.Use(func(*mud.Peer) { time.Sleep(30 * time.Millisecond); go cancel(); time.Sleep(20 * time.Millisecond) }) // the reply waits in the inbox while the cancel waits for the pump
		select {
		case <-result:
			finished++
		case <-time.After(3 * time.Second):
		}
		cancel()
	}
	say("cancel racing a reply: %d of 30 finished", finished)

	answered, saves := 0, 0
	calling := make(chan struct{})
	go func() {
		defer close(calling)
		for range 50 { // a client keeps under 32 calls a second
			if r, err := client.Arm(bg, &pb.ArmRequest{Id: 1}); err == nil && r.Armed {
				answered++
			}
			time.Sleep(40 * time.Millisecond)
		}
	}()
	for open := true; open; {
		select {
		case <-calling:
			open = false
		default:
			gcs.Use(func(p *mud.Peer) {
				if _, err := p.Save(); err == nil {
					saves++
				}
			})
			time.Sleep(time.Millisecond)
		}
	}
	say("saved while calling: %d calls answered, saves %v", answered, map[bool]string{true: "made", false: "none"}[saves > 0])

	a, _ := read(client.Telemetry(bg, &pb.TelemetryRequest{Count: 63}))
	b, _ := read(client.Telemetry(bg, &pb.TelemetryRequest{Count: 63}))
	say("two full streams back to back: %d %d", len(strings.Fields(a)), len(strings.Fields(b)))
	items, err = read(client.Telemetry(bg, &pb.TelemetryRequest{Count: 64}))
	say("a 64th item: %d then %s", len(strings.Fields(items)), told(err))
	items, err = read(client.Telemetry(bg, &pb.TelemetryRequest{Count: 40, BigAt: 2}))
	say("an item past the wire mid-stream: %s, %s, its handler stopped: %v", items, told(err), waitFor(&stopped, 2*time.Second))

	lonePeer := must(mud.New("lone", key, 9, 1))
	lone := mud.NewChannel(lonePeer, mud.RadioFunc(func(*mud.Outgoing) error { return nil }))
	lonePeer.Close()
	time.Sleep(200 * time.Millisecond) // the pump's next round finds the peer gone and closes the channel
	_, err = pb.NewDroneClient(lone).Arm(within(2*time.Second), &pb.ArmRequest{Id: 1})
	say("the peer closed first: %s", told(err))

	var freeSlots int // a call cancelled after sending keeps its slot until its ttl
	gcs.Use(func(p *mud.Peer) { freeSlots = 16 - int(p.Pending()) })
	results := make(chan codes.Code, freeSlots)
	for range freeSlots {
		go func() {
			results <- status.Code(gcs.Invoke(within(5*time.Second), "/test.drone.Drone/Land", &pb.ArmRequest{Id: 1}, &reply))
		}()
	}
	all := true
	for range freeSlots {
		all = all && <-results == codes.Unimplemented
	}
	say("unknown method, every free slot at once: all Unimplemented %v", all)
	link.cut.Store(true)
	eight, cancelEight := context.WithTimeout(bg, 8*24*time.Hour)
	go func() { _, _ = client.Arm(eight, &pb.ArmRequest{Id: 1}) }()
	time.Sleep(200 * time.Millisecond)
	cancelEight()
	link.cut.Store(false)
	say("a deadline eight days out: ttl %d", link.lastTTL.Load())
	gcs.Use(func(*mud.Peer) {
		for range 5000 {
			gcs.Deliver("gcs", make([]byte, 40))
		}
	})
	r, _ = client.Arm(bg, &pb.ArmRequest{Id: 1})
	say("five thousand junk wires: then armed %v", r.Armed)

	ctx, cancel = context.WithCancel(bg)
	stream, _ := client.Telemetry(ctx, &pb.TelemetryRequest{Count: 5})
	time.Sleep(500 * time.Millisecond) // every item has landed
	_, _ = stream.Recv()
	cancel()
	_, err = stream.Recv()
	say("cancelled with items unread: %s", told(err))
	t0 = time.Now()
	stream, _ = client.Telemetry(within(time.Second), &pb.TelemetryRequest{Count: 5})
	for _, err = stream.Recv(); err == nil; _, err = stream.Recv() {
		time.Sleep(600 * time.Millisecond)
	}
	say("a slow reader past its deadline: %s", told(err))
	say("the slow reader's deadline in time: %v", time.Since(t0) < 2*time.Second)

	link.cut.Store(true)
	timedOut := make(chan struct{}, 16)
	for range 16 {
		go func() { _, _ = client.Arm(within(time.Second), &pb.ArmRequest{Id: 1}); timedOut <- struct{}{} }()
	}
	for range 16 {
		<-timedOut
	}
	link.cut.Store(false)
	time.Sleep(2100 * time.Millisecond) // their ttl, a second over the deadline, passes
	r, err = client.Arm(within(3*time.Second), &pb.ArmRequest{Id: 1})
	say("after sixteen calls timed out: %s", map[bool]string{true: "armed true", false: told(err)}[err == nil && r.Armed])

	link3 := &Link{}
	link3.refuseListen.Store(true)
	var faults atomic.Int32
	gcs3 := mud.NewChannel(must(mud.New("gcs3", key, 7, 1)), link3.radio(0), mud.Fault(func(error) { faults.Add(1) }))
	drone3 := mud.NewChannel(must(mud.New("drone3", key, 8, 1)), link3.radio(1), mud.Fault(func(error) {}))
	link3.connect(gcs3, drone3)
	pb.RegisterDroneServer(drone3, DroneService{})
	r, err = pb.NewDroneClient(gcs3).Arm(within(10*time.Second), &pb.ArmRequest{Id: 1})
	say("a radio refusing a listen: armed %v, %v, faults heard %v", r.GetArmed(), err, faults.Load() > 0)

	var holder atomic.Pointer[mud.Channel]
	selfish := mud.NewChannel(must(mud.New("selfish", key, 10, 1)), mud.RadioFunc(func(*mud.Outgoing) error {
		if c := holder.Load(); c != nil {
			c.Close()
		}
		return nil
	}))
	holder.Store(selfish)
	closed := make(chan struct{})
	go func() { time.Sleep(300 * time.Millisecond); selfish.Close(); close(closed) }()
	select {
	case <-closed:
		say("closed from its own send: returns")
	case <-time.After(3 * time.Second):
		say("closed from its own send: hangs")
	}

	pair := udpSet(2)
	gcsU := mud.NewChannel(must(mud.New("gcsu", key, 11, 1)), pair[0])
	droneU := mud.NewChannel(must(mud.New("droneu", key, 12, 1)), pair[1])
	pb.RegisterDroneServer(droneU, DroneService{})
	time.Sleep(200 * time.Millisecond) // a server answers once its listen has reached its socket
	overUDP := pb.NewDroneClient(gcsU)
	r, err = overUDP.Arm(within(5*time.Second), &pb.ArmRequest{Id: 1})
	items, _ = read(overUDP.Telemetry(within(5*time.Second), &pb.TelemetryRequest{Count: 5}))
	say("over UDP: armed %v, %v, telemetry %s", r.GetArmed(), err, items)
	port, _ := pair[1].Port()
	lostSocket, _ := mud.NewUdp("127.0.0.1:0", fmt.Sprintf("127.0.0.1:%d", port))
	lost := mud.NewChannel(must(mud.New("lost", key, 13, 1)), lostSocket)
	lostSocket.Close()
	time.Sleep(100 * time.Millisecond)
	_, err = pb.NewDroneClient(lost).Arm(within(2*time.Second), &pb.ArmRequest{Id: 1})
	say("its socket closed: %s", told(err))
	_, e1 := mud.NewUdp("0.0.0.0:0", "127.0.0.1:9")
	_, e2 := mud.NewUdp("127.0.0.1:0", "8.8.8.8:53")
	_, e3 := mud.NewUdp("0177.0.0.1:0", "127.0.0.1:9")
	_, e4 := mud.NewUdp("127.0.0.1:0")
	say("a public bind: %v, a public peer: %v, this library public: %v", e1, e2, mud.Internet())
	say("an octal address: %v, no peer: %v", e3, e4)

	_, err = client.Arm(within(time.Second), &pb.ArmRequest{Id: 24})
	say("a server that never answers: %s", told(err))
	say("the server saw its ttl pass: %v", waitFor(&heard, 5*time.Second))
	r, _ = client.Arm(bg, &pb.ArmRequest{Id: 1})
	say("then the next caller: armed %v", r.Armed)
	r, err = client.Arm(bg, &pb.ArmRequest{Id: 1})
	say("an infinite deadline: armed %v, %v", r.Armed, err)
	refused := func(f func()) (what string) {
		defer func() {
			if r := recover(); r != nil {
				what = map[bool]string{true: "refused by name", false: "refused by type"}[strings.Contains(fmt.Sprint(r), "1 to 64 bytes")]
			}
		}()
		f()
		return "accepted"
	}
	say("a service name of 70 bytes: %s", refused(func() {
		drone.RegisterService(&grpc.ServiceDesc{ServiceName: strings.Repeat("a", 70), HandlerType: (*pb.DroneServer)(nil)}, DroneService{})
	}))
	say("a service name not UTF-8: %s", refused(func() { drone.RegisterService(&grpc.ServiceDesc{ServiceName: "test.\xff"}, DroneService{}) }))
	say("the wrong handler type: %s", refused(func() { drone.RegisterService(&pb.Drone_ServiceDesc, struct{}{}) }))

	inner := mud.NewChannel(must(mud.New("inner", key, 14, 1)), mud.RadioFunc(func(*mud.Outgoing) error { return nil }))
	returned := make(chan struct{})
	go func() { inner.Use(func(*mud.Peer) { inner.Close() }); close(returned) }()
	select {
	case <-returned:
		say("closed from inside Use: returns")
	case <-time.After(3 * time.Second):
		say("closed from inside Use: hangs")
	}

	three := udpSet(3)
	gcsT, droneT, idleT := mud.NewChannel(must(mud.New("gcst", key, 21, 1)), three[0]), mud.NewChannel(must(mud.New("dronet", key, 22, 1)), three[1]), mud.NewChannel(must(mud.New("idlet", key, 23, 1)), three[2])
	pb.RegisterDroneServer(droneT, DroneService{})
	time.Sleep(200 * time.Millisecond)
	armed := 0
	for range 10 {
		if r, err := pb.NewDroneClient(gcsT).Arm(within(5*time.Second), &pb.ArmRequest{Id: 1}); err == nil && r.Armed {
			armed++
		}
	}
	say("three on UDP, one serving nothing: %d of 10 armed", armed)
	for _, c := range []*mud.Channel{gcsT, droneT, idleT} {
		c.Close()
	}

	link4 := &Link{}
	gcs4 := mud.NewChannel(must(mud.New("gcs4", key, 15, 1)), link4.radio(0))
	drone4 := mud.NewChannel(must(mud.New("drone4", key, 16, 1)), link4.radio(1))
	link4.connect(gcs4, drone4)
	pb.RegisterDroneServer(drone4, DroneService{})
	stub4 := pb.NewDroneClient(gcs4)
	heard.Store(false)
	outcome := make(chan error, 2)
	for range 2 {
		go func() { _, err := stub4.Arm(within(30*time.Second), &pb.ArmRequest{Id: 24}); outcome <- err }()
	}
	time.Sleep(300 * time.Millisecond)
	done := drone4.Stop(200 * time.Millisecond)
	first, second := told(<-outcome), told(<-outcome)
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		say("the stop never settled")
	}
	say("a stop past its grace: running %s and %s, the handler heard it: %v", first, second, heard.Load())
	_, err = stub4.Arm(within(time.Second), &pb.ArmRequest{Id: 1})
	say("a call after the stop: %s", told(err))
	heard.Store(false)
	go func() { _, _ = client.Arm(within(2*time.Second), &pb.ArmRequest{Id: 24}) }() // nothing reaches the caller, who waits out its deadline
	time.Sleep(300 * time.Millisecond)
	drone.Close()
	say("a handler hears its channel close: %v", waitFor(&heard, 5*time.Second))

	// A native peer answers the Rover service with the plain API: a stream ends on its last item, and a refusal has a detail.
	native := &Native{peer: must(mud.New("rover", key, 3, 1))}
	_ = native.peer.Listen("test.drone.Rover")
	link2 := &Link{}
	gcs2 := mud.NewChannel(must(mud.New("gcs2", key, 4, 1)), link2.radio(0))
	link2.native, native.back = native, gcs2
	go native.run()
	rover := pb.NewRoverClient(gcs2)
	items, err = read(rover.Telemetry(bg, &pb.TelemetryRequest{Count: 3}))
	say("native stream: %s, %v", items, err)
	_, err = rover.Arm(bg, &pb.ArmRequest{Id: 1})
	say("native refusal: %s", told(err))
	_, err = rover.Arm(bg, &pb.ArmRequest{Id: 9})
	say("a reply that does not decode: %s", status.Code(err))

	crowd := make([]*mud.Peer, 48) // 48 callers owed a stream each, past the 32 a second: the next caller waits its turn, not behind them all
	req, _ := proto.Marshal(&pb.TelemetryRequest{Count: 20})
	for i := range crowd {
		crowd[i] = must(mud.New(fmt.Sprintf("crowd%d", i), key, uint32(100+i), 1))
		_, _ = crowd[i].Call(0, "test.drone.Drone", "Telemetry", req, 5)
		for {
			o, _ := crowd[i].Outgoing(0)
			if o == nil {
				break
			}
			if !o.Listen {
				drone3.Deliver(o.Topic, o.Wire)
			}
			_ = crowd[i].Sent()
		}
	}
	time.Sleep(300 * time.Millisecond)
	r, err = pb.NewDroneClient(gcs3).Arm(within(3*time.Second), &pb.ArmRequest{Id: 1})
	say("a caller behind 48 owed a stream each: armed %v, %v", r.GetArmed(), err)

	saved, _ := crowd[0].Save()
	_, e1 = mud.Restore(saved, 2)
	_, e2 = mud.Restore(saved, 1)
	_, e3 = mud.Restore(saved[:100], 2)
	say("restored at a higher epoch: %v, at the same: %v, a short copy: %v", e1, e2, e3)
	crowd[1].Close()
	_, e1 = crowd[1].Call(0, "a", "b", nil, 1)
	_, e2 = mud.New("short", key[:31], 1, 1)
	say("a closed peer: %v, pending %d, a short key: %v", e1, crowd[1].Pending(), e2)

	native.stop.Store(true)
	for _, c := range []*mud.Channel{gcs, gcs2, gcs3, drone3, gcs4, drone4, gcsU, droneU, lone, lost, selfish, inner} {
		c.Close()
	}
	say("every channel closed")
	os.Stdout.Sync()
}
