// Sensor telemetry over UDP in C++: a gateway and two sensors, three peers over UDP in one loop, on the binding's own
// API with the messages protoc generates. The sensors publish readings the gateway listens to; the gateway calibrates one.
#include <chrono>
#include <cstdio>
#include <map>
#include <thread>

#include "mud.hpp"
#include "sensor.pb.h"

using demo::sensors::Ack;
using demo::sensors::Offset;
using demo::sensors::Reading;
using namespace std::chrono;

const std::string key = [] { std::string k; for (int i = 1; i <= 32; i++) k.push_back(char(i)); return k; }();   // the fleet key: a product provisions 32 random bytes
const uint32_t epoch = 1;
const char* const names[] = {"gateway", "sensor-1", "sensor-2"};

struct Node {
  Node(const char* name, uint32_t sender, const std::string& bind, const std::string& others) : peer(name, key, sender, epoch), udp(bind, others) {}
  mud::Peer peer;
  mud::Udp udp;
};

std::string at(int32_t port) { return "127.0.0.1:" + std::to_string(port); }
std::string reading(const char* sensor, int32_t centi) {
  Reading r;
  r.set_sensor(sensor);
  r.set_centi_degrees(centi);
  return r.SerializeAsString();
}
std::string degrees(int32_t centi) { return std::to_string(centi / 100) + "." + (centi % 100 < 10 ? "0" : "") + std::to_string(centi % 100) + "C"; }

int main() {
  int32_t port[3];
  {
    std::optional<mud::Udp> probe[3];   // three free ports, held until all are known
    for (int i = 0; i < 3; i++) port[i] = probe[i].emplace("127.0.0.1:0", "127.0.0.1:9").port();
  }
  Node nodes[] = {{names[0], 1, at(port[0]), at(port[1]) + " " + at(port[2])}, {names[1], 2, at(port[1]), at(port[0]) + " " + at(port[2])}, {names[2], 3, at(port[2]), at(port[0]) + " " + at(port[1])}};
  mud::Peer& gateway = nodes[0].peer;
  gateway.listen("sensors.Readings");
  nodes[2].peer.listen("demo.sensors.Sensor");
  nodes[1].peer.publish(0, "sensors.Readings", reading(names[1], 2150), 60);
  nodes[2].peer.publish(0, "sensors.Readings", reading(names[2], 1900), 60);

  std::map<std::string, int32_t> readings;   // sorted by sensor name
  bool called = false, acked = false;
  auto start = steady_clock::now();
  for (uint32_t now = 0; !acked; std::this_thread::sleep_for(milliseconds(5))) {
    for (Node& n : nodes) {
      while (auto o = n.peer.outgoing(now)) {
        try { n.udp.send(*o); } catch (const mud::Error&) { break; }   // refused: the item is there next round
        n.peer.sent();
      }
      while (auto w = n.udp.poll()) {
        mud::Incoming in = n.peer.deliver(now, w->first, w->second);
        Reading r;
        Offset o;
        Ack a;
        if (in.kind == mud::Kind::event && r.ParseFromString(in.bytes)) {
          readings[r.sensor()] = r.centi_degrees();
        } else if (in.kind == mud::Kind::call && o.ParseFromString(in.bytes)) {
          a.set_sensor(names[2]);
          a.set_offset_centi_degrees(o.centi_degrees());
          n.peer.reply(now, in, a.SerializeAsString());
        } else if (in.kind == mud::Kind::reply && a.ParseFromString(in.bytes)) {
          std::printf("calibrated %s by %s\n", a.sensor().c_str(), degrees(a.offset_centi_degrees()).c_str());
          acked = true;
        }
      }
      n.peer.tick(now);
    }
    if (readings.size() == 2 && !called) {
      for (auto& [sensor, centi] : readings) std::printf("reading %s %s\n", sensor.c_str(), degrees(centi).c_str());
      Offset o;
      o.set_centi_degrees(50);
      gateway.call(now, "demo.sensors.Sensor", "Calibrate", o.SerializeAsString(), 10);
      called = true;
    }
    now = uint32_t(duration_cast<seconds>(steady_clock::now() - start).count());
  }
  for (Node& n : nodes) {
    n.peer.close();
    n.udp.close();
  }
  return 0;
}
