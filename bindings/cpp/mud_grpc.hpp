// mudRPC as gRPC's C++ API: the Stub and Service protoc-gen-mud generates run over one mudRPC peer, so a product changes
// its include, grpc:: to mud::, and the line that builds its channel. Beside mud.hpp; the generated code adds protobuf's
// messages. The names and shapes are gRPC's sync API (Copyright the gRPC Authors, Apache License 2.0).
//
// Not a drop-in where gRPC needs a connection: a message of about 460 bytes, a service's full name of at most 64 bytes,
// no metadata, unary and server-streaming calls only, at most 63 items a stream, the first reply wins, and 16 calls in
// flight. A server sends 32 replies a second, half what a receiver takes from one sender, its callers in turn, and a
// client keeps under 32 calls a second. A deadline is the call's ttl, a week at most, none the channel's ttl; a call
// cancelled after it was sent keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
#ifndef MUD_GRPC_HPP
#define MUD_GRPC_HPP
#include <algorithm>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <deque>
#include <functional>
#include <map>
#include <set>
#include <system_error>
#include <vector>

#include "mud.hpp"

namespace mud {

enum StatusCode { OK = 0, CANCELLED = 1, UNKNOWN = 2, INVALID_ARGUMENT = 3, DEADLINE_EXCEEDED = 4, NOT_FOUND = 5, ALREADY_EXISTS = 6, PERMISSION_DENIED = 7, RESOURCE_EXHAUSTED = 8,
                  FAILED_PRECONDITION = 9, ABORTED = 10, OUT_OF_RANGE = 11, UNIMPLEMENTED = 12, INTERNAL = 13, UNAVAILABLE = 14, DATA_LOSS = 15, UNAUTHENTICATED = 16 };

class Status {
 public:
  Status() = default;
  Status(StatusCode code, std::string message) : code_(code), message_(std::move(message)) {}
  StatusCode error_code() const { return code_; }
  const std::string& error_message() const { return message_; }
  bool ok() const { return code_ == StatusCode::OK; }
  static const Status OK, CANCELLED;

 private:
  StatusCode code_ = StatusCode::OK;
  std::string message_;
};
inline const Status Status::OK{};
inline const Status Status::CANCELLED{StatusCode::CANCELLED, ""};

class Channel;
using ChannelInterface = Channel;
struct StubOptions {};

namespace internal {
using Clock = std::chrono::steady_clock;   // monotonic, as a deadline must be: a wall clock stepped forward would expire every call at once
constexpr const char* streamEnd = "end";   // the detail of the reply that ends a server stream and carries no item
constexpr uint32_t week = 604800, maxItems = 63, pace = 32;   // a message's longest life, a stream's items, replies a second
constexpr size_t inboxCap = 1024, maxWorkers = 64;   // wires or calls waiting, handlers running at once
constexpr auto patience = std::chrono::seconds(1);   // how long a refused item is tried

template <class C, class D> std::optional<Clock::time_point> steady(const std::chrono::time_point<C, D>& t) {
  auto now = C::now();
  if (t == std::chrono::time_point<C, D>::max()) return std::nullopt;   // gRPC's infinite deadline
  if (t <= now) return Clock::now();
  auto left = std::chrono::duration_cast<std::chrono::nanoseconds>(std::min<std::chrono::duration<double>>(t - now, std::chrono::seconds(week + 2)));
  return Clock::now() + left;
}

// A detail as text: well-formed UTF-8, any other byte dropped, at most 128 bytes, cut on a character.
inline std::string clip(const std::string& s) {
  std::string out;
  for (size_t i = 0; i < s.size();) {
    uint8_t c = uint8_t(s[i]);
    size_t n = c < 0x80 ? 1 : (c >> 5) == 6 ? 2 : (c >> 4) == 14 ? 3 : (c >> 3) == 30 ? 4 : 0;
    bool ok = n > 0 && i + n <= s.size();
    uint32_t cp = c & (0x7F >> n);
    for (size_t k = 1; ok && k < n; k++) {
      ok = (uint8_t(s[i + k]) & 0xC0) == 0x80;
      cp = cp << 6 | (uint8_t(s[i + k]) & 0x3Fu);
    }
    if (ok && n > 1) ok = cp >= (n == 2 ? 0x80u : n == 3 ? 0x800u : 0x10000u) && cp <= 0x10FFFF && (cp < 0xD800 || cp > 0xDFFF);
    if (!ok) { i++; continue; }
    if (out.size() + n > 128) break;
    out.append(s, i, n);
    i += n;
  }
  return out;
}

// One call in flight: replies in any order, items taken in seq order, the server's status after the items before it; a
// cancel, a deadline or a close ends it whatever the replies say, unread items cut off.
struct Call {   // guarded by its channel's lock
  struct Reply { uint32_t status; std::string detail, bytes; bool end; };
  std::optional<std::array<uint8_t, 12>> id;
  std::optional<Clock::time_point> deadline;
  std::deque<std::string> items;
  std::optional<Status> status, cut;
  bool done = false;
  uint32_t next = 0;
  std::map<uint32_t, Reply> early;

  void end(Status s, bool cutting = false) {
    if (cutting && !cut && !(done && items.empty())) cut = s;   // a call over and read keeps its status, as gRPC's
    if (done) return;
    done = true;
    early.clear();
    status = std::move(s);
  }

  void land(const Incoming& e) {
    if (e.kind == Kind::expired) return end(Status(DEADLINE_EXCEEDED, "the call's ttl passed"), true);
    if (done || e.seq < next) return;
    early.emplace(e.seq, Reply{e.status, e.detail, e.bytes, e.end});
    for (auto r = early.find(next); !done && r != early.end(); r = early.find(next)) {
      Reply y = std::move(r->second);
      early.erase(r);
      next++;
      if (y.status) end(Status(y.status <= 16 ? StatusCode(y.status) : UNKNOWN, y.detail));
      else if (y.end && y.detail == streamEnd) end(Status());
      else {
        items.push_back(std::move(y.bytes));
        if (y.end) end(Status());
      }
    }
  }
};

struct ServerCall;
}  // namespace internal

class ClientContext {
 public:
  ClientContext() = default;
  ClientContext(const ClientContext&) = delete;
  ClientContext& operator=(const ClientContext&) = delete;
  ~ClientContext() { TryCancel(); }   // a call outliving its context is cancelled, as gRPC's

  // The call's deadline on any clock, its ttl in whole seconds: past a week it is a week, and time_point::max() none.
  template <class C, class D> void set_deadline(const std::chrono::time_point<C, D>& deadline) { deadline_ = internal::steady(deadline); }
  std::chrono::system_clock::time_point deadline() const {
    if (!deadline_) return std::chrono::system_clock::time_point::max();
    return std::chrono::system_clock::now() + std::chrono::duration_cast<std::chrono::system_clock::duration>(*deadline_ - internal::Clock::now());
  }

  // Every call waits for its link until its deadline: true is mudRPC's way, false asks nothing more.
  void set_wait_for_ready(bool) {}
  std::string peer() const { return "mudrpc"; }

  // Ends the call CANCELLED from any thread; before it starts, it starts cancelled.
  void TryCancel() {
    std::function<void()> cancel;
    {
      std::lock_guard<std::mutex> l(m_);
      cancelled_ = true;
      cancel = cancel_;
    }
    if (cancel) cancel();
  }

 private:
  friend class Channel;
  bool bind(std::function<void()> cancel) {
    std::lock_guard<std::mutex> l(m_);
    cancel_ = std::move(cancel);
    return cancelled_;
  }

  std::mutex m_;
  std::function<void()> cancel_;
  bool cancelled_ = false;
  std::optional<internal::Clock::time_point> deadline_;
};

class ServerContext {
 public:
  ServerContext() = default;   // a handler's test calls it directly: never cancelled, no caller, no deadline
  // True once the call's ttl has passed, counted from its arrival, its server shut down past its deadline, or its
  // transport closed; a handler's writes fail from then.
  bool IsCancelled() const;
  // mudrpc: and the caller's roster number, proven per device under a signed fleet.
  std::string peer() const;
  std::chrono::system_clock::time_point deadline() const;

 private:
  friend class Channel;
  ServerContext(Channel* t, std::shared_ptr<internal::ServerCall> x) : t_(t), x_(std::move(x)) {}
  Channel* t_ = nullptr;
  std::shared_ptr<internal::ServerCall> x_;
};

template <class R> class ClientReaderInterface {
 public:
  virtual ~ClientReaderInterface() = default;
  virtual bool Read(R* msg) = 0;
  virtual Status Finish() = 0;
};

template <class W> class ClientWriterInterface {
 public:
  virtual ~ClientWriterInterface() = default;
  virtual bool Write(const W& msg) = 0;
  virtual bool WritesDone() = 0;
  virtual Status Finish() = 0;
};

template <class W, class R> class ClientReaderWriterInterface {
 public:
  virtual ~ClientReaderWriterInterface() = default;
  virtual bool Write(const W& msg) = 0;
  virtual bool WritesDone() = 0;
  virtual bool Read(R* msg) = 0;
  virtual Status Finish() = 0;
};

inline Status noClientStream() { return Status(UNIMPLEMENTED, "mudRPC carries no client stream"); }

// A client or bidirectional stream, which mudRPC does not carry: nothing is written and Finish is UNIMPLEMENTED.
template <class W> class ClientWriter final : public ClientWriterInterface<W> {
 public:
  bool Write(const W&) override { return false; }
  bool WritesDone() override { return false; }
  Status Finish() override { return noClientStream(); }
};

template <class W, class R> class ClientReaderWriter final : public ClientReaderWriterInterface<W, R> {
 public:
  bool Write(const W&) override { return false; }
  bool WritesDone() override { return false; }
  bool Read(R*) override { return false; }
  Status Finish() override { return noClientStream(); }
};

// The handler's side of a client or bidirectional stream, so a product's override compiles; it is never called.
template <class R> class ServerReader {
 public:
  bool Read(R*) { return false; }
};

template <class W, class R> class ServerReaderWriter {
 public:
  bool Read(R*) { return false; }
  bool Write(const W&) { return false; }
};

namespace internal {
using Write = std::function<bool(const std::string&)>;
struct Method {
  enum Shape { unary, server_stream, client_stream } shape;
  std::function<Status(ServerContext*, const std::string&, const Write&)> run;
};
}  // namespace internal

// A server stream's items, false once the call is over: its caller gone, its ttl passed, its transport closed, or 63
// items sent.
template <class W> class ServerWriter {
 public:
  explicit ServerWriter(const internal::Write& write) : write_(write) {}
  bool Write(const W& msg) { return write_(msg.SerializeAsString()); }

 private:
  const internal::Write& write_;
};

class Service {
 public:
  virtual ~Service() = default;

 protected:
  explicit Service(std::string name) : name_(std::move(name)) {}
  void AddMethod(const std::string& method, internal::Method m) { methods_[method] = std::move(m); }

 private:
  friend class Channel;
  std::string name_;
  std::map<std::string, internal::Method> methods_;
};

namespace internal {
struct ServerCall {   // guarded by its channel's lock
  Incoming call;
  Clock::time_point until;
  const Method* method = nullptr;
  uint32_t seq = 0;
  bool done = false, killed = false;
};
}  // namespace internal

// gRPC's channel and server port over one peer, pumped on a thread of its own: mud::CreateChannel builds it.
//
// The peer is the channel's while it lives: reach it only through use, whose function must not call the channel. The
// radio is a Udp, which the pump sends through and reads in its own round, or a function taking each outgoing item on
// the pump's thread with no lock held, with what arrives handed to deliver. ttl_secs is the life of a call made with no
// deadline; fault hears the first error the radio throws on an item, which is tried for a second, then let go.
class Channel : public std::enable_shared_from_this<Channel> {
 public:
  using Radio = std::function<void(const Outgoing&)>;
  using Fault = std::function<void(const std::exception&)>;
  class Made {   // only CreateChannel builds one, which starts its pump
    Made() {}   // user-provided, so no aggregate initialization gets past it
    friend class Channel;
  };

  Channel(Made, Peer peer, std::shared_ptr<Udp> udp, Radio radio, uint32_t ttl_secs, Fault fault)
      : peer_(std::move(peer)), udp_(std::move(udp)), radio_(std::move(radio)), fault_(std::move(fault)), ttl_(std::max(1u, std::min(ttl_secs, internal::week))) {
    if (!fault_) fault_ = [](const std::exception& e) { std::fprintf(stderr, "%s\n", e.what()); };   // as gRPC logs
  }
  Channel(const Channel&) = delete;
  Channel& operator=(const Channel&) = delete;

  ~Channel() {
    close();
    if (pump_.get_id() == std::this_thread::get_id()) pump_.detach();   // the pump let the last reference go: it finds the channel gone
    else if (pump_.joinable()) pump_.join();
  }

  // The peer, between rounds of the pump: save, stage, rotate, roster, counts.
  template <class F> auto use(F f) -> decltype(f(std::declval<Peer&>())) {
    std::lock_guard<std::mutex> l(m_);
    struct Kick { Channel* t; ~Kick() { t->signal(); } } kick{this};
    return f(peer_);
  }

  // A wire from the radio. Past 1024 waiting for the pump, a wire is dropped, as core would drop a flood.
  void deliver(std::string topic, std::string wire) {
    std::lock_guard<std::mutex> l(wake_->m);
    if (!wake_->taking || wake_->inbox.size() >= internal::inboxCap) return;
    wake_->inbox.push_back({std::move(topic), std::move(wire), internal::Clock::now()});
    wake_->kicked = true;
    wake_->cv.notify_one();
  }

  // Stops the pump, ends every call in flight UNAVAILABLE, and cancels every handler's context.
  void close() {
    std::lock_guard<std::mutex> l(m_);
    shut_down();
  }

  // For the generated code.
  std::shared_ptr<internal::Call> start(std::string_view service, std::string_view method, ClientContext* ctx, std::string request);
  bool read(internal::Call& c, std::string* out);
  Status finish(internal::Call& c);
  void cut(internal::Call& c, Status s) {
    std::lock_guard<std::mutex> l(m_);
    cut_locked(c, std::move(s));
  }

 private:
  friend std::shared_ptr<Channel> CreateChannel(Peer, std::shared_ptr<Udp>, uint32_t, Fault);
  friend std::shared_ptr<Channel> CreateChannel(Peer, Radio, uint32_t, Fault);
  friend class ServerContext;
  friend class Server;
  friend class ServerBuilder;
  struct Wire { std::string topic, wire; internal::Clock::time_point at; };
  struct Wake {   // outlives the channel, for a pump that finds it gone
    std::mutex m;   // the inbox's own, so a pump delivering into another channel never waits on that one's pump
    std::condition_variable cv;
    bool kicked = false, taking = true;
    std::deque<Wire> inbox;
  };
  struct Answer { std::shared_ptr<internal::ServerCall> x; uint32_t seq, status; std::string detail, payload; bool end; };

  static std::shared_ptr<Channel> make(Peer peer, std::shared_ptr<Udp> udp, Radio radio, uint32_t ttl_secs, Fault fault) {
    auto t = std::make_shared<Channel>(Made(), std::move(peer), std::move(udp), std::move(radio), ttl_secs, std::move(fault));
    t->pump_ = std::thread([w = std::weak_ptr<Channel>(t), wake = t->wake_] {
      for (;;) {
        bool idle;
        {   // the channel is held for a round, never across the wait, so one dropped unclosed goes
          auto t = w.lock();
          if (!t || !t->round()) return;
          std::lock_guard<std::mutex> l(t->m_);
          idle = !t->udp_ && t->peer_.pending() == 0 && t->peer_.queued() == 0 && t->answers_.empty() && t->to_listen_.empty() && !t->refused_since_;
        }
        std::unique_lock<std::mutex> l(wake->m);   // nothing timed is waiting, core's slots included: a deliver, a call or a close wakes it
        if (idle) wake->cv.wait(l, [&] { return wake->kicked; });
        else wake->cv.wait_for(l, std::chrono::milliseconds(10), [&] { return wake->kicked; });
        wake->kicked = false;
      }
    });
    return t;
  }

  uint32_t now() const { return uint32_t(std::min<int64_t>(std::chrono::duration_cast<std::chrono::seconds>(internal::Clock::now() - start_).count(), UINT32_MAX)); }

  void signal() {
    std::lock_guard<std::mutex> l(wake_->m);
    wake_->kicked = true;
    wake_->cv.notify_one();
  }

  // The channel closed, by its owner or by a peer or socket gone; from then every call is UNAVAILABLE.
  void shut_down() {
    if (closed_) return;
    closed_ = true;
    {
      std::lock_guard<std::mutex> l(wake_->m);
      wake_->taking = false;
      wake_->inbox.clear();
    }
    for (auto& p : pending_) p.second->end(Status(UNAVAILABLE, "the transport is closed"), true);
    pending_.clear();
    serving_ = false;
    queue_.clear();
    answers_.clear();
    order_.clear();
    work_.notify_all();
    landed_.notify_all();
    signal();
  }

  void shut(const std::string& why) {
    bool was = closed_;
    shut_down();
    if (!was) faults_.push_back(std::make_exception_ptr(std::runtime_error(why)));
  }

  static int32_t code(std::exception_ptr e) {
    try { std::rethrow_exception(e); } catch (const Error& x) { return x.code; } catch (...) { return 0; }
  }

  // A step on the peer: a peer that is gone closes the channel, any other error is a fault.
  template <class F> bool step(F f) {
    try {
      f();
      return !closed_;
    } catch (const Error& e) {
      if (e.code == MUD_ECORRUPT || e.code == MUD_EMEM) shut(std::string("the peer is closed: ") + e.what());
      else faults_.push_back(std::current_exception());
    } catch (...) {
      faults_.push_back(std::current_exception());
    }
    return false;
  }

  // One round of the pump; false once the channel is closed. The radio is called with no lock held, so it may hop to a
  // thread that is calling into this channel; the item stays the peer's until sent().
  bool round() {
    bool live = turn();
    std::vector<std::exception_ptr> told;
    {
      std::lock_guard<std::mutex> l(m_);
      told.swap(faults_);
    }
    for (auto& e : told) {   // with no lock held, so a fault may call back in from any thread
      try { std::rethrow_exception(e); } catch (const std::exception& x) { fault_(x); } catch (...) { fault_(std::runtime_error("the radio threw what cannot be told")); }
    }
    return live;
  }

  bool turn() {
    {
      std::lock_guard<std::mutex> l(m_);
      if (closed_) return false;
      uint32_t t = now();
      for (; !to_listen_.empty(); to_listen_.pop_front()) {
        try {
          peer_.listen(to_listen_.front());
        } catch (const Error& e) {
          if (e.code == MUD_EFULL) break;   // a full outbox: the next round listens
          faults_.push_back(std::current_exception());
        }
      }
      step([&] {
        std::deque<Wire> got;
        {
          std::lock_guard<std::mutex> g(wake_->m);
          got.swap(wake_->inbox);
        }
        for (auto& w : got) {
          std::optional<Incoming> e;
          try { e = peer_.deliver(t, w.topic, w.wire); } catch (const Error&) {}   // a wire the radio mangled: no verdict to act on
          if (e) land(*e, w.at);
        }
        if (udp_) {
          try {
            for (size_t i = 0; i < internal::inboxCap; i++) {
              auto w = udp_->poll();
              if (!w) break;
              std::optional<Incoming> e;
              try { e = peer_.deliver(t, w->first, w->second); } catch (const Error&) {}
              if (e) land(*e, internal::Clock::now());
            }
          } catch (const Error&) {
            return shut("the socket is closed");
          }
        }
        while (auto e = peer_.tick(t)) land(*e, internal::Clock::now());
        answer(t);
      });
    }
    for (;;) {
      std::optional<Outgoing> o;
      {
        std::lock_guard<std::mutex> l(m_);
        if (closed_ || !step([&] { o = peer_.outgoing(now()); }) || !o) break;
      }
      std::exception_ptr err;
      try { radio_(*o); } catch (...) { err = std::current_exception(); }
      std::lock_guard<std::mutex> l(m_);
      if (closed_) break;
      if (err) {
        if (udp_ && code(err) == MUD_EINVAL) {
          shut("the socket is closed");
          break;
        }
        auto at = internal::Clock::now();
        if (!refused_since_) {
          refused_since_ = at;
          faults_.push_back(err);
        }
        if (at - *refused_since_ < internal::patience) break;   // the next round tries it again; past a second it is let go
      }
      refused_since_.reset();
      if (!step([&] { peer_.sent(); })) break;
    }
    std::lock_guard<std::mutex> l(m_);
    return !closed_;
  }

  void land(const Incoming& e, internal::Clock::time_point at) {
    if (e.kind == Kind::reply || e.kind == Kind::expired) {
      auto i = pending_.find(e.id);
      if (i == pending_.end()) return;
      i->second->land(e);
      if (i->second->done) pending_.erase(i);
      landed_.notify_all();
      return;
    }
    if (e.kind != Kind::call || !serving_) return;
    auto s = services_.find(e.topic);
    if (s == services_.end()) return;   // a call on a service this channel does not serve is not its to answer
    auto x = std::make_shared<internal::ServerCall>();
    x->call = e;
    x->until = at + std::chrono::seconds(e.ttl_secs);   // its ttl counts from its arrival
    auto m = s->second->methods_.find(e.method);
    if (m == s->second->methods_.end()) return finish(x, Status(UNIMPLEMENTED, ""));
    if (m->second.shape == internal::Method::client_stream) return finish(x, noClientStream());
    if (queue_.size() >= internal::inboxCap) return finish(x, Status(RESOURCE_EXHAUSTED, "the server is behind"));
    x->method = &m->second;
    queue_.push_back(x);
    if (idle_ == 0 && workers_ < internal::maxWorkers) {
      workers_++;
      try { std::thread([t = shared_from_this()] { t->work(); }).detach(); } catch (const std::system_error&) { workers_--; }   // no thread to be had: the ones running take it
    }
    work_.notify_one();
  }

  // A handler's thread: calls from the queue until the server stops; a call its caller has given up on is shed unrun.
  void work() {
    std::unique_lock<std::mutex> l(m_);
    for (;;) {
      idle_++;
      work_.wait(l, [&] { return !queue_.empty() || !serving_; });
      idle_--;
      if (queue_.empty()) break;
      auto x = queue_.front();
      queue_.pop_front();
      if (internal::Clock::now() >= x->until) continue;
      running_.insert(x);
      l.unlock();
      ServerContext ctx(this, x);
      Status s;
      try {
        s = x->method->run(&ctx, x->call.bytes, [&](const std::string& b) { return respond(x, b); });
      } catch (...) {
        s = Status(UNKNOWN, "Unexpected error in RPC handling");   // gRPC's words for a handler's exception
      }
      l.lock();
      running_.erase(x);
      finish(x, s);
    }
    if (--workers_ == 0) landed_.notify_all();
  }

  bool respond(const std::shared_ptr<internal::ServerCall>& x, const std::string& b) {
    std::lock_guard<std::mutex> l(m_);
    if (x->done || closed_ || internal::Clock::now() > x->until) return false;
    if (x->seq >= internal::maxItems) {
      finish(x, Status(RESOURCE_EXHAUSTED, "a stream carries at most 63 items"));
      return false;
    }
    bool unary = x->method->shape == internal::Method::unary;
    owe(x, x->seq++, 0, "", b, unary);
    x->done = unary;
    return true;
  }

  void finish(const std::shared_ptr<internal::ServerCall>& x, const Status& s) {
    if (x->done) return;
    x->done = true;
    owe(x, x->seq, s.error_code(), s.ok() ? internal::streamEnd : internal::clip(s.error_message()), "", true);
  }

  void owe(const std::shared_ptr<internal::ServerCall>& x, uint32_t seq, uint32_t status, std::string detail, std::string payload, bool end) {
    if (closed_) return;
    auto& line = answers_[x->call.sender];
    if (line.size() >= internal::inboxCap) return;   // a sender owed 16 full streams is flooding: its caller waits out its ttl
    size_t owed = 0;
    for (auto& a : answers_) owed += a.second.size();
    if (owed >= 4 * internal::inboxCap) {   // the server owes 64 full streams: the longest line gives way, so one flooding sender cannot crowd out the rest
      auto longest = std::max_element(answers_.begin(), answers_.end(), [](const auto& a, const auto& b) { return a.second.size() < b.second.size(); });
      if (longest->first == x->call.sender) return;
      longest->second.pop_back();
    }
    if (std::find(order_.begin(), order_.end(), x->call.sender) == order_.end()) order_.push_back(x->call.sender);
    line.push_back({x, seq, status, std::move(detail), std::move(payload), end});
    signal();
  }

  // The server's replies, 32 a second, a turn each: a caller sends one and goes to the back of the line, so a spent
  // second leaves the next one starting with the caller after; a full outbox leaves the rest for the next round, each
  // until its call's ttl.
  void answer(uint32_t t) {
    if (t != pace_sec_) {
      pace_sec_ = t;
      paced_ = 0;
    }
    auto at = internal::Clock::now();
    while (paced_ < internal::pace && !order_.empty()) {
      uint32_t sender = order_.front();
      order_.pop_front();
      auto& line = answers_[sender];
      if (line.empty()) {
        answers_.erase(sender);
        continue;
      }
      Answer a = std::move(line.front());
      line.pop_front();
      try {
        if (at <= a.x->until) {
          peer_.reply(t, a.x->call, a.payload, a.status, a.detail, a.seq, a.end);
          paced_++;
        }
      } catch (const Error& e) {
        if (e.code == MUD_EFULL || e.code == MUD_ERATE) {
          line.push_front(std::move(a));
          order_.push_front(sender);
          return;
        } else if (e.code == MUD_ETOOLARGE && !a.payload.empty()) {
          a.x->done = a.x->killed = true;   // the stream is over: its handler's writes fail from here
          line.erase(std::remove_if(line.begin(), line.end(), [&](const Answer& b) { return b.x == a.x; }), line.end());
          line.push_front({a.x, a.seq, RESOURCE_EXHAUSTED, "the reply is larger than a mudRPC wire", "", true});
        } else {
          faults_.push_back(std::current_exception());
        }
      }
      if (line.empty()) answers_.erase(sender);
      else order_.push_back(sender);
    }
  }

  void cut_locked(internal::Call& c, Status s) {
    c.end(std::move(s), true);
    if (c.id) {
      auto i = pending_.find(*c.id);
      if (i != pending_.end() && i->second.get() == &c) pending_.erase(i);
    }
    landed_.notify_all();
  }

  // The server's side.

  std::string serve(const std::vector<Service*>& services, uint64_t* served) {
    std::lock_guard<std::mutex> l(m_);
    if (closed_) return "the transport is closed";
    if (serving_ || workers_ > 0) return "a transport serves one server";
    for (auto* s : services) {
      if (s->name_.empty() || s->name_.size() > MUD_MAX_TOPIC || internal::clip(s->name_) != s->name_) return "mudRPC carries a service's full name of 1 to 64 bytes of UTF-8: " + internal::clip(s->name_);
    }
    services_.clear();
    for (auto* s : services) {
      services_[s->name_] = s;
      if (heard_.insert(s->name_).second) to_listen_.push_back(s->name_);
    }
    serving_ = true;
    stopping_ = false;
    *served = ++served_;
    signal();
    return "";
  }

  // Takes no new calls; the running ones and those waiting finish, and past the deadline are cancelled. A server
  // already stopped, another serving since, does nothing.
  void stop(uint64_t served, std::optional<internal::Clock::time_point> deadline) {
    std::unique_lock<std::mutex> l(m_);
    if (served != served_) return;
    serving_ = false;
    stopping_ = true;
    work_.notify_all();
    landed_.notify_all();
    auto idle = [&] { return served != served_ || workers_ == 0; };   // a server built since is not this one's to wait on
    if (deadline && !landed_.wait_until(l, *deadline, idle) && served == served_) {
      for (auto& x : queue_) finish(x, Status(CANCELLED, "the server shut down"));
      queue_.clear();
      for (auto& x : running_) {
        x->killed = true;
        finish(x, Status(CANCELLED, "the server shut down"));
      }
    }
    landed_.wait(l, idle);
  }

  void stopped(uint64_t served) {
    std::unique_lock<std::mutex> l(m_);
    landed_.wait(l, [&] { return served != served_ || ((stopping_ || closed_) && workers_ == 0); });
  }

  std::mutex m_;   // every field below, the peer's one thread
  Peer peer_;
  std::shared_ptr<Udp> udp_;
  Radio radio_;
  Fault fault_;
  uint32_t ttl_;
  internal::Clock::time_point start_ = internal::Clock::now();
  std::shared_ptr<Wake> wake_ = std::make_shared<Wake>();
  std::thread pump_;
  bool closed_ = false;
  std::vector<std::exception_ptr> faults_;   // told after the lock is let go, as the radio is called
  std::condition_variable landed_, work_;
  std::map<std::array<uint8_t, 12>, std::shared_ptr<internal::Call>> pending_;
  std::map<std::string, Service*> services_;
  std::set<std::string> heard_;
  std::deque<std::string> to_listen_;
  bool serving_ = false, stopping_ = false;
  std::deque<std::shared_ptr<internal::ServerCall>> queue_;
  std::set<std::shared_ptr<internal::ServerCall>> running_;
  size_t workers_ = 0, idle_ = 0;
  uint64_t served_ = 0;   // the servers built on this channel, the latest the one serving
  std::map<uint32_t, std::deque<Answer>> answers_;
  std::deque<uint32_t> order_;   // the senders owed a reply, each once, the next to send first
  uint32_t pace_sec_ = UINT32_MAX, paced_ = 0;
  std::optional<internal::Clock::time_point> refused_since_;
};

// The channel over a Udp, as grpc::CreateChannel over a target; the stub and the server both take it.
inline std::shared_ptr<Channel> CreateChannel(Peer peer, std::shared_ptr<Udp> udp, uint32_t ttl_secs = 60, Channel::Fault fault = {}) {
  auto u = udp;
  return Channel::make(std::move(peer), std::move(udp), [u](const Outgoing& o) { u->send(o); }, ttl_secs, std::move(fault));
}

// The channel over another radio: radio takes each outgoing item, and what it receives goes to deliver.
inline std::shared_ptr<Channel> CreateChannel(Peer peer, Channel::Radio radio, uint32_t ttl_secs = 60, Channel::Fault fault = {}) {
  return Channel::make(std::move(peer), nullptr, std::move(radio), ttl_secs, std::move(fault));
}

inline std::shared_ptr<internal::Call> Channel::start(std::string_view service, std::string_view method, ClientContext* ctx, std::string request) {
  auto c = std::make_shared<internal::Call>();
  bool cancelled = ctx->bind([w = weak_from_this(), wc = std::weak_ptr<internal::Call>(c)] {
    auto t = w.lock();
    auto call = wc.lock();
    if (t && call) t->cut(*call, Status(CANCELLED, "Cancelled"));
  });
  std::lock_guard<std::mutex> l(m_);
  if (cancelled) c->end(Status(CANCELLED, "Cancelled"), true);
  if (closed_) c->end(Status(UNAVAILABLE, "the transport is closed"));
  if (c->done) return c;
  uint32_t life = ttl_;
  if (ctx->deadline_) {
    auto left = std::chrono::duration<double>(*ctx->deadline_ - internal::Clock::now()).count();
    if (!(left > 0)) {
      c->end(Status(DEADLINE_EXCEEDED, "Deadline Exceeded"));
      return c;
    }
    life = uint32_t(std::min(std::ceil(left) + 1, double(internal::week)));   // a second over: the clock is whole seconds, the deadline is the reader's
    c->deadline = ctx->deadline_;
  }
  if (service.size() > MUD_MAX_TOPIC) {   // core's -3 for a topic too long, which is the call's own argument
    c->end(Status(INVALID_ARGUMENT, "mudRPC: invalid argument"));
    return c;
  }
  try {
    c->id = peer_.call(now(), service, method, request, life);
    pending_[*c->id] = c;
    signal();
  } catch (const Error& e) {
    signal();   // a full peer frees its slots on a tick
    if (e.code == MUD_ECORRUPT || e.code == MUD_EMEM) {   // -1 is the call's own: a name empty, too long or not UTF-8
      shut(std::string("the peer is closed: ") + e.what());
      c->end(Status(UNAVAILABLE, "the transport is closed"));
    } else {
      c->end(Status(e.code == MUD_EFULL || e.code == MUD_ETOOLARGE || e.code == MUD_ERATE ? RESOURCE_EXHAUSTED : e.code == MUD_EINVAL ? INVALID_ARGUMENT : INTERNAL, e.what()));
    }
  }
  return c;
}

// The next item, in seq order; false at the call's end. A deadline passed cuts off what is unread, unless the call is
// over and its items read.
inline bool Channel::read(internal::Call& c, std::string* out) {
  std::unique_lock<std::mutex> l(m_);
  for (;;) {
    if (!c.cut && c.deadline && internal::Clock::now() >= *c.deadline && !(c.status && c.items.empty())) cut_locked(c, Status(DEADLINE_EXCEEDED, "Deadline Exceeded"));
    if (c.cut) return false;
    if (!c.items.empty()) {
      *out = std::move(c.items.front());
      c.items.pop_front();
      return true;
    }
    if (c.status) return false;
    if (c.deadline) landed_.wait_until(l, *c.deadline);
    else landed_.wait(l);
  }
}

inline Status Channel::finish(internal::Call& c) {
  for (std::string skip; read(c, &skip);) {}   // as gRPC's, after the reader stops: what it left unread is dropped
  std::lock_guard<std::mutex> l(m_);
  return c.cut ? *c.cut : *c.status;
}

inline bool ServerContext::IsCancelled() const {
  if (!t_) return false;
  std::lock_guard<std::mutex> l(t_->m_);
  return x_->killed || t_->closed_ || internal::Clock::now() > x_->until;
}

inline std::string ServerContext::peer() const { return x_ ? "mudrpc:" + std::to_string(x_->call.sender) : ""; }

inline std::chrono::system_clock::time_point ServerContext::deadline() const {
  if (!x_) return std::chrono::system_clock::time_point::max();
  return std::chrono::system_clock::now() + std::chrono::duration_cast<std::chrono::system_clock::duration>(x_->until - internal::Clock::now());
}

// A server stream's reader: the call starts as it is built, as gRPC's.
template <class R> class ClientReader final : public ClientReaderInterface<R> {
 public:
  template <class Q> ClientReader(Channel* t, const char* service, const char* method, ClientContext* ctx, const Q& request)
      : t_(t->shared_from_this()), call_(t->start(service, method, ctx, request.SerializeAsString())) {}
  bool Read(R* msg) override {
    std::string b;
    if (failed_ || !t_->read(*call_, &b)) return false;
    if (msg->ParseFromString(b)) return true;
    failed_ = Status(INTERNAL, "Failed to parse server response");
    t_->cut(*call_, *failed_);
    return false;
  }
  Status Finish() override {
    Status s = t_->finish(*call_);
    return failed_ ? *failed_ : s;
  }

 private:
  std::shared_ptr<Channel> t_;
  std::shared_ptr<internal::Call> call_;
  std::optional<Status> failed_;
};

class Server {
 public:
  Server(const Server&) = delete;
  Server& operator=(const Server&) = delete;
  ~Server() { Shutdown(); }

  // Returns once the server is shut down, or its channel closed, and its handlers have returned.
  void Wait() { t_->stopped(served_); }
  // Takes no new calls; returns once the running ones have finished, cancelling them at the deadline, on any clock.
  void Shutdown() { t_->stop(served_, std::nullopt); }
  template <class C, class D> void Shutdown(const std::chrono::time_point<C, D>& deadline) { t_->stop(served_, internal::steady(deadline)); }

 private:
  friend class ServerBuilder;
  Server(std::shared_ptr<Channel> t, uint64_t served) : t_(std::move(t)), served_(served) {}
  std::shared_ptr<Channel> t_;
  uint64_t served_;
};

class ServerBuilder {
 public:
  // The channel to serve on, in place of an address and its credentials: one server a channel.
  ServerBuilder& AddListeningPort(std::shared_ptr<Channel> channel) {
    t_ = std::move(channel);
    return *this;
  }
  ServerBuilder& RegisterService(Service* service) {
    services_.push_back(service);
    return *this;
  }
  // nullptr, the reason told to the channel's fault, when the channel already serves, is closed, or a service's full name
  // is past 64 bytes.
  std::unique_ptr<Server> BuildAndStart() {
    if (!t_) return nullptr;
    uint64_t served = 0;
    std::string why = t_->serve(services_, &served);
    if (why.empty()) return std::unique_ptr<Server>(new Server(t_, served));
    t_->fault_(std::runtime_error(why));
    return nullptr;
  }

 private:
  std::shared_ptr<Channel> t_;
  std::vector<Service*> services_;
};

namespace internal {
template <class Q, class R> Status BlockingUnaryCall(Channel* t, const char* service, const char* method, ClientContext* ctx, const Q& request, R* response) {
  auto c = t->start(service, method, ctx, request.SerializeAsString());
  std::string b;
  bool got = t->read(*c, &b);
  Status s = t->finish(*c);
  if (!s.ok()) return s;
  if (!got) return Status(INTERNAL, "No message returned for unary request");
  if (!response->ParseFromString(b)) return Status(INTERNAL, "Failed to parse server response");
  return s;
}

template <class Q, class R, class F> Method Unary(F f) {
  return {Method::unary, [f](ServerContext* ctx, const std::string& in, const Write& write) {
            Q q;
            R r;
            if (!q.ParseFromString(in)) return Status(INTERNAL, "Failed to parse request");
            Status s = f(ctx, &q, &r);
            if (s.ok()) write(r.SerializeAsString());
            return s;
          }};
}

template <class Q, class R, class F> Method ServerStreaming(F f) {
  return {Method::server_stream, [f](ServerContext* ctx, const std::string& in, const Write& write) {
            Q q;
            if (!q.ParseFromString(in)) return Status(INTERNAL, "Failed to parse request");
            ServerWriter<R> w(write);
            return f(ctx, &q, &w);
          }};
}

inline Method ClientStreaming() { return {Method::client_stream, nullptr}; }
}  // namespace internal

}  // namespace mud
#endif
