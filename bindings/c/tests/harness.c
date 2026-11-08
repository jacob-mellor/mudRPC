/* The standard harness in C: must produce the same log as adapters/harness.rs, byte for byte.
   usage: harness <scenario file> [--signed] [--gcm]   (prints the log) */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "mudrpc.h"

#define MAXP 1024   /* the simulator's ceiling */
static mud_buf S(const char* s) { mud_buf b = { (const uint8_t*)s, strlen(s) }; return b; }

static const char* dropped_name(const mud_incoming* in, char* buf) {
    static const char* decode[] = { "", "Codec(Truncated)", "Codec(Overflow)", "Codec(Tag)", "Codec(Full)", "Codec(Utf8)", "Missing", "Id" };
    static const char* sealed[] = { "", "Open(Short)", "Open(Tag)", "Open(Full)", "Open(Sig)", "Open(Roster)", "Open(Budget)" };
    static const char* plain[] = { "", "", "", "", "Duplicate", "NoPending", "Topic", "Seq", "Id", "Replay", "Rate" };
    switch (in->dropped) {
        case 1: sprintf(buf, "Version(%u)", in->status); return buf;
        case 2: return in->status >= 1 && in->status <= 6 ? sealed[in->status] : "?";
        case 3: sprintf(buf, "Decode(%s)", in->status < 8 ? decode[in->status] : "?"); return buf;
        default: return in->dropped >= 4 && in->dropped <= 10 ? plain[in->dropped] : "?";
    }
}

/* rpc::Error's name for a refused call, publish or reply, as the Rust harness logs it */
static const char* error_name(int32_t r) {
    static const char* names[] = { "", "", "Full", "TooLarge", "Corrupt", "", "Epoch", "Unprovisioned", "Exhausted", "Rate" };
    return r <= -2 && r >= -9 && r != -5 ? names[-r] : "?";
}

/* Rust's {:?} for a str: quotes and backslash escaped, \t \r \n \0 by name, other ASCII controls as \u{xx}. */
static char qa[2048], qb[2048];
static char* q(const uint8_t* p, size_t n, char* out) {
    char* o = out; *o++ = '"';
    for (size_t i = 0; i < n && o < out + 2000; i++) {
        uint8_t c = p[i];
        if (c == '"' || c == '\\') { *o++ = '\\'; *o++ = (char)c; }
        else if (c == '\n') { *o++ = '\\'; *o++ = 'n'; } else if (c == '\r') { *o++ = '\\'; *o++ = 'r'; } else if (c == '\t') { *o++ = '\\'; *o++ = 't'; } else if (c == 0) { *o++ = '\\'; *o++ = '0'; }
        else if (c < 0x20 || c == 0x7f) o += sprintf(o, "\\u{%x}", c);
        else *o++ = (char)c;
    }
    *o++ = '"'; *o = 0; return out;
}

static char (*notes)[160]; static int* kinded; static int cap;   /* a round's notes, grown as it needs: junk at 20 000 a second is 2 000 a round a radio */
static void room(int nn) {
    if (nn < cap) return;
    cap = cap ? 2 * cap : 4096; notes = realloc(notes, (size_t)cap * sizeof *notes); kinded = realloc(kinded, (size_t)cap * sizeof *kinded);
    if (!notes || !kinded) exit(1);
}

static int signed_fleet, aes, npeers;
static int hub(uint32_t sender) { return signed_fleet && sender == 1; }   /* gcs is the hub */
static size_t psize(uint32_t sender) { return hub(sender) ? mud_hub_size() : mud_size(); }
static void* new_peer(const char* name, uint32_t sender, uint32_t epoch, int listen_nav) {
    uint8_t key[32], seed[32]; for (int i = 0; i < 32; i++) { key[i] = (uint8_t)i; seed[i] = (uint8_t)(sender + i); }   /* counting bytes, as every harness derives them; a constant secret is refused */
    void* p = malloc(psize(sender));
    mud_config c = { S(name), { key, 32 }, sender, epoch, { seed, signed_fleet ? 32 : 0 }, 1000, (uint32_t)hub(sender), (uint32_t)aes };
    if (mud_new(p, psize(sender), &c) != MUD_OK) exit(1);
    if (signed_fleet) for (uint32_t s = 1; s <= (uint32_t)npeers; s++) {
        uint8_t sd[32], pk[32]; for (int i = 0; i < 32; i++) sd[i] = (uint8_t)(s + i);
        if (mud_public_key((mud_buf){ sd, 32 }, pk) != MUD_OK) exit(1);
        int r = mud_roster(p, psize(sender), s, (mud_buf){ pk, 32 }, (mud_buf){ NULL, 0 }); if (r == MUD_EFULL) break; if (r != MUD_OK) exit(1);   /* a leaf believes the hub and the drones that fit */
    }
    if (listen_nav && mud_listen(p, psize(sender), S("nav")) != MUD_OK) exit(1);
    return p;
}

int main(int argc, char** argv) {
    if (argc < 2) return 2;
    FILE* f = fopen(argv[1], "rb"); if (!f || fseek(f, 0, SEEK_END)) return 2;
    long size = ftell(f); rewind(f); char* text = size < 0 ? NULL : malloc((size_t)size + 1); if (!text) return 2;
    size_t tl = fread(text, 1, (size_t)size, f); fclose(f); text[tl] = 0;
    int32_t line = 0;
    mud_sim* sim = mud_sim_new(S(text), &line);
    if (!sim) { fprintf(stderr, "%s:%d: scenario error\n", argv[1], line); return 1; }
    int n = mud_sim_peers(sim); if (n > MAXP) return 1;
    npeers = n;
    for (int a = 2; a < argc; a++) { signed_fleet |= strcmp(argv[a], "--signed") == 0; aes |= strcmp(argv[a], "--gcm") == 0; }
    uint8_t next_key[32]; for (int i = 0; i < 32; i++) next_key[i] = (uint8_t)(32 + i);
    char names[MAXP][32];
    void* peers[MAXP] = { 0 }; size_t lens[MAXP] = { 0 };   /* each block with its length, as every entry takes it */
    for (int i = 0; i < n; i++) { if (i == 0) strcpy(names[i], "gcs"); else sprintf(names[i], "drone-%d", i); }
    /* host events, in time order and file order within a tick; a clone's radio stays empty until its event */
    int ne = mud_sim_events(sim);
    mud_event* host = calloc((size_t)ne + 1, sizeof *host); int nh = 0;
    for (int i = 0; i < ne; i++) { mud_event e; mud_sim_event(sim, i, &e); if (e.kind >= 11 || e.kind == 7) host[nh++] = e; }
    for (int i = 1; i < nh; i++) { mud_event e = host[i]; int j = i; while (j > 0 && host[j - 1].at_ms > e.at_ms) { host[j] = host[j - 1]; j--; } host[j] = e; }
    int is_clone[MAXP] = { 0 };
    for (int i = 0; i < nh; i++) if (host[i].kind == 11) is_clone[host[i].b] = 1;
    for (int i = 0; i < n; i++) if (!is_clone[i]) { peers[i] = new_peer(names[i], (uint32_t)i + 1, 2, i > 0); lens[i] = psize((uint32_t)i + 1); }
    uint8_t* images[MAXP] = { 0 };   /* what each host wrote to flash at boot */
    for (int i = 0; i < n; i++) if (peers[i]) { images[i] = malloc(psize((uint32_t)i + 1)); memcpy(images[i], peers[i], psize((uint32_t)i + 1)); }
    uint64_t end = mud_sim_last_event_ms(sim) + 50000;
    static uint8_t scratch[MUD_MAX_TOPIC + MUD_MSG], plain[MUD_MSG], rx[MUD_MAX_TOPIC + MUD_MSG];
    struct { int peer; uint32_t rate; uint32_t clock; } spam[MAXP]; int ns = 0;
    int ping = 2, hi = 0, flooding[MAXP] = { 0 }, landed[MAXP] = { 0 };   /* radios flooding; events each peer took this second */
    char note[512];
    uint64_t round = 0;   /* gcs's cadence counts rounds, so a warp off the round grid keeps it */
    for (uint64_t t = 0; t <= end;) {
        uint32_t now = (uint32_t)(t / 1000);
        int warped = 0, any = 0;
        if (round % 10 == 0) {
            for (int i = 0; i < n; i++) any |= flooding[i];
            for (int i = 0; i < n; i++) { if (landed[i] > 0 && any) { sprintf(note, "peer%d honest events landed this second: %d", i, landed[i]); mud_sim_note(sim, S(note)); } landed[i] = 0; }
        }
        while (hi < nh && host[hi].at_ms <= t) {
            mud_event e = host[hi++];
            switch (e.kind) {
                case 11: peers[e.b] = new_peer(names[e.a], (uint32_t)e.a + 1, 1, 1); lens[e.b] = psize((uint32_t)e.a + 1); sprintf(note, "host: radio %llu is a clone of peer%llu from an old image", (unsigned long long)e.b, (unsigned long long)e.a); mud_sim_note(sim, S(note)); break;
                case 12: { int k = 0; while (k < ns && spam[k].peer != (int)e.a) { k++; } if (k < ns) { spam[k] = spam[--ns]; } if (e.b > 0) { spam[ns].peer = (int)e.a; spam[ns].rate = (uint32_t)e.b; spam[ns].clock = 1000000; ns++; }
                          sprintf(note, "host: peer%llu spams at %llu/s", (unsigned long long)e.a, (unsigned long long)e.b); mud_sim_note(sim, S(note)); break; }
                case 13: for (int i = 0; i < n; i++) if (peers[i]) mud_stage(peers[i], lens[i], (mud_buf){ next_key, 32 }); mud_sim_note(sim, S("host: every peer stages the next key")); break;
                case 14: if (peers[e.a]) mud_rotate(peers[e.a], lens[e.a]); sprintf(note, "host: peer%llu rotates", (unsigned long long)e.a); mud_sim_note(sim, S(note)); break;
                case 15: for (int i = 0; i < n; i++) if (peers[i]) mud_retire(peers[i], lens[i]); mud_sim_note(sim, S("host: every peer retires the previous key")); break;
                case 7: flooding[e.a] = e.b > 0; break;
                case 16: sprintf(note, "host: the clock jumps %llu ms", (unsigned long long)e.a); mud_sim_note(sim, S(note)); mud_sim_step(sim, e.a); t += e.a; if (end < t + 50000) end = t + 50000; warped = 1; break;
                case 19: if (images[e.a]) { memcpy(peers[e.a], images[e.a], psize((uint32_t)e.a + 1)); if (mud_resume(peers[e.a], psize((uint32_t)e.a + 1), 3) != MUD_OK) exit(1); sprintf(note, "host: peer%llu reboots from its boot image at epoch 3, which its storage never kept", (unsigned long long)e.a); mud_sim_note(sim, S(note)); } break;
                case 20: if (images[e.a] && peers[e.a]) {
                    size_t size = psize((uint32_t)e.a + 1), half = size / 2; memcpy((uint8_t*)peers[e.a] + half, images[e.a] + half, size - half);   /* the live block is the latest save; its second half is torn back to the boot image */
                    if (mud_resume(peers[e.a], size, 3) == MUD_OK) sprintf(note, "host: peer%llu reboots from a torn block, accepted", (unsigned long long)e.a);
                    else { memcpy(peers[e.a], images[e.a], size); if (mud_resume(peers[e.a], size, 3) != MUD_OK) exit(1); sprintf(note, "host: peer%llu reboots from a torn block, refused as Corrupt; booting from the boot image", (unsigned long long)e.a); }
                    mud_sim_note(sim, S(note)); } break;
                case 21: if (signed_fleet && peers[e.a]) mud_revoke(peers[e.a], lens[e.a], (uint32_t)e.b + 1); sprintf(note, "host: peer%llu revokes peer%llu's roster line", (unsigned long long)e.a, (unsigned long long)e.b); mud_sim_note(sim, S(note)); break;
                case 22: mud_sim_note(sim, S("host: every drone publishes its position to gcs"));
                         for (int i = 1; i < n; i++) { int32_t r = peers[i] ? mud_publish(peers[i], lens[i], now, S("gcs"), S("pos"), 600) : MUD_OK; if (r != MUD_OK) { sprintf(note, "peer%d refused publish %s", i, error_name(r)); mud_sim_note(sim, S(note)); } }
                         break;
                case 17: { sprintf(note, "host: peer%llu calls a hostile method", (unsigned long long)e.a); mud_sim_note(sim, S(note)); mud_id id;
                           int32_t r = peers[e.a] ? mud_call(peers[e.a], lens[e.a], now, S("nav"), e.text, S("x"), 20, &id) : MUD_OK; if (r != MUD_OK) { sprintf(note, "peer%llu refused call %s", (unsigned long long)e.a, error_name(r)); mud_sim_note(sim, S(note)); }
                           break; }
                default: break;
            }
            if (warped) break;
        }
        if (warped) continue;
        if (peers[0]) {
            mud_id id;
            if (round == 0) {
                mud_call(peers[0], lens[0], now, S("nav"), S("Ping"), S("1"), 60, &id);
                mud_call(peers[0], lens[0], now, S("nav"), S("Ping"), S("2"), 10, &id);
                mud_sim_note(sim, S("gcs calls Ping#1 ttl 60 and Ping#2 ttl 10"));
            } else if (round % 150 == 0) {
                ping++;
                char body[16]; sprintf(body, "%d", ping);
                int32_t r = mud_call(peers[0], lens[0], now, S("nav"), S("Ping"), S(body), 20, &id);
                if (r == MUD_OK) sprintf(note, "gcs calls Ping#%d ttl 20", ping); else sprintf(note, "peer0 refused call %s", error_name(r));
                mud_sim_note(sim, S(note));
            }
        }
        for (int i = 0; i < n; i++) {
            if (!peers[i]) continue;
            int nn = 0;
            for (int k = 0; k < ns; k++) if (spam[k].peer == i) for (uint32_t m = 0; m < spam[k].rate * 100 / 1000; m++) {
                int32_t r = mud_publish(peers[i], lens[i], ++spam[k].clock, S("nav"), S("spam"), 5);
                if (r != MUD_OK) { room(nn); sprintf(notes[nn], "peer%d refused publish %s", i, error_name(r)); kinded[nn++] = 0; }
            }
            mud_out o;
            while (mud_outgoing(peers[i], lens[i], now, (mud_mutbuf){ scratch, sizeof scratch }, &o) > MUD_NONE) {
                int32_t r = o.kind == MUD_SEND ? mud_sim_send(sim, i, o.topic, o.wire, o.ttl_secs) : mud_sim_listen(sim, i, o.topic);
                if (r != MUD_OK) break;
                mud_sent(peers[i], lens[i]);
            }
            static mud_incoming calls[256]; static uint8_t call_bytes[256][MUD_MSG]; static char call_strs[256][3][MUD_MAX_TOPIC + 1]; int nc = 0;   /* a round's calls: at most RX_PER_SEC */
            mud_buf topic, wire;
            int kind;
            while ((kind = mud_sim_poll(sim, i, (mud_mutbuf){ rx, sizeof rx }, &topic, &wire)) > 0) {
                if (kind != 1) continue;
                mud_incoming in;
                int k = mud_deliver(peers[i], lens[i], now, topic, wire, (mud_mutbuf){ plain, sizeof plain }, &in);
                if (k == MUD_EVENT) landed[i]++;
                room(nn);
                if (k == MUD_CALL) {
                    sprintf(notes[nn], "peer%d call %s %s", i, q(in.method.ptr, in.method.len, qa), q(in.bytes.ptr, in.bytes.len, qb)); kinded[nn++] = 1;
                    if (nc < 256) {
                        memcpy(call_bytes[nc], in.bytes.ptr, in.bytes.len);
                        memcpy(call_strs[nc][0], in.topic.ptr, in.topic.len); memcpy(call_strs[nc][1], in.method.ptr, in.method.len); memcpy(call_strs[nc][2], in.reply_to.ptr, in.reply_to.len);
                        calls[nc] = in; calls[nc].bytes.ptr = call_bytes[nc]; calls[nc].topic.ptr = (uint8_t*)call_strs[nc][0]; calls[nc].method.ptr = (uint8_t*)call_strs[nc][1]; calls[nc].reply_to.ptr = (uint8_t*)call_strs[nc][2];
                        nc++;
                    }
                } else if (k == MUD_REPLY) { sprintf(notes[nn], "peer%d reply seq=%u end=%s status=%u %s", i, in.seq, in.end ? "true" : "false", in.status, q(in.bytes.ptr, in.bytes.len, qa)); kinded[nn++] = 1; }
                else if (k == MUD_EVENT) { sprintf(notes[nn], "peer%d event %s %s from sender %u", i, q(in.topic.ptr, in.topic.len, qa), q(in.bytes.ptr, in.bytes.len, qb), in.sender); kinded[nn++] = 1; }
                else if (k == MUD_FLOOD) { sprintf(notes[nn], "peer%d flood from sender %u", i, in.sender); kinded[nn++] = 0; }
                else if (k == MUD_ROLLBACK) { sprintf(notes[nn], "peer%d rollback from sender %u", i, in.sender); kinded[nn++] = 0; }
                else if (k == MUD_DROPPED) { char d[64]; sprintf(notes[nn], "peer%d dropped %s", i, dropped_name(&in, d)); kinded[nn++] = 0; }
            }
            mud_incoming ex;
            while (mud_tick(peers[i], lens[i], now, &ex) == MUD_EXPIRED) { room(nn); sprintf(notes[nn], "peer%d expired", i); kinded[nn++] = 0; }
            int opened = mud_opened(peers[i], lens[i]);
            const char* via = opened == 1 ? " via Previous" : opened == 2 ? " via Next" : "";
            for (int c = 0; c < nc; c++) { int32_t r = mud_reply(peers[i], lens[i], now, &calls[c], 0, 0, S(""), calls[c].bytes, 1); if (r != MUD_OK) { room(nn); sprintf(notes[nn], "peer%d refused reply %s", i, error_name(r)); kinded[nn++] = 0; } }
            for (int a = 0; a < nn;) {
                int run = 1; while (a + run < nn && strcmp(notes[a], notes[a + run]) == 0) run++;
                if (run > 1) sprintf(note, "%s ×%d", notes[a], run); else if (kinded[a]) sprintf(note, "%s%s", notes[a], via); else strcpy(note, notes[a]);
                mud_sim_note(sim, S(note));
                a += run;
            }
        }
        mud_sim_step(sim, 100);
        t += 100; round++;
    }
    for (int i = 0; i < n; i++) if (peers[i]) { sprintf(note, "peer%d pressure %u", i, mud_pressure(peers[i], lens[i])); mud_sim_note(sim, S(note)); }
    size_t need = (size_t)mud_sim_log(sim, (mud_mutbuf){ NULL, 0 });
    uint8_t* log = malloc(need + 1);
    mud_sim_log(sim, (mud_mutbuf){ log, need });
    fwrite(log, 1, need, stdout);
    mud_sim_free(sim);
    return 0;
}
