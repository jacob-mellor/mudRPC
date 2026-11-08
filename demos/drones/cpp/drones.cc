// gRPC drone telemetry in C++: a ground station and a mock drone, two peers over UDP on one machine, written as gRPC
// C++ code is, with mud:: for grpc::. Building the channels is the only mudRPC code.
#include <iostream>
#include <thread>

#include "drone.mud.pb.h"

using namespace demo::drones;
using namespace std::chrono;

const std::string key = [] { std::string k; for (int i = 1; i <= 32; i++) k.push_back(char(i)); return k; }();   // the fleet key: a product provisions 32 random bytes
const uint32_t epoch = 1;

class MockDrone final : public Drone::Service {
  bool armed_ = false;

  mud::Status Arm(mud::ServerContext*, const ArmRequest*, ArmReply* reply) override {
    armed_ = true;
    reply->set_armed(true);
    reply->set_drone("drone-1");
    return mud::Status::OK;
  }

  mud::Status Telemetry(mud::ServerContext*, const TelemetryRequest* r, mud::ServerWriter<Sample>* writer) override {
    for (uint32_t i = 0; i < r->samples(); i++) {
      Sample s;
      s.set_seq(i);
      s.set_altitude_m(10 * (i + 1));
      s.set_battery_pct(100 - 2 * i);
      writer->Write(s);
    }
    return mud::Status::OK;
  }

  mud::Status Land(mud::ServerContext*, const LandRequest*, LandReply* reply) override {
    if (!armed_) return mud::Status(mud::FAILED_PRECONDITION, "not armed");
    armed_ = false;
    reply->set_landed(true);
    return mud::Status::OK;
  }
};

struct Context : mud::ClientContext {
  Context() { set_deadline(system_clock::now() + seconds(10)); }
};

std::string told(const mud::Status& s) {
  static const char* names[] = {"OK", "CANCELLED", "UNKNOWN", "INVALID_ARGUMENT", "DEADLINE_EXCEEDED", "NOT_FOUND", "ALREADY_EXISTS", "PERMISSION_DENIED", "RESOURCE_EXHAUSTED",
                                "FAILED_PRECONDITION", "ABORTED", "OUT_OF_RANGE", "UNIMPLEMENTED", "INTERNAL", "UNAVAILABLE", "DATA_LOSS", "UNAUTHENTICATED"};
  return std::string(names[s.error_code()]) + " " + s.error_message();
}

std::string at(int32_t port) { return "127.0.0.1:" + std::to_string(port); }

int main() {
  int32_t a, b;
  {
    mud::Udp pa("127.0.0.1:0", "127.0.0.1:9"), pb("127.0.0.1:0", "127.0.0.1:9");   // two free ports, held until both are known
    a = pa.port();
    b = pb.port();
  }
  auto gcs = mud::CreateChannel(mud::Peer("gcs", key, 1, epoch), std::make_shared<mud::Udp>(at(a), at(b)));
  auto drone = mud::CreateChannel(mud::Peer("drone", key, 2, epoch), std::make_shared<mud::Udp>(at(b), at(a)));
  MockDrone service;
  mud::ServerBuilder builder;
  builder.AddListeningPort(drone);
  builder.RegisterService(&service);
  std::unique_ptr<mud::Server> server(builder.BuildAndStart());
  std::this_thread::sleep_for(milliseconds(200));   // a server answers once its listen has reached its socket

  // From here, the code a gRPC C++ product already has.
  auto stub = Drone::NewStub(gcs);
  Context first;
  LandReply landed;
  std::cout << "land: " << told(stub->Land(&first, LandRequest(), &landed)) << std::endl;

  ArmRequest pilot;
  pilot.set_pilot("gcs");
  Context arming;
  ArmReply armed;
  stub->Arm(&arming, pilot, &armed);
  std::cout << "armed " << armed.drone() << " for " << pilot.pilot() << std::endl;

  TelemetryRequest want;
  want.set_samples(3);
  Context streaming;
  auto reader = stub->Telemetry(&streaming, want);
  std::cout << "telemetry";
  for (Sample s; reader->Read(&s);) std::cout << " " << s.seq() << ":" << s.altitude_m() << "m " << s.battery_pct() << "%";
  reader->Finish();
  std::cout << std::endl;

  Context last;
  if (stub->Land(&last, LandRequest(), &landed).ok() && landed.landed()) std::cout << "landed" << std::endl;

  server->Shutdown();
  drone->close();
  gcs->close();
  return 0;
}
