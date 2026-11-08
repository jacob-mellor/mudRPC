// mudRPC for C++: one header over the C ABI, mudrpc.h beside it, and the built library. C++17 and the standard library.
//
// A call is a message; a reply may come hours later or expire; nothing blocks. You poll: drain outgoing(now) into your
// transport and call sent(), feed what arrives to deliver(now, topic, wire), call tick(now). now is your uptime in
// seconds. Bytes are std::string, taken as std::string_view.
#ifndef MUD_HPP
#define MUD_HPP
#include <array>
#include <chrono>
#include <cstring>
#include <memory>
#include <mutex>
#include <new>
#include <optional>
#include <stdexcept>
#include <string>
#include <string_view>
#include <thread>
#include <utility>

#include "mudrpc.h"

namespace mud {

// A code from mudrpc.h's enum, the same number in every binding: -1 invalid argument, -2 full, -3 too large, -4 corrupt,
// -5 bad memory, -6 epoch not higher, -7 unprovisioned, -8 counter exhausted, -9 this second's sends spent, -10 busy.
class Error : public std::runtime_error {
 public:
  explicit Error(int32_t code) : std::runtime_error(describe(code)), code(code) {}
  int32_t code;

  static std::string describe(int32_t code) {
    switch (code) {
      case -1: return "mudRPC: invalid argument";
      case -2: return "mudRPC: full";
      case -3: return "mudRPC: too large";
      case -4: return "mudRPC: corrupt";
      case -5: return "mudRPC: bad memory";
      case -6: return "mudRPC: epoch not higher";
      case -7: return "mudRPC: unprovisioned (sender 0)";
      case -8: return "mudRPC: counter exhausted";
      case -9: return "mudRPC: this second's sends spent";
      case -10: return "mudRPC: busy: another thread holds this peer";
      case -99: return "mudRPC: panic";
      default: return "mudRPC: " + std::to_string(code);
    }
  }
};

enum class Kind : int32_t { none, call, reply, event, expired, dropped, flood, rollback };

// gRPC's name for a status number, "FAILED_PRECONDITION" for 9, "" past 16.
inline const char* status_name(uint32_t status) { return mud_status_name(status); }

// A verdict. dropped names why a wire was refused: 1 version, 2 open, 3 decode, 4 duplicate, 5 no pending call, 6 topic,
// 7 seq, 8 id, 9 replay, 10 rate, with status its detail.
struct Incoming {
  Kind kind = Kind::none;
  std::array<uint8_t, 12> id{};
  std::string topic, method, reply_to, detail, bytes;
  uint32_t ttl_secs = 0, seq = 0, status = 0;
  bool end = false;
  int32_t dropped = 0;
  uint32_t sender = 0;
};

// An item for the transport: a wire to send on topic, kept for ttl_secs, or a topic to listen to.
struct Outgoing {
  bool listen = false;
  std::string topic, wire;
  uint32_t ttl_secs = 0;
};

namespace detail {
inline int32_t check(int32_t code) {
  if (code < 0) throw Error(code);
  return code;
}
inline mud_buf buf(std::string_view s) { return {reinterpret_cast<const uint8_t*>(s.data()), s.size()}; }
inline std::string str(mud_buf b) { return b.len ? std::string(reinterpret_cast<const char*>(b.ptr), b.len) : std::string(); }
struct Free {
  void operator()(uint8_t* p) const { ::operator delete(p, std::align_val_t(mud_align())); }
};
using Block = std::unique_ptr<uint8_t, Free>;
inline Block block(size_t size) {
  Block b(static_cast<uint8_t*>(::operator new(size, std::align_val_t(mud_align()))));
  std::memset(b.get(), 0, size);
  return b;
}
}  // namespace detail

// A mudRPC peer. name is its inbox topic; key the 32-byte fleet key; sender the roster number (0 cannot send); epoch a
// number the host persists beside the key and raises on every boot and restore, written to storage first.
//
// A signed peer (a 32-byte seed) believes 16 senders; hub true is the hub shape, 256, for the host that believes a fleet.
// The line past either is Error -2. Not thread-safe: one thread drives a peer. The bytes from save() contain the key;
// store them as you store the key.
class Peer {
 public:
  static constexpr uint32_t cipher_chacha20_poly1305 = 0, cipher_aes_256_gcm = 1;
  static constexpr uint32_t cipher_default = cipher_aes_256_gcm;   // AES, the NIST algorithm buyers ask for; ChaCha20-Poly1305 smaller and faster without an AES instruction

  Peer(std::string_view name, std::string_view key, uint32_t sender, uint32_t epoch, std::string_view seed = {}, uint32_t budget = 1000, bool hub = false, uint32_t cipher = cipher_default)
      : Peer(hub ? mud_hub_size() : mud_size()) {
    if (key.size() != 32 || (!seed.empty() && seed.size() != 32)) throw Error(-1);
    mud_config c{detail::buf(name), detail::buf(key), sender, epoch, detail::buf(seed), budget, hub, cipher};
    detail::check(mud_new(mem_.get(), size_, &c));
  }

  // A peer from save() bytes, from this same build of the library, with a higher epoch from the host.
  static Peer restore(std::string_view saved, uint32_t epoch) {
    if (saved.size() != mud_size() && saved.size() != mud_hub_size()) throw Error(-4);
    Peer p(saved.size());
    std::memcpy(p.mem_.get(), saved.data(), saved.size());
    detail::check(mud_resume(p.mem_.get(), p.size_, epoch));
    return p;
  }

  Peer(Peer&&) = default;   // a moved-from peer answers -1
  Peer& operator=(Peer&&) = delete;
  ~Peer() { close(); }

  // Ends the peer: its block is zeroed, keys included, once no other thread is inside it. Every later call is Error -4,
  // the counts 0; the memory goes with the Peer.
  void close() {
    for (int i = 0; i < 1000 && mud_drop(mem_.get(), size_) == MUD_EBUSY; i++) std::this_thread::sleep_for(std::chrono::milliseconds(1));   // an entry on another thread, which ends in microseconds
  }

  // The peer's bytes, to write to flash. They contain the key, and cannot send until restore has resumed them.
  std::string save() {
    detail::Block out = detail::block(size_);
    detail::check(mud_save(mem_.get(), size_, out.get(), size_));
    return std::string(reinterpret_cast<char*>(out.get()), size_);
  }

  // A higher epoch for a live peer; clears Error -8.
  void renew(uint32_t epoch) { detail::check(mud_renew(mem_.get(), size_, epoch)); }
  // The next fleet key, before the flag day.
  void stage(std::string_view key) { detail::check(mud_stage(mem_.get(), size_, detail::buf(key))); }
  void rotate() { detail::check(mud_rotate(mem_.get(), size_)); }
  void retire() { detail::check(mud_retire(mem_.get(), size_)); }
  // Which key slot opened the last accepted wire: 0 current, 1 previous, 2 next.
  int32_t opened() { return detail::check(mud_opened(mem_.get(), size_)); }
  // Signed peers: a line for sender, its public key and the next one during a rotation.
  void roster(uint32_t sender, std::string_view key, std::string_view next = {}) { detail::check(mud_roster(mem_.get(), size_, sender, detail::buf(key), detail::buf(next))); }
  void revoke(uint32_t sender) { detail::check(mud_revoke(mem_.get(), size_, sender)); }

  // The 32-byte public key of a seed, for rosters.
  static std::string public_key(std::string_view seed) {
    std::string out(32, '\0');
    detail::check(mud_public_key(detail::buf(seed), reinterpret_cast<uint8_t*>(out.data())));
    return out;
  }

  // A call; its replies carry the id returned. It expires after ttl_secs.
  std::array<uint8_t, 12> call(uint32_t now, std::string_view service, std::string_view method, std::string_view request, uint32_t ttl_secs) {
    mud_id id{};
    detail::check(mud_call(mem_.get(), size_, now, detail::buf(service), detail::buf(method), detail::buf(request), ttl_secs, &id));
    std::array<uint8_t, 12> out;
    std::memcpy(out.data(), id.bytes, 12);
    return out;
  }

  void publish(uint32_t now, std::string_view topic, std::string_view payload, uint32_t ttl_secs) { detail::check(mud_publish(mem_.get(), size_, now, detail::buf(topic), detail::buf(payload), ttl_secs)); }
  void listen(std::string_view topic) { detail::check(mud_listen(mem_.get(), size_, detail::buf(topic))); }

  // Answers a Kind::call verdict: status gRPC's number and detail its text; seq 0..63, end on the last item only.
  void reply(uint32_t now, const Incoming& call, std::string_view payload, uint32_t status = 0, std::string_view detail = {}, uint32_t seq = 0, bool end = true) {
    if (call.kind != Kind::call) throw Error(-1);
    mud_incoming c{};
    c.kind = MUD_CALL;
    std::memcpy(c.id.bytes, call.id.data(), 12);
    c.topic = detail::buf(call.topic);
    c.method = detail::buf(call.method);
    c.reply_to = detail::buf(call.reply_to);
    c.bytes = detail::buf(call.bytes);
    c.ttl_secs = call.ttl_secs;
    c.seq = call.seq;
    c.end = call.end;
    detail::check(mud_reply(mem_.get(), size_, now, &c, seq, status, detail::buf(detail), detail::buf(payload), end));
  }

  // Every wire gets a verdict; a Kind::dropped one names why.
  Incoming deliver(uint32_t now, std::string_view topic, std::string_view wire) {
    mud_incoming out{};
    detail::check(mud_deliver(mem_.get(), size_, now, detail::buf(topic), detail::buf(wire), {plain_.data(), plain_.size()}, &out));
    return incoming(out);   // the verdict points into the topic and the scratch: copied while they live
  }

  std::optional<Incoming> tick(uint32_t now) {
    mud_incoming out{};
    if (detail::check(mud_tick(mem_.get(), size_, now, &out)) == MUD_NONE) return std::nullopt;
    return incoming(out);
  }

  // The next item for the transport, left in place until sent().
  std::optional<Outgoing> outgoing(uint32_t now) {
    mud_out out{};
    int32_t kind = detail::check(mud_outgoing(mem_.get(), size_, now, {scratch_.data(), scratch_.size()}, &out));
    if (kind == MUD_NONE) return std::nullopt;
    return Outgoing{kind == MUD_LISTEN, detail::str(out.topic), detail::str(out.wire), out.ttl_secs};
  }

  void sent() { detail::check(mud_sent(mem_.get(), size_)); }

  // Live seen or sender entries evicted for want of room; rising under load means replay protection is degraded.
  uint32_t pressure() { return mud_pressure(mem_.get(), size_); }
  // Calls in flight, of 16: at 16 a call is Error -2.
  uint32_t pending() { return mud_pending(mem_.get(), size_); }
  // Items queued for the transport, of 16: at 16 every producer is Error -2.
  uint32_t queued() { return mud_queued(mem_.get(), size_); }
  // The operator's counters, kept across restores: managed (sealer taken for a change), rollbacks, floods, dropped.
  struct Counts { uint32_t managed, rollbacks, floods, dropped; };
  Counts counts() { return {mud_count(mem_.get(), size_, 0), mud_count(mem_.get(), size_, 1), mud_count(mem_.get(), size_, 2), mud_count(mem_.get(), size_, 3)}; }

 private:
  explicit Peer(size_t size) : mem_(detail::block(size)), size_(size) {}

  static Incoming incoming(const mud_incoming& r) {
    Incoming e;
    e.kind = static_cast<Kind>(r.kind);
    std::memcpy(e.id.data(), r.id.bytes, 12);
    e.topic = detail::str(r.topic);
    e.method = detail::str(r.method);
    e.reply_to = detail::str(r.reply_to);
    e.detail = detail::str(r.detail);
    e.bytes = detail::str(r.bytes);
    e.ttl_secs = r.ttl_secs;
    e.seq = r.seq;
    e.status = r.status;
    e.end = r.end != 0;
    e.dropped = r.dropped;
    e.sender = r.sender;
    return e;
  }

  detail::Block mem_;   // lives as long as the Peer, so a thread still inside it never meets freed memory
  size_t size_;
  std::array<uint8_t, MUD_MAX_TOPIC + MUD_MSG> scratch_{};
  std::array<uint8_t, MUD_MSG> plain_{};
};

// A product's UDP: each wire to every listed peer, on private addresses alone unless the library was built with the
// cargo feature internet, which no release is. A channel's pump and the owner's close never meet inside the library.
class Udp {
 public:
  // Binds "ip:port" and sends to each peer's "ip:port", apart by spaces or commas, at least one; a bad or public address,
  // or no peer, is Error -1 ("0.0.0.0" is public), a bind the system refuses -2.
  Udp(std::string_view bind, std::string_view peers) {
    int32_t code = MUD_EINVAL;
    udp_ = mud_udp_new(detail::buf(bind), detail::buf(peers), &code);
    if (!udp_) throw Error(code);
  }
  Udp(const Udp&) = delete;
  Udp& operator=(const Udp&) = delete;
  ~Udp() { close(); }

  // Whether this library may bind or send to a public address.
  static bool internet() { return mud_internet() == 1; }

  // The bound port, the one chosen for port 0.
  int32_t port() {
    std::lock_guard<std::mutex> l(held_);
    return detail::check(mud_udp_port(live()));
  }

  // One outgoing item: a listen keeps wires on its topic from then, every other topic dropped unread; a send throws -2
  // when no peer took the wire, so the item stays queued.
  void send(const Outgoing& item) {
    std::lock_guard<std::mutex> l(held_);
    detail::check(item.listen ? mud_udp_listen(live(), detail::buf(item.topic)) : mud_udp_send(live(), detail::buf(item.topic), detail::buf(item.wire)));
  }

  // The next (topic, wire) from a listed peer, or none.
  std::optional<std::pair<std::string, std::string>> poll() {
    std::lock_guard<std::mutex> l(held_);
    mud_buf topic{}, wire{};
    if (detail::check(mud_udp_poll(live(), {scratch_.data(), scratch_.size()}, &topic, &wire)) != 1) return std::nullopt;
    return std::make_pair(detail::str(topic), detail::str(wire));
  }

  void close() {
    std::lock_guard<std::mutex> l(held_);
    mud_udp_free(udp_);
    udp_ = nullptr;
  }

 private:
  mud_udp* live() {
    if (!udp_) throw Error(-1);   // the socket is closed
    return udp_;
  }

  std::mutex held_;
  mud_udp* udp_ = nullptr;
  std::array<uint8_t, MUD_MAX_TOPIC + MUD_MSG> scratch_{};
};

}  // namespace mud
#endif
