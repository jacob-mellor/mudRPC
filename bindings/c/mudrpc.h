/* mudRPC C ABI. See SKILL.md. The caller owns all memory; nothing here allocates or keeps a pointer past the call.
   The host contract, the same in every binding:
   - The epoch is persisted beside the key, written to storage before the peer is created, and raised on every boot and restore (mud_resume) or renewal (mud_renew); the check is against the saved epoch only, so the host alone keeps one from being handed out twice. 2^32 sends under one epoch is MUD_EEXHAUSTED until renewed.
   - Every nonce is sender ‖ epoch ‖ counter: one roster number per device (0 cannot send: MUD_EUNPROVISIONED) and an epoch that never repeats for it, so a restore, clone or reflash is a provisioning event. Under one fleet key a verdict's sender is the number the sender wrote; a signed fleet proves it, but for MUD_ROLLBACK, judged before the signature.
   - mud_save gives bytes that contain the key: store them as the key. The copy cannot send (MUD_ECORRUPT) until restored with a higher epoch (MUD_EEPOCH otherwise) by the same build, which reads the shape and the cipher from the saved bytes.
   - A restore expires what was in flight: uptime restarted, so every pending call is MUD_EXPIRED by id on the first mud_tick and queued output is dropped; re-issue what still matters. mud_resume re-queues the peer's own listen; your other listen calls are yours to repeat.
   - A receiver remembers the newest 64 counters per sender, so a replay is dropped however long its ttl. The cost: a wire 64 behind its sender's newest is dropped as a replay too, seen or not, so a message that must land late must not be overtaken by 64 newer ones from its sender.
   - The ttl is the transport's promise, which core cannot check: a hostile relay can hold a wire past it and hand it to a receiver that never heard that sender, so a time-sensitive command carries its own timestamp in its payload.
   - One thread drives a peer; it is not thread-safe, and the block says so when it is broken: every entry below that takes a peer's block and returns a code holds its busy byte while it runs, and a second caller that finds it held is MUD_EBUSY with nothing done. A detector of the host's fault, not a lock; mud_new builds over whatever mem holds, and mud_resume alone takes a held block and clears the byte when it succeeds, so a block an entry a crash or a panic ended is MUD_EBUSY, unsaved and undropped, until a higher epoch resumes it. now is uptime in seconds; now and ttl are unsigned 32-bit, a ttl of 0 is 1 and one above a week a week.
   - Names, services and topics are UTF-8, 1 to 64 bytes (empty is MUD_EINVAL), methods are not; a payload above about 460 bytes, 396 signed, is MUD_ETOOLARGE, since the 512-byte wire carries the header and the names.
   - Every error is a code in the enum below, the same number in every wrapper; a struct pointer misaligned for its type, or a buffer or struct sharing a byte with the block, is MUD_EINVAL. MUD_EFULL is 16 calls pending (a reply, or mud_tick past the ttl, frees one) or 16 items queued undrained (the peer's own listen is one): mud_pending and mud_queued say how near, and the product decides what not to queue.
   - mud_drop, close in a wrapper, zeroes the block, keys included; every later call is MUD_ECORRUPT, or -1 in a wrapper, which frees the block (counts and pressure read 0).
   - mud_count and mud_pressure are the operator's dwell signal: a managed count (a key staged, rotated or retired, a roster line added or removed) that rises when nobody ordered a change is something on the host rekeying the unit.
   - MUD_CIPHER_DEFAULT is AES-256-GCM, the NIST algorithm buyers ask for; ChaCha20-Poly1305 is smaller and faster without an AES instruction. A fleet runs one cipher: a peer drops the other's wires by version. */
#ifndef MUDRPC_H
#define MUDRPC_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

typedef struct { const uint8_t* ptr; size_t len; } mud_buf;      /* bytes or UTF-8 text, not NUL-terminated: ptr readable for len bytes, null only when len is 0 */
typedef struct { uint8_t* ptr; size_t len; } mud_mutbuf;   /* the caller's writable bytes: ptr writable for len bytes, never null */
typedef struct { uint8_t bytes[12]; } mud_id;   /* the call's nonce: sender ‖ epoch ‖ counter */
typedef struct { mud_buf name; mud_buf key; uint32_t sender; uint32_t epoch; mud_buf seed; uint32_t budget; uint32_t hub; uint32_t cipher; } mud_config;  /* what mud_new builds a peer from; anything else is MUD_EINVAL. name: the peer's inbox topic; key: exactly 32 bytes and not 32 equal ones (a zeroed config, an example's constant), as for seed and mud_stage; sender: roster number, 0 cannot send; epoch: persisted beside the key, higher every boot; seed: empty for an unsigned peer, this sender's 32-byte Ed25519 seed for a signed one (wire version 2, or 4 under AES); budget: verifications per second a signed receiver affords, at least 1; hub: 0 a leaf (MUD_ROSTER lines in mud_size() bytes), 1 a hub (MUD_HUB_ROSTER lines in mud_hub_size() bytes, a seed required); cipher: a MUD_CIPHER_ below */
enum { MUD_CIPHER_CHACHA20_POLY1305 = 0, MUD_CIPHER_AES_256_GCM = 1, MUD_CIPHER_DEFAULT = MUD_CIPHER_AES_256_GCM };   /* wire versions 1 and 2 signed; 3 and 4 signed; a zeroed config is ChaCha, so name the cipher */

typedef struct {                  /* a verdict from mud_deliver or mud_tick; fields a kind does not use are zero or empty */
    int32_t kind;                 /* MUD_NONE, MUD_CALL, MUD_REPLY, MUD_EVENT, MUD_EXPIRED, MUD_DROPPED, MUD_FLOOD, MUD_ROLLBACK */
    mud_id id;                    /* the call's: MUD_CALL, MUD_REPLY, MUD_EXPIRED */
    mud_buf topic, method, reply_to, detail, bytes;   /* point into your scratch and topic buffers */
    uint32_t ttl_secs, seq, status;   /* ttl_secs a call's; seq a call's or a reply's stream; status a reply's, or the detail of a drop */
    int32_t end;                  /* 1 on the last of a call's or a reply's stream */
    int32_t dropped;              /* MUD_DROPPED: 1 version, 2 open, 3 decode, 4 duplicate, 5 no pending call, 6 topic, or a call's reply_to, empty or too long, 7 seq beyond the stream bound or its lowest end, or not 0 on a call,
                                     8 id field disagrees with the header, 9 replay, behind the sender's window or floor, 10 rate.
                                     status carries detail: the version byte; open 1 short 2 tag 3 scratch too small 4 signature 5 no roster line 6 budget; decode 1..7 */
    uint32_t sender;              /* the sender's roster number: MUD_CALL, MUD_REPLY, MUD_EVENT, MUD_FLOOD, MUD_ROLLBACK; what the sender wrote, or proven under a signed fleet but for MUD_ROLLBACK */
} mud_incoming;

typedef struct { int32_t kind; mud_buf topic, wire; uint32_t ttl_secs; } mud_out;   /* an item for the transport: kind MUD_SEND with topic, wire and ttl_secs, or MUD_LISTEN with topic; both point into your scratch */

enum { MUD_OK = 0, MUD_EINVAL = -1, MUD_EFULL = -2, MUD_ETOOLARGE = -3, MUD_ECORRUPT = -4, MUD_EMEM = -5, MUD_EEPOCH = -6, MUD_EUNPROVISIONED = -7, MUD_EEXHAUSTED = -8, MUD_ERATE = -9, MUD_EBUSY = -10, MUD_EPANIC = -99 };   /* MUD_OK done; MUD_EINVAL an argument refused: a null or misaligned block, a struct misaligned, a buffer null, too short or on the block's bytes, text not UTF-8, a name or topic empty, a value out of range; MUD_EFULL no pending or outbox slot, a full roster, or a simulated radio down; MUD_ETOOLARGE a topic over MUD_MAX_TOPIC, a message over MUD_MSG, a seq at 64, or mud_gen's out too small; MUD_ECORRUPT bytes that are not a live peer's, a block mud_resume refuses, or one restored and never resumed; MUD_EMEM a block null, misaligned or smaller than its shape; MUD_EEPOCH an epoch not above the current one; MUD_EUNPROVISIONED a send from sender 0; MUD_EEXHAUSTED the counter under this epoch is spent: mud_renew; MUD_ERATE this second's 32 sends are spent: the next second sends; MUD_EBUSY another entry holds the block, two threads on one peer: nothing was done; MUD_EPANIC a panic inside the library, while it unwinds; built without std there is no panic to catch: the tripwire proves none is reachable */
enum { MUD_NONE = 0, MUD_CALL = 1, MUD_REPLY = 2, MUD_EVENT = 3, MUD_EXPIRED = 4, MUD_DROPPED = 5, MUD_FLOOD = 6, MUD_ROLLBACK = 7 };   /* MUD_NONE nothing: no verdict, or nothing queued; MUD_CALL a call for this peer to answer with mud_reply; MUD_REPLY a reply to a call of ours, end closes the stream; MUD_EVENT a publish on a topic this peer listens to; MUD_EXPIRED a call of ours whose ttl passed with the stream open; MUD_DROPPED a wire refused, dropped and status say why; MUD_FLOOD a sender over 64 a second, over its share of a second the 256 cap spent, or the one whose wire came after 64 from senders with no window landed that second (the fleet past its windows, not a flooder: size the fleet or sign), reported once a second; MUD_ROLLBACK a sender whose epoch went back: a cloned or rolled-back device */
enum { MUD_SEND = 1, MUD_LISTEN = 2 };   /* MUD_SEND wire to send on topic, kept for ttl_secs; MUD_LISTEN topic to announce and subscribe to */
#define MUD_MAX_TOPIC 64
#define MUD_MSG 512
#define MUD_ROSTER 16         /* senders a leaf believes; a hub MUD_HUB_ROSTER; the line past either is MUD_EFULL */
#define MUD_HUB_ROSTER 256

size_t  mud_size(void);                                   /* bytes a leaf needs */
size_t  mud_hub_size(void);                               /* bytes a hub needs */
size_t  mud_align(void);                                  /* the alignment every block and every saved copy needs */
int32_t mud_new(void* mem, size_t len, const mud_config* config);   /* every entry takes mem's len after it: under 4 bytes or under the shape its tag names is MUD_EMEM, never read past; every entry below answers MUD_EINVAL to a null or misaligned mem and MUD_ECORRUPT to bytes that are not a peer's */
int32_t mud_save(void* mem, size_t len, void* out, size_t out_len);   /* the peer's bytes for storage: out is its shape's size (mud_size() or mud_hub_size()) aligned to mud_align(), sharing no byte with mem; the copy cannot send until mud_resume */
int32_t mud_resume(void* mem, size_t len, uint32_t epoch); /* after restoring saved bytes into mem: a higher epoch, written to storage first */
int32_t mud_renew(void* mem, size_t len, uint32_t epoch); /* a higher epoch for a live peer; clears MUD_EEXHAUSTED */
int32_t mud_drop(void* mem, size_t len);                  /* ends the peer: the block is zeroed, keys included; then free mem yourself */
int32_t mud_stage(void* mem, size_t len, mud_buf key);    /* the next fleet key, 32 bytes and not 32 equal ones: the first of a rotation's three steps */
int32_t mud_rotate(void* mem, size_t len);                /* previous <- current <- next: the peer seals under the staged key and still opens the old one */
int32_t mud_retire(void* mem, size_t len);                /* drops the previous fleet key, once every peer has rotated */
int32_t mud_roster(void* mem, size_t len, uint32_t sender, mud_buf key, mud_buf next); /* signed peers: a line for sender, its public key and the next one during a rotation (empty otherwise); MUD_EINVAL for a key anyone could sign for or an unsigned peer, MUD_EFULL for a full roster */
int32_t mud_revoke(void* mem, size_t len, uint32_t sender); /* signed peers: remove the line */
int32_t mud_public_key(mud_buf seed, uint8_t* out);       /* 32 bytes, for building rosters */
int32_t mud_opened(void* mem, size_t len);                /* key slot that opened the last accepted wire: 0 current 1 previous 2 next */
int32_t mud_call(void* mem, size_t len, uint32_t now, mud_buf service, mud_buf method, mud_buf req, uint32_t ttl_secs, mud_id* out); /* out: the id its replies will carry; the call expires after ttl_secs */
int32_t mud_publish(void* mem, size_t len, uint32_t now, mud_buf topic, mud_buf payload, uint32_t ttl_secs); /* for every listener, no reply; kept by the mesh for ttl_secs */
int32_t mud_listen(void* mem, size_t len, mud_buf topic); /* its events arrive as MUD_EVENT, after mud_outgoing hands the transport its MUD_LISTEN */
int32_t mud_reply(void* mem, size_t len, uint32_t now, const mud_incoming* call, uint32_t seq, uint32_t status, mud_buf detail, mud_buf payload, int32_t end); /* answers a MUD_CALL exactly as mud_deliver filled it, status gRPC's number and detail its UTF-8 text, empty for none: seq 0..63, end 1 once, on the last item and on a unary reply; the caller completes when every seq up to the lowest end it has seen has arrived, and drops an item above that end */
int32_t mud_deliver(void* mem, size_t len, uint32_t now, mud_buf topic, mud_buf wire, mud_mutbuf scratch, mud_incoming* out); /* returns kind; scratch >= MUD_MSG */
int32_t mud_tick(void* mem, size_t len, uint32_t now, mud_incoming* out);                                           /* MUD_EXPIRED or MUD_NONE */
int32_t mud_outgoing(void* mem, size_t len, uint32_t now, mud_mutbuf scratch, mud_out* out);                  /* returns kind; scratch >= MUD_MAX_TOPIC + MUD_MSG */
int32_t mud_sent(void* mem, size_t len);                                                                            /* the transport took the item mud_outgoing last showed; with nothing shown, nothing happens */
uint32_t mud_pressure(void* mem, size_t len);           /* live seen or sender entries evicted, or senders refused a window, for want of room; rising under load means replay protection is degraded */
uint32_t mud_count(void* mem, size_t len, int32_t which); /* the operator's counters, kept across restores: 0 managed (sealer taken for a change), 1 rollbacks, 2 floods, 3 dropped but duplicates */
uint32_t mud_pending(void* mem, size_t len);            /* calls in flight, of 16: at 16 mud_call is MUD_EFULL until a reply or mud_tick frees one */
uint32_t mud_queued(void* mem, size_t len);             /* items queued for the transport, of 16: at 16 every producer is MUD_EFULL until mud_sent, or mud_outgoing drops a wire past its ttl */

const char* mud_status_name(uint32_t status);             /* gRPC's name for a status number, "FAILED_PRECONDITION" for 9, "" past 16: a C string that lives as long as the library */

/* Development tools, not core: the simulator and the generator. These allocate; nothing above does. */
typedef struct mud_udp mud_udp;   /* a product's UDP socket and the peers it sends to; free with mud_udp_free */
int32_t  mud_internet(void);                                          /* 1 when built with the cargo feature `internet`, so a public address may be bound or sent to; 0 in every release */
mud_udp* mud_udp_new(mud_buf bind, mud_buf peers, int32_t* code);   /* bind "ip:port", peers "ip:port" apart by spaces or commas; NULL with *code MUD_EINVAL for bad text or a public address without `internet` (0.0.0.0 is public), MUD_EFULL when the system refuses the bind */
void     mud_udp_free(mud_udp*);                                     /* NULL is ignored */
int32_t  mud_udp_port(mud_udp*);                                     /* the bound port, the one chosen for port 0 */
int32_t  mud_udp_listen(mud_udp*, mud_buf topic);                    /* a peer's listen item: wires on this topic are kept, on any other dropped unread; MUD_EINVAL for a bad argument */
int32_t  mud_udp_send(mud_udp*, mud_buf topic, mud_buf wire);        /* to every listed peer: MUD_OK when one took it, MUD_EFULL when none did (keep the item, send it again), MUD_EINVAL for a bad argument */
int32_t  mud_udp_poll(mud_udp*, mud_mutbuf scratch, mud_buf* topic, mud_buf* wire);   /* 1 a wire from a listed peer on a topic listened to (topic and wire in scratch, MUD_MAX_TOPIC + MUD_MSG bytes), MUD_NONE when none waits; anything else is dropped unread */
typedef struct mud_sim mud_sim;   /* a simulated mesh for one scenario; free with mud_sim_free */
mud_sim* mud_sim_new(mud_buf scenario_text, int32_t* line);         /* one radio per peer; NULL on error, with the failing line (0 for a missing header) */
void     mud_sim_free(mud_sim*);                                     /* NULL is ignored */
int32_t  mud_sim_peers(mud_sim*);                                    /* the scenario's peers line: the radio count */
uint64_t mud_sim_last_event_ms(mud_sim*);                            /* the latest event's at_ms, from which a harness runs on for its tail */
uint64_t mud_sim_now_ms(mud_sim*);                                   /* scenario time, in milliseconds */
void     mud_sim_step(mud_sim*, uint64_t dt_ms);                     /* one round of the world: the radio events now due, messages moved and expired, link changes reported, then the clock on dt_ms */
void     mud_sim_note(mud_sim*, mud_buf text);                        /* a host line in the log at the current time: a harness's peer events and host events */
int32_t  mud_sim_send(mud_sim*, int32_t radio, mud_buf topic, mud_buf wire, uint32_t ttl_secs);   /* kept for ttl_secs; MUD_EINVAL for a bad argument, MUD_EFULL when the radio is down */
int32_t  mud_sim_listen(mud_sim*, int32_t radio, mud_buf topic);    /* MUD_EFULL when the radio is down */
int32_t  mud_sim_poll(mud_sim*, int32_t radio, mud_mutbuf scratch, mud_buf* topic, mud_buf* wire);  /* 1 message, 2 on-mesh, 3 off-mesh, 0 none */
void     mud_sim_silent(mud_sim*, int32_t radio, int32_t on);        /* sends nothing while on is nonzero, and still receives */
int32_t  mud_sim_log(mud_sim*, mud_mutbuf out);                       /* the world's log, one line per entry: bytes needed; filled when out is large enough */
typedef struct { uint64_t at_ms; int32_t kind; uint64_t a, b; mud_buf text; } mud_event;   /* one scenario event: at_ms when; kind 1 partition 2 heal 3 jam 4 unjam (heal a) 5 malformed 6 replay 7 flood 8 bandwidth 9 peer_down 10 peer_up 18 noise 23 hold, which the world applies, or a host event the harness performs: 11 clone 12 spam 13 stage 14 rotate 15 retire 16 warp 17 hostile 19 stale_reboot 20 torn_save 21 revoke 22 telemetry; a the first radio or warp's milliseconds; b the second radio, a rate, bytes per second, noise or hold's milliseconds; text the topic (malformed, flood) or the method (hostile) */
int32_t  mud_sim_events(mud_sim*);                                   /* the scenario's event lines, for mud_sim_event */
int32_t  mud_sim_event(mud_sim*, int32_t i, mud_event* out);        /* event i in file order; text points into the simulator and lives as long as it does */
int32_t  mud_gen(mud_buf lang, mud_buf proto_text, mud_mutbuf out, int32_t* line);   /* "rust" | "python" | "c" */

#ifdef __cplusplus
}
#endif
#endif
