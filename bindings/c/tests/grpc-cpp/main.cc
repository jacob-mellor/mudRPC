// The C++ drop-in under test: the Stub and Service protoc-gen-mud generated from drone.proto, written as gRPC C++ code
// is with mud:: for grpc::, over two mudRPC peers in this process. Building the channels is the only mudRPC code above
// them.
#include <atomic>
#include <future>
#include <iostream>

#include "drone.mud.pb.h"

using namespace std::chrono;
using test::drone::ArmReply;
using test::drone::ArmRequest;
using test::drone::Drone;
using test::drone::Sample;
using test::drone::TelemetryRequest;

const std::string key = [] { std::string k; for (int i = 1; i <= 32; i++) k.push_back(char(i)); return k; }();
std::atomic<bool> heard{false}, closed_heard{false}, shut_heard{false}, stopped{false};
std::atomic<long> reached{0};

void say(const std::string& s) { std::cout << s << std::endl; }   // each line as it happens, so a run cut short shows where
void nap(int ms) { std::this_thread::sleep_for(milliseconds(ms)); }
std::string yes(bool b) { return b ? "true" : "false"; }
ArmRequest arm(uint32_t id, size_t note = 0) { ArmRequest r; r.set_id(id); r.set_note(std::string(note, '\0')); return r; }
TelemetryRequest telemetry(uint32_t count, uint32_t fail_after = 0) { TelemetryRequest r; r.set_count(count); r.set_fail_after(fail_after); return r; }

std::string told(const mud::Status& s) {
  static const char* names[] = {"OK", "CANCELLED", "UNKNOWN", "INVALID_ARGUMENT", "DEADLINE_EXCEEDED", "NOT_FOUND", "ALREADY_EXISTS", "PERMISSION_DENIED", "RESOURCE_EXHAUSTED",
                                "FAILED_PRECONDITION", "ABORTED", "OUT_OF_RANGE", "UNIMPLEMENTED", "INTERNAL", "UNAVAILABLE", "DATA_LOSS", "UNAUTHENTICATED"};
  return std::string(names[s.error_code()]) + " " + s.error_message();
}

template <class F> std::string thrown(F f) {
  try { f(); return "no error"; } catch (const mud::Error& e) { return "Error " + std::to_string(e.code); }
}

// A unary call as a product makes one, with a deadline from now when there is one.
mud::Status call(Drone::StubInterface& stub, const ArmRequest& r, std::optional<milliseconds> timeout = std::nullopt, ArmReply* out = nullptr) {
  mud::ClientContext ctx;
  ctx.set_wait_for_ready(true);
  if (timeout) ctx.set_deadline(system_clock::now() + *timeout);
  ArmReply reply;
  return stub.Arm(&ctx, r, out ? out : &reply);
}

bool armed(Drone::StubInterface& stub, std::optional<milliseconds> timeout = std::nullopt) {
  ArmReply r;
  return call(stub, arm(1), timeout, &r).ok() && r.armed();
}

// A stream read to its end: the items' seqs, then its status, the items before a failure said first.
std::string read(Drone::StubInterface& stub, const TelemetryRequest& r, std::optional<milliseconds> timeout = std::nullopt, int pause = 0, mud::Status* status = nullptr) {
  mud::ClientContext ctx;
  if (timeout) ctx.set_deadline(system_clock::now() + *timeout);
  auto reader = stub.Telemetry(&ctx, r);
  std::string seen;
  for (Sample s; reader->Read(&s);) {
    seen += (seen.empty() ? "" : " ") + std::to_string(s.seq());
    if (pause) nap(pause);
  }
  mud::Status s = reader->Finish();
  if (status) *status = s;
  return seen;
}

size_t count(const std::string& seen) { return seen.empty() ? 0 : 1 + std::count(seen.begin(), seen.end(), ' '); }

// Sockets on loopback that list each other: ports chosen free, chosen again while another test takes one between the
// choosing and the binding.
std::vector<std::shared_ptr<mud::Udp>> udp_set(int n) {
  for (int i = 0;; i++) {
    std::vector<int32_t> ports;
    for (int k = 0; k < n; k++) ports.push_back(mud::Udp("127.0.0.1:0", "127.0.0.1:9").port());
    std::vector<std::shared_ptr<mud::Udp>> made;
    try {
      for (int32_t p : ports) {
        std::string peers;
        for (int32_t q : ports) if (q != p) peers += " 127.0.0.1:" + std::to_string(q);
        made.push_back(std::make_shared<mud::Udp>("127.0.0.1:" + std::to_string(p), peers));
      }
      return made;
    } catch (const mud::Error& e) {
      if (e.code != -2 || i >= 50) throw;
    }
  }
}

class Native;

// The radio between the channels: drops nothing unless cut, and can hold the drone's wires to deliver them reversed.
struct Link {
  std::mutex m;
  mud::Channel* ends[2] = {nullptr, nullptr};
  Native* native = nullptr;
  bool cut = false, refuse_listen = false;
  int hold = 0;
  std::vector<std::pair<std::string, std::string>> held;
  uint32_t last_ttl = 0;
  void send(int from, const mud::Outgoing& o);
};

// A mudRPC peer with no gRPC: Rover's Telemetry sends count items, count-1 down to 0, end on the last; Arm refuses. It
// can also make one call of its own, the status of its answer kept.
class Native {
 public:
  explicit Native(mud::Peer p) : peer_(std::move(p)) {
    peer_.listen("test.drone.Rover");
    thread_ = std::thread([this] {
      auto start = steady_clock::now();
      while (!stopped_) {
        {
          std::lock_guard<std::mutex> l(m_);
          uint32_t now = uint32_t(duration_cast<seconds>(steady_clock::now() - start).count());
          if (!ask_.empty()) {
            peer_.call(now, ask_, method_, "", 5);
            ask_.clear();
          }
          for (auto& w : inbox_) {
            mud::Incoming c;
            try { c = peer_.deliver(now, w.first, w.second); } catch (const mud::Error&) { continue; }
            if (c.kind == mud::Kind::reply) answer_ = std::to_string(c.status) + " " + c.detail;
            if (c.kind != mud::Kind::call) continue;
            uint32_t n = c.bytes.size() > 1 ? uint8_t(c.bytes[1]) : 0;
            if (c.method == "Telemetry" && n == 99) peer_.reply(now, c, "\x0a\xff\xff");
            else if (c.method == "Arm") peer_.reply(now, c, "", 9, "native says no");
            else for (uint32_t i = 0; i < n; i++) {
              Sample s;
              s.set_seq(n - 1 - i);
              peer_.reply(now, c, s.SerializeAsString(), 0, "", i, i == n - 1);
            }
          }
          inbox_.clear();
          while (auto o = peer_.outgoing(now)) {
            if (!o->listen && back) back->deliver(o->topic, o->wire);
            peer_.sent();
          }
        }
        nap(10);
      }
    });
  }
  ~Native() {
    stopped_ = true;
    thread_.join();
  }
  void deliver(const std::string& t, const std::string& w) {
    std::lock_guard<std::mutex> l(m_);
    inbox_.emplace_back(t, w);
  }
  void ask(const std::string& service, const std::string& method) {
    std::lock_guard<std::mutex> l(m_);
    ask_ = service;
    method_ = method;
  }
  std::string answer() {
    std::lock_guard<std::mutex> l(m_);
    return answer_;
  }
  mud::Channel* back = nullptr;

 private:
  std::mutex m_;
  mud::Peer peer_;
  std::vector<std::pair<std::string, std::string>> inbox_;
  std::string ask_, method_, answer_;
  std::atomic<bool> stopped_{false};
  std::thread thread_;
};

void Link::send(int from, const mud::Outgoing& o) {
  std::unique_lock<std::mutex> l(m);
  if (o.listen) {
    if (refuse_listen) throw std::runtime_error("this radio has no listen");
    return;
  }
  last_ttl = o.ttl_secs;
  if (cut) return;
  if (native) {
    l.unlock();
    native->deliver(o.topic, o.wire);
    return;
  }
  mud::Channel* to = ends[1 - from];
  std::vector<std::pair<std::string, std::string>> now{{o.topic, o.wire}};
  if (from == 1 && hold > 0) {
    held.emplace_back(o.topic, o.wire);
    if (int(held.size()) < hold) return;
    hold = 0;
    now.assign(held.rbegin(), held.rend());
    held.clear();
  }
  l.unlock();
  for (auto& w : now) to->deliver(w.first, w.second);
}

class DroneService final : public Drone::Service {
  mud::Status Arm(mud::ServerContext* context, const ArmRequest* r, ArmReply* reply) override {
    switch (r->id()) {
      case 0: return mud::Status(mud::StatusCode::FAILED_PRECONDITION, "battery low");
      case 13: throw std::runtime_error("the camera is gone");
      case 23:
        reply->set_armed(true);
        reply->set_by(std::string(1000, 'x'));
        return mud::Status::OK;
      case 25: {
        std::string s;
        for (int i = 0; i < 100; i++) s += "\xc3\xa9";
        return mud::Status(mud::StatusCode::FAILED_PRECONDITION, s);
      }
      case 26: return mud::Status(mud::StatusCode::FAILED_PRECONDITION, "bad \xff\xc3 byte");
      case 24: case 28: case 29:
        while (!context->IsCancelled()) nap(20);
        (r->id() == 24 ? heard : r->id() == 28 ? closed_heard : shut_heard) = true;
        return mud::Status::OK;
      case 30: reached = long(duration_cast<seconds>(context->deadline() - system_clock::now()).count()); [[fallthrough]];
      default:
        reply->set_armed(true);
        reply->set_id(r->id());
        reply->set_by(context->peer());
        return mud::Status::OK;
    }
  }

  mud::Status Telemetry(mud::ServerContext*, const TelemetryRequest* r, mud::ServerWriter<Sample>* writer) override {
    for (uint32_t i = 0; i < r->count(); i++) {
      if (r->fail_after() != 0 && i == r->fail_after()) return mud::Status(mud::StatusCode::ABORTED, "lost the camera");
      Sample s;
      s.set_seq(i);
      if (r->big_at() != 0) {
        if (i == r->big_at()) s.set_pad(std::string(1000, 'x'));
        nap(30);
      }
      if (!writer->Write(s)) {
        stopped = true;
        return mud::Status::CANCELLED;
      }
    }
    return mud::Status::OK;
  }

  mud::Status Upload(mud::ServerContext*, mud::ServerReader<Sample>* reader, ArmReply* reply) override {
    for (Sample s; reader->Read(&s);) {}
    reply->set_armed(true);
    return mud::Status::OK;
  }
};

// A service whose full name mudRPC cannot carry, written by hand.
class BadName final : public mud::Service {
 public:
  explicit BadName(std::string name) : mud::Service(std::move(name)) {}
};

// A channel its own radio reaches, set once the channel is built, which its pump may already be running.
struct Box {
  std::mutex m;
  std::weak_ptr<mud::Channel> t;
  void set(const std::shared_ptr<mud::Channel>& c) { std::lock_guard<std::mutex> l(m); t = c; }
  std::shared_ptr<mud::Channel> get() { std::lock_guard<std::mutex> l(m); return t.lock(); }
};

struct Served {
  std::unique_ptr<mud::Server> server;
  std::thread waiting;
};
std::vector<Served> servers;
void serve(std::shared_ptr<mud::Channel> channel, mud::Service* service) {
  mud::ServerBuilder builder;
  builder.AddListeningPort(channel);
  builder.RegisterService(service);
  std::unique_ptr<mud::Server> server(builder.BuildAndStart());
  mud::Server* s = server.get();
  servers.push_back({std::move(server), std::thread([s] { s->Wait(); })});
}

std::shared_ptr<mud::Channel> over(Link& link, int end, const char* name, uint32_t sender, mud::Channel::Fault fault = {}) {
  auto t = mud::CreateChannel(mud::Peer(name, key, sender, 1), [&link, end](const mud::Outgoing& o) { link.send(end, o); }, 60, fault);
  link.ends[end] = t.get();
  return t;
}

int main() {
  DroneService service;
  Link link;
  std::mutex said;
  std::string fault;
  auto gcs = over(link, 0, "gcs", 1);
  auto drone = over(link, 1, "drone", 2, [&](const std::exception& e) { std::lock_guard<std::mutex> l(said); fault = e.what(); });
  serve(drone, &service);

  // From here, the code a gRPC C++ product already has.
  auto stub = Drone::NewStub(gcs);
  ArmReply r;
  call(*stub, arm(7), std::nullopt, &r);
  say("arm: armed " + yes(r.armed()) + " id " + std::to_string(r.id()) + " by " + r.by());
  say("refused: " + told(call(*stub, arm(0))));
  say("handler throws: " + told(call(*stub, arm(13))));
  {
    mud::ClientContext ctx;
    say("unknown method: " + told(mud::internal::BlockingUnaryCall(gcs.get(), "test.drone.Drone", "Nope", &ctx, arm(1), &r)));   // as a hand-written stub would
    mud::ClientContext badName, longName;
    say("a method name that is not UTF-8: " + told(mud::internal::BlockingUnaryCall(gcs.get(), "test.drone.Drone", "\xff", &badName, arm(1), &r)) + ", a service name of 65 bytes: " +
        told(mud::internal::BlockingUnaryCall(gcs.get(), std::string(65, 'a').c_str(), "Arm", &longName, arm(1), &r)) + ", then armed " + yes(armed(*stub)));
    mud::ClientContext fly;
    say("a method the service leaves out: " + told(stub->Fly(&fly, arm(1), &r)));
  }

  {
    std::lock_guard<std::mutex> l(link.m);
    link.hold = 6;   // five items and the stream's end, delivered newest first
  }
  say("telemetry reversed: " + read(*stub, telemetry(5)));
  say("telemetry empty: " + read(*stub, telemetry(0)));
  mud::Status st;
  std::string seen = read(*stub, telemetry(5, 2), std::nullopt, 0, &st);
  say("telemetry fails after: " + seen);
  say("telemetry fails: " + told(st));
  {
    mud::ClientContext ctx;
    auto writer = stub->Upload(&ctx, &r);
    Sample s;
    say("upload: wrote " + yes(writer->Write(s)) + ", " + told(writer->Finish()));
    mud::ClientContext chat;
    auto both = stub->Chat(&chat);
    Sample::Point p;
    say("chat: read " + yes(both->Read(&p)) + ", " + told(both->Finish()));
  }
  say("too large: " + told(call(*stub, arm(1, 1000))));

  link.m.lock();
  link.cut = true;
  link.m.unlock();
  auto t0 = steady_clock::now();
  say("deadline: " + told(call(*stub, arm(9), seconds(2))));
  auto took = steady_clock::now() - t0;
  say("deadline in time: " + yes(took >= seconds(2) && took < milliseconds(3500)));
  {
    mud::ClientContext ctx;
    auto waiting = std::async(std::launch::async, [&] { return stub->Arm(&ctx, arm(10), &r); });
    nap(300);
    ctx.TryCancel();
    say("cancelled: " + told(waiting.get()));
    mud::ClientContext early;
    early.TryCancel();
    say("cancelled before it starts: " + told(stub->Arm(&early, arm(10), &r)));
  }
  say("past the deadline: " + told(call(*stub, arm(1), milliseconds(-1000))));
  link.m.lock();
  link.cut = false;
  link.m.unlock();
  say("after the cut: armed " + yes(armed(*stub)));

  say("a reply past the wire: " + told(call(*stub, arm(23))));
  mud::Status wide = call(*stub, arm(25));
  const std::string& m = wide.error_message();
  say("a detail of 100 two-byte characters: " + told(wide).substr(0, 19) + ", " + std::to_string(std::count_if(m.begin(), m.end(), [](char c) { return (uint8_t(c) & 0xC0) != 0x80; })) + " kept, " + std::to_string(m.size()) + " bytes");
  say("a detail that is not UTF-8: " + told(call(*stub, arm(26))));
  say("two full streams back to back: " + std::to_string(count(read(*stub, telemetry(63)))) + " " + std::to_string(count(read(*stub, telemetry(63)))));
  read(*stub, telemetry(64), std::nullopt, 0, &st);
  say("a 64th item: " + told(st));
  TelemetryRequest big = telemetry(40);
  big.set_big_at(2);
  seen = read(*stub, big, std::nullopt, 0, &st);
  nap(100);
  say("an item past the wire mid-stream: " + seen + ", " + told(st) + ", its handler stopped: " + yes(stopped));
  {
    mud::ClientContext ctx;
    auto reader = stub->Telemetry(&ctx, telemetry(2));
    for (Sample s; reader->Read(&s);) {}
    ctx.TryCancel();
    say("a cancel after the end: " + told(reader->Finish()));
    mud::ServerContext direct;
    Drone::Service& handler = service;
    ArmRequest q = arm(5);
    ArmReply a;
    mud::Status got = handler.Arm(&direct, &q, &a);   // called before the line is built: C++ leaves the order of a sum's terms open
    say("a handler called directly: " + told(got) + "armed " + yes(a.armed()) + ", cancelled " + yes(direct.IsCancelled()));
  }

  auto lone = mud::CreateChannel(mud::Peer("lone", key, 9, 1), [](const mud::Outgoing&) {});
  lone->use([](mud::Peer& p) { p.close(); });
  nap(200);   // the pump's next round finds the peer gone and closes the channel
  say("the peer closed first: " + told(call(*Drone::NewStub(lone), arm(1), seconds(2))));

  size_t free16 = 16 - gcs->use([](mud::Peer& p) { return p.pending(); });   // a call cancelled after sending keeps its slot until its ttl
  std::vector<std::future<std::string>> nopes;
  for (size_t i = 0; i < free16; i++) {
    nopes.push_back(std::async(std::launch::async, [&] {
      mud::ClientContext ctx;
      ctx.set_deadline(system_clock::now() + seconds(5));
      ArmReply x;
      return told(mud::internal::BlockingUnaryCall(gcs.get(), "test.drone.Drone", "Nope", &ctx, arm(1), &x));
    }));
  }
  std::set<std::string> codes;
  for (auto& f : nopes) codes.insert(f.get());
  say("unknown method, every free slot at once: " + std::string(codes == std::set<std::string>{"UNIMPLEMENTED "} ? "all unimplemented" : *codes.begin()));
  link.m.lock();
  link.cut = true;
  link.m.unlock();
  {
    mud::ClientContext far;
    far.set_deadline(system_clock::now() + hours(8 * 24));
    auto waiting = std::async(std::launch::async, [&] { return stub->Arm(&far, arm(1), &r); });
    nap(200);
    link.m.lock();
    link.cut = false;
    link.m.unlock();
    far.TryCancel();
    waiting.wait();
  }
  say("a deadline eight days out: ttl " + std::to_string(link.last_ttl));
  {
    mud::ClientContext forever;
    forever.set_deadline(system_clock::time_point::max());
    ArmReply fresh;
    mud::Status got = stub->Arm(&forever, arm(1), &fresh);
    say("an infinite deadline: " + told(got) + "armed " + yes(fresh.armed()));
  }
  for (int i = 0; i < 5000; i++) gcs->deliver("gcs", std::string(40, '\0'));
  say("five thousand junk wires: then armed " + yes(armed(*stub)));

  {
    mud::ClientContext ctx;
    auto reading = std::async(std::launch::async, [&] {
      auto reader = stub->Telemetry(&ctx, telemetry(5));
      Sample s;
      reader->Read(&s);
      nap(500);   // every item lands, and the reader is cancelled
      bool more = reader->Read(&s);
      return std::string(more ? "item " + std::to_string(s.seq()) + ", " : "") + told(reader->Finish());
    });
    nap(250);
    ctx.TryCancel();
    say("cancelled with items unread: " + reading.get());
  }
  t0 = steady_clock::now();
  read(*stub, telemetry(5), seconds(1), 600, &st);
  say("a slow reader past its deadline: " + told(st));
  say("the slow reader's deadline in time: " + yes(steady_clock::now() - t0 < seconds(2)));
  call(*stub, arm(30), hours(3));
  say("a three-hour ttl reaches the server as: " + std::to_string(reached) + "s");
  {
    mud::ServerBuilder builder;
    builder.AddListeningPort(drone).RegisterService(&service);
    bool none = builder.BuildAndStart() == nullptr;
    std::lock_guard<std::mutex> l(said);
    say("a second server on one channel: " + std::string(none ? "none, " : "built, ") + fault);
  }
  say("a server that never answers: " + told(call(*stub, arm(24), seconds(1))));
  nap(1500);
  say("the server saw its ttl pass: " + yes(heard));

  Link link3;
  link3.refuse_listen = true;
  std::atomic<int> faults{0};
  auto gcs3 = over(link3, 0, "gcs3", 7, [&](const std::exception&) { faults++; });
  auto drone3 = over(link3, 1, "drone3", 8);
  serve(drone3, &service);
  bool armed3 = armed(*Drone::NewStub(gcs3), seconds(10));
  say("a radio refusing a listen: armed " + yes(armed3) + ", faults heard " + (faults > 0 ? "yes" : "no"));

  Link linkB;
  Box back;
  auto gcsB = mud::CreateChannel(mud::Peer("gcsb", key, 19, 1), [&](const mud::Outgoing& o) {
    std::async(std::launch::async, [&] { if (auto t = back.get()) t->use([](mud::Peer& p) { return p.pending(); }); }).get();   // the radio waits on a thread that calls into its channel
    linkB.send(0, o);
  });
  back.set(gcsB);
  linkB.ends[0] = gcsB.get();
  auto droneB = over(linkB, 1, "droneb", 20);
  serve(droneB, &service);
  say("a radio calling back in from another thread: armed " + yes(armed(*Drone::NewStub(gcsB), seconds(5))));

  auto deaf = mud::CreateChannel(mud::Peer("deaf", key, 31, 1), [](const mud::Outgoing&) {});
  {
    auto deafStub = Drone::NewStub(deaf);
    std::vector<std::thread> sixteen;
    for (int i = 0; i < 16; i++) sixteen.emplace_back([&] { call(*deafStub, arm(1), seconds(1)); });
    for (auto& t : sixteen) t.join();
    nap(2500);   // past every ttl, which only a tick frees in core
    say("after sixteen calls timed out: " + told(call(*deafStub, arm(1), seconds(1))));
  }
  deaf->close();
  std::weak_ptr<mud::Channel> dropped = mud::CreateChannel(mud::Peer("dropped", key, 32, 1), [](const mud::Outgoing&) {});
  nap(100);   // the pump may hold it to the end of a round
  say("a channel dropped unclosed is freed: " + yes(dropped.expired()));
  {
    auto t = mud::CreateChannel(mud::Peer("midround", key, 33, 1), [](const mud::Outgoing&) { nap(200); });
    dropped = t;
    nap(50);   // the pump is inside its radio, holding the channel for its round
  }
  nap(400);
  say("a channel dropped mid-round is freed: " + yes(dropped.expired()));
  Box self;
  auto selfish = mud::CreateChannel(mud::Peer("selfish", key, 10, 1), [&](const mud::Outgoing&) { if (auto t = self.get()) t->close(); });
  self.set(selfish);
  nap(300);
  say("closed from its own send: " + thrown([&] { selfish->close(); }));

  auto pair = udp_set(2);
  auto gcsU = mud::CreateChannel(mud::Peer("gcsu", key, 11, 1), pair[0]);
  auto droneU = mud::CreateChannel(mud::Peer("droneu", key, 12, 1), pair[1]);
  serve(droneU, &service);
  nap(200);   // a server answers once its listen has reached its socket
  auto overUdp = Drone::NewStub(gcsU);
  say("over UDP: armed " + yes(armed(*overUdp, seconds(5))) + ", telemetry " + read(*overUdp, telemetry(5)));
  auto lostSocket = std::make_shared<mud::Udp>("127.0.0.1:0", "127.0.0.1:" + std::to_string(pair[1]->port()));
  auto lost = mud::CreateChannel(mud::Peer("lost", key, 13, 1), lostSocket);
  lostSocket->close();
  nap(100);
  say("its socket closed: " + told(call(*Drone::NewStub(lost), arm(1), seconds(2))));
  say("a public bind: " + thrown([] { mud::Udp("0.0.0.0:0", "127.0.0.1:9"); }) + ", a public peer: " + thrown([] { mud::Udp("127.0.0.1:0", "8.8.8.8:53"); }) + ", this library public: " + yes(mud::Udp::internet()));
  say("an octal address: " + thrown([] { mud::Udp("127.0.0.1:0", "010.0.0.1:7000"); }) + ", no peer: " + thrown([] { mud::Udp("127.0.0.1:0", ""); }));

  std::string longFault;
  auto longName = mud::CreateChannel(mud::Peer("long", key, 14, 1), [](const mud::Outgoing&) {}, 60, [&](const std::exception& e) { longFault = e.what(); });
  for (auto name : {std::string(70, 'a'), std::string("test.\xff")}) {
    BadName bad(name);
    mud::ServerBuilder builder;
    builder.AddListeningPort(longName).RegisterService(&bad).RegisterService(&service);
    bool none = builder.BuildAndStart() == nullptr;
    say("a service name of " + std::to_string(name.size()) + " bytes" + (name.size() < 64 ? ", not UTF-8: " : ": ") + std::string(none ? "none, " : "built, ") + longFault);
  }

  auto three = udp_set(3);
  std::vector<std::shared_ptr<mud::Channel>> nodes;
  for (int i = 0; i < 3; i++) nodes.push_back(mud::CreateChannel(mud::Peer("node" + std::to_string(i), key, 21 + i, 1), three[i]));
  serve(nodes[1], &service);
  nap(200);   // a server answers once its listen has reached its socket
  int armedOf10 = 0;
  for (int i = 0; i < 10; i++) armedOf10 += armed(*Drone::NewStub(nodes[0]), seconds(5));
  say("three on UDP, one serving nothing: " + std::to_string(armedOf10) + " of 10 armed");

  {
    Link linkC;
    auto gcsC = over(linkC, 0, "gcsc", 17);
    auto droneC = over(linkC, 1, "dronec", 18);
    serve(droneC, &service);
    auto waiting = std::async(std::launch::async, [&] { return call(*Drone::NewStub(gcsC), arm(28), seconds(30)); });
    nap(300);
    droneC->close();
    gcsC->close();
    say("a call when its channel closes: " + told(waiting.get()));
    servers.back().waiting.join();
    say("a handler hears its channel close: " + yes(closed_heard));
  }
  {
    Link linkD;
    auto gcsD = over(linkD, 0, "gcsd", 34);
    auto droneD = over(linkD, 1, "droned", 35);
    serve(droneD, &service);
    auto waiting = std::async(std::launch::async, [&] { return call(*Drone::NewStub(gcsD), arm(29), seconds(30)); });
    nap(300);
    servers.back().server->Shutdown(system_clock::now() + milliseconds(200));
    mud::Status got = waiting.get();
    say("a shutdown past its deadline: " + told(got) + ", the handler heard it: " + yes(shut_heard));
    servers.back().waiting.join();
    Served old = std::move(servers.back());
    servers.pop_back();
    serve(droneD, &service);
    old.server.reset();   // its end leaves the server built since serving
    say("a server built after a shutdown, the old one let go: armed " + yes(armed(*Drone::NewStub(gcsD), seconds(5))));
    for (int i = 0; i < 2; i++) {   // this one, and the one whose channel closed
      servers.back().server->Shutdown();
      if (servers.back().waiting.joinable()) servers.back().waiting.join();
      servers.pop_back();
    }
  }

  // A native peer answers the Rover service with the plain API: a stream ends on its last item, and a refusal has a
  // detail. It calls the drone's client stream, which a drop-in server refuses.
  Link link2;
  Native native(mud::Peer("rover", key, 3, 1));
  auto gcs2 = over(link2, 0, "gcs2", 4);
  link2.native = &native;
  native.back = gcs2.get();
  auto rover = test::drone::Rover::NewStub(gcs2);
  {
    mud::ClientContext ctx;
    auto reader = rover->Telemetry(&ctx, telemetry(3));
    std::string got;
    for (Sample s; reader->Read(&s);) got += (got.empty() ? "" : " ") + std::to_string(s.seq());
    say("native stream: " + got + ", " + told(reader->Finish()));
    mud::ClientContext refused;
    say("native refusal: " + told(rover->Arm(&refused, arm(1), &r)));
    mud::ClientContext bad;
    auto garbled = rover->Telemetry(&bad, telemetry(99));
    Sample s;
    garbled->Read(&s);
    say("a reply that does not decode: " + told(garbled->Finish()));
  }
  Link linkN;
  Native asker(mud::Peer("asker", key, 36, 1));
  auto droneN = over(linkN, 1, "dronen", 37);
  linkN.native = &asker;
  asker.back = droneN.get();
  serve(droneN, &service);
  asker.ask("test.drone.Drone", "Upload");
  for (int i = 0; i < 100 && asker.answer().empty(); i++) nap(20);
  say("a client stream from a native peer: status " + asker.answer());

  {
    std::vector<mud::Peer> crowd;   // 48 callers owed a stream each, past the 32 a second: the next caller waits its turn, not behind them all
    for (uint32_t i = 0; i < 48; i++) {
      crowd.emplace_back("crowd" + std::to_string(i), key, 100 + i, 1);
      crowd.back().call(0, "test.drone.Drone", "Telemetry", telemetry(20).SerializeAsString(), 5);
      while (auto o = crowd.back().outgoing(0)) {
        if (!o->listen) drone->deliver(o->topic, o->wire);
        crowd.back().sent();
      }
    }
    nap(300);
    say("a caller behind 48 owed a stream each: armed " + yes(armed(*stub, seconds(3))));
  }
  mud::Peer spare("spare", key, 30, 1);
  std::string saved = spare.save();
  say("restored at a higher epoch: " + thrown([&] { mud::Peer::restore(saved, 2); }) + ", at the same: " + thrown([&] { mud::Peer::restore(saved, 1); }) + ", a short copy: " + thrown([&] { mud::Peer::restore(saved.substr(1), 3); }));
  spare.close();
  say("a closed peer: " + thrown([&] { spare.listen("x"); }) + ", pending " + std::to_string(spare.pending()) + ", a short key: " + thrown([] { mud::Peer("k", "\x01", 1, 1); }));
  for (auto& s : servers) s.server->Shutdown();
  for (auto& s : servers) if (s.waiting.joinable()) s.waiting.join();
  say("every server returned from Wait");
  for (auto* t : {&gcs, &drone, &gcs2, &gcs3, &drone3, &gcsU, &droneU, &lost, &longName, &gcsB, &droneB, &droneN}) (*t)->close();
  for (auto& n : nodes) n->close();
}
