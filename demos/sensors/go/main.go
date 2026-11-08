// Sensor telemetry over UDP in Go: a gateway and two sensors, three mudRPC peers on UDP in one loop, the messages the
// protobuf types protoc-gen-go generates from sensor.proto. The binding's own API, as a device would use it.
package main

import (
	"fmt"
	"net"
	"sort"
	"time"

	mud "github.com/jacob-mellor/mudRPC/bindings/go/v3"
	"google.golang.org/protobuf/proto"

	"mudrpc.demo/sensors/pb"
)

// The fleet key: a product provisions 32 random bytes to every peer.
var key = []byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32}

func must[T any](v T, err error) T {
	if err != nil {
		panic(err)
	}
	return v
}

func free() string {
	l := must(net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)}))
	defer l.Close()
	return l.LocalAddr().String()
}

func enc(m proto.Message) []byte { return must(proto.Marshal(m)) }

func degrees(centi int32) string { return fmt.Sprintf("%d.%02dC", centi/100, centi%100) }

func main() {
	names, addrs := []string{"gateway", "sensor-1", "sensor-2"}, []string{free(), free(), free()}
	peers, radios := make([]*mud.Peer, 3), make([]*mud.Udp, 3)
	for i, n := range names {
		others := append(append([]string{}, addrs[:i]...), addrs[i+1:]...)
		radios[i], peers[i] = must(mud.NewUdp(addrs[i], others...)), must(mud.New(n, key, uint32(i+1), 1))
	}
	gateway := peers[0]
	gateway.Listen("sensors.Readings") // the gateway pumps first, so its listen reaches its socket before the readings do
	peers[2].Listen("demo.sensors.Sensor")
	peers[1].Publish(0, "sensors.Readings", enc(&pb.Reading{Sensor: "sensor-1", CentiDegrees: 2150}), 60)
	peers[2].Publish(0, "sensors.Readings", enc(&pb.Reading{Sensor: "sensor-2", CentiDegrees: 1900}), 60)

	var readings []*pb.Reading
	start, done := time.Now(), false
	for !done {
		now := uint32(time.Since(start) / time.Second)
		for i, p := range peers {
			for o, _ := p.Outgoing(now); o != nil; o, _ = p.Outgoing(now) {
				if radios[i].Send(o) != nil {
					break // the item is there next round
				}
				p.Sent()
			}
			for topic, wire, _ := radios[i].Poll(); wire != nil; topic, wire, _ = radios[i].Poll() {
				in, _ := p.Deliver(now, topic, wire)
				if in == nil {
					continue
				}
				switch in.Kind {
				case mud.Event:
					var r pb.Reading
					proto.Unmarshal(in.Bytes, &r)
					if readings = append(readings, &r); len(readings) == 2 {
						sort.Slice(readings, func(a, b int) bool { return readings[a].Sensor < readings[b].Sensor })
						for _, r := range readings {
							fmt.Printf("reading %s %s\n", r.Sensor, degrees(r.CentiDegrees))
						}
						gateway.Call(now, "demo.sensors.Sensor", "Calibrate", enc(&pb.Offset{CentiDegrees: 50}), 10)
					}
				case mud.Call:
					var off pb.Offset
					proto.Unmarshal(in.Bytes, &off)
					p.Reply(now, in, enc(&pb.Ack{Sensor: names[i], OffsetCentiDegrees: off.CentiDegrees}))
				case mud.Reply:
					var a pb.Ack
					proto.Unmarshal(in.Bytes, &a)
					fmt.Printf("calibrated %s by %s\n", a.Sensor, degrees(a.OffsetCentiDegrees))
					done = true
				}
			}
			for e, _ := p.Tick(now); e != nil; e, _ = p.Tick(now) {
			}
		}
		time.Sleep(5 * time.Millisecond)
	}
	for i := range peers {
		peers[i].Close()
		radios[i].Close()
	}
}
