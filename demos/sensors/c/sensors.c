/* Sensor telemetry over UDP in C: a gateway and two sensors, three peers over UDP in one loop, the messages mud gen wrote from sensor.proto over the C ABI. */
#define _POSIX_C_SOURCE 200809L
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>
#include "mudrpc.h"
#include "sensor.h"

static mud_buf S(const char* s) { mud_buf b = { (const uint8_t*)s, strlen(s) }; return b; }
static mud_view V(const char* s) { mud_view v = { (const uint8_t*)s, strlen(s) }; return v; }

static void die(const char* what, int code) { fprintf(stderr, "%s: %d\n", what, code); exit(1); }
static void ok(int32_t r) { if (r < 0) die("mudrpc", r); }

typedef struct { void* mem; mud_udp* udp; } Peer;
typedef struct { char sensor[MUD_MAX_TOPIC + 1]; int32_t centi_degrees; } Seen;
static size_t len;
static Peer gateway, sensors[2];
static Seen seen[2]; static int nseen;   /* the readings the gateway has */
static mud_id pending; static int done;  /* its Calibrate call, and the ack printed */
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

static void open_peer(Peer* p, const char* name, uint32_t sender, const uint8_t* key, const char* bind, const char* peers) {
    mud_config c = { S(name), { key, 32 }, sender, 1, { NULL, 0 }, 0, 0, MUD_CIPHER_DEFAULT };   /* epoch 1; unsigned fleet; a leaf */
    p->mem = malloc(len);
    ok(mud_new(p->mem, len, &c));
    int32_t code; p->udp = mud_udp_new(S(bind), S(peers), &code);
    if (!p->udp) die("udp", code);
}

static void reading(uint32_t t, const mud_incoming* in) {   /* the gateway: a Reading published; on the second, print both and calibrate sensor-2 */
    Reading r; if (Reading_decode(&r, in->bytes.ptr, in->bytes.len) || r.sensor.len > MUD_MAX_TOPIC || nseen == 2) die("reading", -1);
    memcpy(seen[nseen].sensor, r.sensor.ptr, r.sensor.len); seen[nseen].sensor[r.sensor.len] = 0; seen[nseen++].centi_degrees = r.centi_degrees;
    if (nseen < 2) return;
    if (strcmp(seen[0].sensor, seen[1].sensor) > 0) { Seen s = seen[0]; seen[0] = seen[1]; seen[1] = s; }
    for (int i = 0; i < 2; i++) printf("reading %s %d.%02dC\n", seen[i].sensor, seen[i].centi_degrees / 100, seen[i].centi_degrees % 100);
    Offset o = { 50 };
    ok(Sensor_Calibrate(gateway.mem, len, t, &o, 10, &pending));
}

static void calibrate(Peer* p, uint32_t t, const mud_incoming* in) {   /* sensor-2: answer the call */
    Offset o; if (Sensor_method(in) != 0 || Offset_decode(&o, in->bytes.ptr, in->bytes.len)) die("call", -1);
    Ack a = { V("sensor-2"), o.centi_degrees };
    ok(Sensor_Calibrate_reply(p->mem, len, t, in, 0, 0, S(""), &a, 1));
}

static void ack(const mud_incoming* in) {   /* the gateway: the ack */
    Ack a; if (in->status != 0 || Ack_decode(&a, in->bytes.ptr, in->bytes.len)) die("ack", (int)in->status);
    printf("calibrated %.*s by %d.%02dC\n", (int)a.sensor.len, a.sensor.ptr, a.offset_centi_degrees / 100, a.offset_centi_degrees % 100);
    done = 1;
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
        if (kind == MUD_EVENT) reading(t, &in);
        else if (kind == MUD_CALL) calibrate(p, t, &in);
        else if (kind == MUD_REPLY && memcmp(in.id.bytes, pending.bytes, 12) == 0) ack(&in);
    }
    while (mud_tick(p->mem, len, t, &in) == MUD_EXPIRED) die("expired", 0);
}

int main(void) {
    uint8_t key[32]; for (int i = 0; i < 32; i++) key[i] = (uint8_t)(i + 1);   /* the fleet key; a product provisions 32 random bytes to every peer */
    char addr[3][32], others[3][64];
    for (int i = 0; i < 3; i++) sprintf(addr[i], "127.0.0.1:%d", free_port());
    for (int i = 0; i < 3; i++) sprintf(others[i], "%s %s", addr[(i + 1) % 3], addr[(i + 2) % 3]);
    struct timespec t0; clock_gettime(CLOCK_MONOTONIC, &t0); start = t0.tv_sec;
    len = mud_size();
    open_peer(&gateway, "gateway", 1, key, addr[0], others[0]);
    open_peer(&sensors[0], "sensor-1", 2, key, addr[1], others[1]);
    open_peer(&sensors[1], "sensor-2", 3, key, addr[2], others[2]);
    ok(mud_listen(gateway.mem, len, S("sensors.Readings")));
    ok(mud_listen(sensors[1].mem, len, S(Sensor_TOPIC)));

    uint8_t buf[2][MUD_MSG];
    Reading r[2] = { { V("sensor-1"), 2150 }, { V("sensor-2"), 1900 } };
    for (int i = 0; i < 2; i++) {
        size_t n = Reading_encode(&r[i], buf[i], sizeof buf[i]); if (n == MUD_ERR) die("encode", i);
        ok(mud_publish(sensors[i].mem, len, now(), S("sensors.Readings"), (mud_buf){ buf[i], n }, 60));
    }
    Peer* all[3] = { &gateway, &sensors[0], &sensors[1] };   /* the gateway first, so its listens stand before the first reading reaches its socket */
    while (!done) {
        uint32_t t = now();
        for (int i = 0; i < 3; i++) pump(all[i], t);
        nanosleep(&(struct timespec){ 0, 2000000 }, NULL);
    }
    for (int i = 0; i < 3; i++) { ok(mud_drop(all[i]->mem, len)); free(all[i]->mem); mud_udp_free(all[i]->udp); }
    return 0;
}
