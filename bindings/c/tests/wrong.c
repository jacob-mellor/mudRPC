/* The host that does everything wrong: each mistake in SKILL.md's Verify table, in its order, and what the library
   answers, one line each. abi.rs holds the lines this must print. usage: wrong */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "mudrpc.h"

static mud_buf S(const char* s) { mud_buf b = { (const uint8_t*)s, strlen(s) }; return b; }
static const char* name(int32_t r) {
    static const char* codes[] = { "MUD_OK", "MUD_EINVAL", "MUD_EFULL", "MUD_ETOOLARGE", "MUD_ECORRUPT", "MUD_EMEM", "MUD_EEPOCH", "MUD_EUNPROVISIONED", "MUD_EEXHAUSTED", "MUD_ERATE", "MUD_EBUSY" };
    return r <= 0 && r >= -10 ? codes[-r] : "?";
}
static void say(const char* mistake, int32_t r) { printf("%s: %s\n", mistake, name(r)); }

static uint8_t key[32], same[32], seed[32], scratch[MUD_MAX_TOPIC + MUD_MSG], plain[MUD_MSG], held[MUD_MSG];
static mud_config cfg(const char* n, uint32_t sender, const uint8_t* sd, uint32_t hub) {
    mud_config c = { S(n), { key, 32 }, sender, 5, { sd, sd ? 32 : 0 }, 10, hub, MUD_CIPHER_DEFAULT };
    return c;
}
/* hands each item to a transport that takes it, or up to the first wire when `wire` keeps it; the last wire's ttl */
static uint32_t drain(void* p, size_t len, uint32_t now, mud_buf* wire) {
    mud_out o; uint32_t ttl = 0;
    while (mud_outgoing(p, len, now, (mud_mutbuf){ scratch, sizeof scratch }, &o) > 0) {
        int send = o.kind == MUD_SEND;
        if (send) ttl = o.ttl_secs;
        if (send && wire) { memcpy(held, o.wire.ptr, o.wire.len); *wire = (mud_buf){ held, o.wire.len }; }
        mud_sent(p, len);
        if (send && wire) return ttl;
    }
    return ttl;
}

int main(void) {
    for (int i = 0; i < 32; i++) { key[i] = (uint8_t)(1 + i); same[i] = 7; seed[i] = (uint8_t)(7 + i); }   /* counting bytes stand in for the OS's random ones */
    size_t leaf = mud_size(), hub = mud_hub_size(), big = leaf + 8;
    void *a = malloc(big), *z = malloc(leaf), *h = malloc(hub), *copy = malloc(leaf), *srv = malloc(leaf), *cli = malloc(leaf);
    mud_config c = cfg("gcs", 1, NULL, 0);
    int32_t longer = mud_new(a, big, &c);
    if (mud_save(a, big, copy, leaf) != MUD_OK) return 1;

    say("mud_resume at the saved epoch", mud_resume(copy, leaf, 5));
    say("mud_renew below the current epoch", mud_renew(a, big, 4));
    c = cfg("gcs", 0, NULL, 0); mud_new(z, leaf, &c);
    say("a send from sender 0", mud_publish(z, leaf, 0, S("t"), S("x"), 10));
    c = cfg("gcs", 2, NULL, 0); c.key = (mud_buf){ same, 32 };
    say("a key of 32 equal bytes", mud_new(z, leaf, &c));
    say("a staged key of 32 equal bytes", mud_stage(a, big, (mud_buf){ same, 32 }));
    c = cfg("gcs", 2, same, 0);
    say("a seed of 32 equal bytes", mud_new(z, leaf, &c));
    c = cfg("gcs", 1, NULL, 0);
    say("a block one byte short of mud_size()", mud_new(z, leaf - 1, &c));
    c = cfg("gcs", 3, seed, 1);
    say("a hub at a leaf's length", mud_new(h, leaf, &c));
    printf("a block longer than its shape: silent, %s\n", name(longer));
    c = cfg("gcs", 3, NULL, 1);
    say("hub 1 without a seed", mud_new(h, hub, &c));
    say("a saved copy sent from before mud_resume", mud_publish(copy, leaf, 0, S("t"), S("x"), 10));
    ((uint32_t*)copy)[0] = 0;   /* the tag, the block's first word: an AES leaf's 3 written as a ChaCha leaf's 0 */
    say("a saved copy retagged to the other cipher, resumed", mud_resume(copy, leaf, 6));
    mud_publish(a, big, 0, S("t"), S("x"), 0);
    printf("ttl 0: clamped to %u\n", drain(a, big, 0, NULL));
    mud_publish(a, big, 0, S("t"), S("x"), UINT32_MAX);
    printf("ttl past a week: clamped to %u\n", drain(a, big, 0, NULL));
    char long_[MUD_MAX_TOPIC + 2]; memset(long_, 't', MUD_MAX_TOPIC + 1); long_[MUD_MAX_TOPIC + 1] = 0;
    say("a topic over MUD_MAX_TOPIC", mud_publish(a, big, 0, S(long_), S("x"), 10));
    c = cfg(long_, 2, NULL, 0);
    say("a name over MUD_MAX_TOPIC", mud_new(z, leaf, &c));
    static const uint8_t payload[500];
    say("a payload over the ceiling", mud_publish(a, big, 0, S("t"), (mud_buf){ payload, sizeof payload }, 10));
    c = cfg("", 2, NULL, 0);
    say("an empty name", mud_new(z, leaf, &c));
    mud_id id;
    say("an empty service", mud_call(a, big, 0, S(""), S("M"), S(""), 10, &id));
    say("an empty topic", mud_publish(a, big, 0, S(""), S("x"), 10));

    c = cfg("srv", 8, NULL, 0); mud_new(srv, leaf, &c);
    c = cfg("cli", 9, NULL, 0); mud_new(cli, leaf, &c);
    mud_buf w; mud_incoming in;
    mud_call(cli, leaf, 0, S("srv"), S("M"), S(""), 10, &id);
    drain(cli, leaf, 0, &w);
    if (mud_deliver(srv, leaf, 0, S("srv"), w, (mud_mutbuf){ plain, sizeof plain }, &in) != MUD_CALL) return 1;
    mud_reply(srv, leaf, 0, &in, 0, 0, S(""), S(""), 1);
    drain(srv, leaf, 0, &w);
    int32_t k = mud_deliver(a, big, 0, S("cli"), w, (mud_mutbuf){ plain, sizeof plain }, &in);
    printf("a reply to a call this peer does not have: silent, %s, dropped %d\n", k == MUD_DROPPED ? "MUD_DROPPED" : "?", in.dropped);

    for (int i = 0; i < 16; i++) { mud_call(a, big, 1, S("nav"), S("M"), S(""), 10, &id); drain(a, big, 1, NULL); }
    say("a 17th call pending", mud_call(a, big, 1, S("nav"), S("M"), S(""), 10, &id));
    for (int i = 0; i < 16; i++) mud_publish(a, big, 1, S("t"), S("x"), 10);
    say("a 17th item queued undrained", mud_publish(a, big, 1, S("t"), S("x"), 10));
    drain(a, big, 1, NULL);
    for (int i = 0; i < 32; i++) { mud_publish(a, big, 2, S("t"), S("x"), 10); drain(a, big, 2, NULL); }
    say("the 33rd send in one second", mud_publish(a, big, 2, S("t"), S("x"), 10));
    return 0;
}
