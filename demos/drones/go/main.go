// gRPC drone telemetry in Go: the client and server protoc-gen-go-grpc generates from drone.proto, over two mudRPC
// peers on UDP in this process. Building the channels is the only mudRPC code.
package main

import (
	"context"
	"fmt"
	"net"
	"time"

	mud "github.com/jacob-mellor/mudRPC/bindings/go/v3"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"mudrpc.demo/drones/pb"
)

// The fleet key: a product provisions 32 random bytes to every peer.
var key = []byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32}

type droneService struct {
	pb.UnimplementedDroneServer
	armed bool
}

func (d *droneService) Arm(context.Context, *pb.ArmRequest) (*pb.ArmReply, error) {
	d.armed = true
	return &pb.ArmReply{Armed: true, Drone: "drone-1"}, nil
}

func (d *droneService) Telemetry(r *pb.TelemetryRequest, s grpc.ServerStreamingServer[pb.Sample]) error {
	for i := range r.Samples {
		if err := s.Send(&pb.Sample{Seq: i, AltitudeM: int32(10 * (i + 1)), BatteryPct: int32(100 - 2*i)}); err != nil {
			return err
		}
	}
	return nil
}

func (d *droneService) Land(context.Context, *pb.LandRequest) (*pb.LandReply, error) {
	if !d.armed {
		return nil, status.Error(codes.FailedPrecondition, "not armed")
	}
	d.armed = false
	return &pb.LandReply{Landed: true}, nil
}

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

// within is a 10 s deadline on one call.
func within() context.Context {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	time.AfterFunc(10*time.Second, cancel)
	return ctx
}

func main() {
	gcsAddr, droneAddr := free(), free()
	gcsRadio, droneRadio := must(mud.NewUdp(gcsAddr, droneAddr)), must(mud.NewUdp(droneAddr, gcsAddr))
	gcsPeer, dronePeer := must(mud.New("gcs", key, 1, 1)), must(mud.New("drone", key, 2, 1))
	gcs, drone := mud.NewChannel(gcsPeer, gcsRadio), mud.NewChannel(dronePeer, droneRadio)
	pb.RegisterDroneServer(drone, &droneService{})

	client := pb.NewDroneClient(gcs)
	_, err := client.Land(within(), &pb.LandRequest{})
	fmt.Printf("land: %s %s\n", mud.StatusName(uint32(status.Code(err))), status.Convert(err).Message())
	fmt.Printf("armed %s for gcs\n", must(client.Arm(within(), &pb.ArmRequest{Pilot: "gcs"})).Drone)
	fmt.Print("telemetry")
	stream := must(client.Telemetry(within(), &pb.TelemetryRequest{Samples: 3}))
	for s, err := stream.Recv(); err == nil; s, err = stream.Recv() {
		fmt.Printf(" %d:%dm %d%%", s.Seq, s.AltitudeM, s.BatteryPct)
	}
	fmt.Println()
	must(client.Land(within(), &pb.LandRequest{}))
	fmt.Println("landed")

	for _, c := range []interface{ Close() }{gcs, drone, gcsPeer, dronePeer, gcsRadio, droneRadio} { // channels before their peers
		c.Close()
	}
}
