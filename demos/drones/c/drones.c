/* gRPC drone telemetry in C: a ground station and a mock drone, two peers over UDP on one machine, the stubs mud gen wrote from drone.proto over the C ABI. */
#define _POSIX_C_SOURCE 200809L
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>
#include "mudrpc.h"
#include "drone.h"

static mud_buf S(const char* s) { mud_buf b = { (const uint8_t*)s, strlen(s) }; return b; }
static mud_view V(const char* s) { mud_view v = { (const uint8_t*)s, strlen(s) }; return v; }

static void die(const char* what, int code) { fprintf(stderr, "%s: %d\n", what, code); exit(1); }
static void ok(int32_t r) { if (r < 0) die("mudrpc", r); }

typedef struct { void* mem; mud_udp* udp; } Peer;
typedef struct { uint32_t status; size_t n, dn; uint8_t bytes[MUD_MSG]; char detail[MUD_MSG]; } Reply;
static size_t len;
static Peer gcs, drone;
static int armed;                        /* the mock drone's state */
static mud_id pending;                   /* the ground station's call in flight */
static Reply replies[4]; static int got, last = -1;   /* its stream, by seq; last is the seq that carried end */
static uint8_t scratch[MUD_MAX_TOPIC + MUD_MSG], rx[MUD_MAX_TOPIC + MUD_MSG], plain[MUD_MSG];
static time_t start;

static uint32_t now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return (uint32_t)(t.tv_sec - start); }

static int free_port(void) {
    int s = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in a = { 0 }; socklen_t n = sizeof a; a.sin_family = AF_INET; a.sin_addr.s_addr = htonl(0x7F000001);
    if (s < 0 || bind(s, (struct sockaddr*)&a, n) || getsockname(s, (struct sockaddr*)&a, &n)) die("port", -1);
    close(s);
    return ntohs(a.sin_port);
}

static void open_peer(Peer* p, const char* name, uint32_t sender, const uint8_t* key, const char* bind, const char* peer) {
    mud_config c = { S(name), { key, 32 }, sender, 1, { NULL, 0 }, 0, 0, MUD_CIPHER_DEFAULT };   /* epoch 1; unsigned fleet; a leaf */
    p->mem = malloc(len);
    ok(mud_new(p->mem, len, &c));
    int32_t code; p->udp = mud_udp_new(S(bind), S(peer), &code);
    if (!p->udp) die("udp", code);
}

static void answer(Peer* p, uint32_t t, const mud_incoming* in) {
    switch (Drone_method(in)) {
    case 0: { ArmReply r = { 1, V("drone-1") }; armed = 1; ok(Drone_Arm_reply(p->mem, len, t, in, 0, 0, S(""), &r, 1)); break; }
    case 1: { TelemetryRequest q; if (TelemetryRequest_decode(&q, in->bytes.ptr, in->bytes.len)) die("decode", -1);
              for (uint32_t i = 0; i < q.samples; i++) { Sample s = { i, (int32_t)(10 * (i + 1)), (int32_t)(100 - 2 * i) }; ok(Drone_Telemetry_reply(p->mem, len, t, in, i, 0, S(""), &s, i + 1 == q.samples)); }
              break; }
    case 2: { LandReply r = { armed }; if (armed) { armed = 0; ok(Drone_Land_reply(p->mem, len, t, in, 0, 0, S(""), &r, 1)); } else ok(Drone_Land_reply(p->mem, len, t, in, 0, 9, S("not armed"), &r, 1)); }   /* 9: FAILED_PRECONDITION */
            break;
    default: die("method", -1);
    }
}

static void keep(const mud_incoming* in) {
    if (in->seq >= 4) die("seq", (int)in->seq);
    Reply* r = &replies[in->seq];
    r->status = in->status; r->n = in->bytes.len; r->dn = in->detail.len;
    memcpy(r->bytes, in->bytes.ptr, r->n); memcpy(r->detail, in->detail.ptr, r->dn);
    got++; if (in->end) last = (int)in->seq;
}

static void pump(Peer* p, uint32_t t) {
    mud_out o; int kind; mud_buf topic, wire; mud_incoming in;
    while ((kind = mud_outgoing(p->mem, len, t, (mud_mutbuf){ scratch, sizeof scratch }, &o)) != MUD_NONE) {
        ok(kind);
        if ((o.kind == MUD_SEND ? mud_udp_send(p->udp, o.topic, o.wire) : mud_udp_listen(p->udp, o.topic)) != MUD_OK) break;
        ok(mud_sent(p->mem, len));
    }
    while (mud_udp_poll(p->udp, (mud_mutbuf){ rx, sizeof rx }, &topic, &wire) == 1) {
        kind = mud_deliver(p->mem, len, t, topic, wire, (mud_mutbuf){ plain, sizeof plain }, &in);
        ok(kind);
        if (kind == MUD_CALL) answer(p, t, &in);
        else if (kind == MUD_REPLY && memcmp(in.id.bytes, pending.bytes, 12) == 0) keep(&in);
    }
    while (mud_tick(p->mem, len, t, &in) == MUD_EXPIRED) die("expired", 0);
}

static void wait_reply(void) {   /* pump both peers until every item of the stream is in */
    got = 0; last = -1;
    while (last < 0 || got != last + 1) {
        uint32_t t = now();
        pump(&drone, t); pump(&gcs, t);   /* the listener first, so its listens stand before the first wire reaches its socket */
        nanosleep(&(struct timespec){ 0, 2000000 }, NULL);
    }
}

int main(void) {
    uint8_t key[32]; for (int i = 0; i < 32; i++) key[i] = (uint8_t)(i + 1);   /* the fleet key; a product provisions 32 random bytes to every peer */
    char a[32], b[32]; sprintf(a, "127.0.0.1:%d", free_port()); sprintf(b, "127.0.0.1:%d", free_port());
    struct timespec t0; clock_gettime(CLOCK_MONOTONIC, &t0); start = t0.tv_sec;
    len = mud_size();
    open_peer(&gcs, "gcs", 1, key, a, b);
    open_peer(&drone, "drone", 2, key, b, a);
    ok(mud_listen(drone.mem, len, S(Drone_TOPIC)));

    LandRequest land = { 0 }; ArmRequest arm = { V("gcs") }; TelemetryRequest tele = { 3 };
    ok(Drone_Land(gcs.mem, len, now(), &land, 10, &pending)); wait_reply();
    if (replies[0].status > 16) die("status", (int)replies[0].status);
    printf("land: %s %.*s\n", mud_status_name(replies[0].status), (int)replies[0].dn, replies[0].detail);

    ok(Drone_Arm(gcs.mem, len, now(), &arm, 10, &pending)); wait_reply();
    ArmReply ar; if (ArmReply_decode(&ar, replies[0].bytes, replies[0].n) || !ar.armed) die("arm", -1);
    printf("armed %.*s for %.*s\n", (int)ar.drone.len, ar.drone.ptr, (int)arm.pilot.len, arm.pilot.ptr);

    ok(Drone_Telemetry(gcs.mem, len, now(), &tele, 10, &pending)); wait_reply();
    printf("telemetry");
    for (int i = 0; i < got; i++) { Sample s; if (Sample_decode(&s, replies[i].bytes, replies[i].n)) die("sample", i); printf(" %u:%dm %d%%", s.seq, s.altitude_m, s.battery_pct); }
    printf("\n");

    ok(Drone_Land(gcs.mem, len, now(), &land, 10, &pending)); wait_reply();
    LandReply lr; if (LandReply_decode(&lr, replies[0].bytes, replies[0].n) || !lr.landed) die("land", -1);
    printf("landed\n");

    ok(mud_drop(gcs.mem, len)); ok(mud_drop(drone.mem, len));
    free(gcs.mem); free(drone.mem); mud_udp_free(gcs.udp); mud_udp_free(drone.udp);
    return 0;
}
