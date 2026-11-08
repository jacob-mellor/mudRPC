/* mudRPC for Android: the JNI glue between mudrpc/Mud.java beside this file and mudrpc.h, one file. The NDK builds it into
   libmudrpc.so with libmudrpc.a for aarch64-linux-android; a desktop compiler builds it against the shared library for the
   tests. Every byte[] argument is pinned for the call and released after it; every result is one byte[] the Java side reads
   in native byte order: 32-bit ints, 64-bit longs, and each buffer as a 4-byte length then its bytes. Nothing here keeps a
   pointer past the call. */
#include <jni.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "mudrpc.h"

typedef struct { JNIEnv* env; jbyteArray arr[8]; jbyte* p[8]; int n; } args;

static mud_buf take(args* a, jbyteArray arr) {
    mud_buf b = { NULL, 1 };   // null, or an array the JVM cannot pin: a buffer the library refuses as MUD_EINVAL
    if (arr == NULL) return b;
    int i = a->n;
    a->p[i] = (*a->env)->GetByteArrayElements(a->env, arr, NULL);
    if (a->p[i] == NULL) return b;
    a->arr[i] = arr;
    a->n++;
    b.ptr = (const uint8_t*) a->p[i];
    b.len = (size_t) (*a->env)->GetArrayLength(a->env, arr);
    return b;
}
static void release(args* a) { for (int i = 0; i < a->n; i++) (*a->env)->ReleaseByteArrayElements(a->env, a->arr[i], a->p[i], JNI_ABORT); }

typedef struct { uint8_t* p; size_t n, cap; } pack;
static void put(pack* k, const void* d, size_t n) {
    if (k->n + n > k->cap) { k->cap = (k->cap + n) * 2; k->p = realloc(k->p, k->cap); }
    memcpy(k->p + k->n, d, n);
    k->n += n;
}
static void put_i32(pack* k, int32_t v) { put(k, &v, 4); }
static void put_i64(pack* k, int64_t v) { put(k, &v, 8); }
static void put_buf(pack* k, mud_buf b) { put_i32(k, (int32_t) b.len); if (b.len) put(k, b.ptr, b.len); }
static jbyteArray done(JNIEnv* env, pack* k) {
    jbyteArray a = (*env)->NewByteArray(env, (jsize) k->n);
    if (a != NULL) (*env)->SetByteArrayRegion(env, a, 0, (jsize) k->n, (const jbyte*) k->p);   // null: the JVM's OutOfMemoryError is pending
    free(k->p);
    return a;
}
static void put_incoming(pack* k, int32_t code, const mud_incoming* in) {
    put_i32(k, code);
    if (code <= 0) return;
    put_i32(k, in->kind); put_i32(k, (int32_t) in->ttl_secs); put_i32(k, (int32_t) in->seq); put_i32(k, (int32_t) in->status); put_i32(k, in->end); put_i32(k, in->dropped); put_i32(k, (int32_t) in->sender);
    put(k, in->id.bytes, 12);
    put_buf(k, in->topic); put_buf(k, in->method); put_buf(k, in->reply_to); put_buf(k, in->detail); put_buf(k, in->bytes);
}
static void set_code(JNIEnv* env, jintArray out, int32_t code) { jint c = code; (*env)->SetIntArrayRegion(env, out, 0, 1, &c); }

#define MEM (void*) (intptr_t) mem
#define UDP (mud_udp*) (intptr_t) udp
#define SIM (mud_sim*) (intptr_t) sim

static jlong size(JNIEnv* e, jclass c) { return (jlong) mud_size(); }
static jlong hubSize(JNIEnv* e, jclass c) { return (jlong) mud_hub_size(); }
static jlong alloc(JNIEnv* e, jclass c, jlong n) { void* p = NULL; if (posix_memalign(&p, mud_align(), (size_t) n)) return 0; memset(p, 0, (size_t) n); return (jlong) (intptr_t) p; }
static void free_(JNIEnv* e, jclass c, jlong mem) { free(MEM); }
static jint newPeer(JNIEnv* e, jclass c, jlong mem, jlong len, jbyteArray name, jbyteArray key, jint sender, jint epoch, jbyteArray seed, jint budget, jint hub, jint cipher) {
    args a = { .env = e };
    mud_config cfg = { take(&a, name), take(&a, key), (uint32_t) sender, (uint32_t) epoch, take(&a, seed), (uint32_t) budget, (uint32_t) hub, (uint32_t) cipher };
    int32_t r = mud_new(MEM, (size_t) len, &cfg);
    release(&a);
    return r;
}
static jbyteArray save(JNIEnv* e, jclass c, jlong mem, jlong len) {
    void* out = NULL;
    pack k = { 0 };
    int32_t r = posix_memalign(&out, mud_align(), (size_t) len) ? MUD_EMEM : (memset(out, 0, (size_t) len), mud_save(MEM, (size_t) len, out, (size_t) len));   // zeroed first: a leaf in a hub-sized block writes less than len
    put_i32(&k, r);
    if (r == 0) put(&k, out, (size_t) len);
    free(out);
    return done(e, &k);
}
static jint resume(JNIEnv* e, jclass c, jlong mem, jlong len, jbyteArray saved, jint epoch) {
    args a = { .env = e };
    mud_buf s = take(&a, saved);
    if (s.ptr) memcpy(MEM, s.ptr, s.len);
    release(&a);
    return mud_resume(MEM, (size_t) len, (uint32_t) epoch);
}
static jint renew(JNIEnv* e, jclass c, jlong mem, jlong len, jint epoch) { return mud_renew(MEM, (size_t) len, (uint32_t) epoch); }
static jint drop(JNIEnv* e, jclass c, jlong mem, jlong len) { return mud_drop(MEM, (size_t) len); }
static jint stage(JNIEnv* e, jclass c, jlong mem, jlong len, jbyteArray key) { args a = { .env = e }; int32_t r = mud_stage(MEM, (size_t) len, take(&a, key)); release(&a); return r; }
static jint rotate(JNIEnv* e, jclass c, jlong mem, jlong len) { return mud_rotate(MEM, (size_t) len); }
static jint retire(JNIEnv* e, jclass c, jlong mem, jlong len) { return mud_retire(MEM, (size_t) len); }
static jint roster(JNIEnv* e, jclass c, jlong mem, jlong len, jint sender, jbyteArray key, jbyteArray next) { args a = { .env = e }; int32_t r = mud_roster(MEM, (size_t) len, (uint32_t) sender, take(&a, key), take(&a, next)); release(&a); return r; }
static jint revoke(JNIEnv* e, jclass c, jlong mem, jlong len, jint sender) { return mud_revoke(MEM, (size_t) len, (uint32_t) sender); }
static jint opened(JNIEnv* e, jclass c, jlong mem, jlong len) { return mud_opened(MEM, (size_t) len); }
static jbyteArray publicKey(JNIEnv* e, jclass c, jbyteArray seed) {
    args a = { .env = e };
    uint8_t out[32];
    pack k = { 0 };
    int32_t r = mud_public_key(take(&a, seed), out);
    release(&a);
    put_i32(&k, r);
    if (r == 0) put(&k, out, 32);
    return done(e, &k);
}
static jbyteArray call(JNIEnv* e, jclass c, jlong mem, jlong len, jint now, jbyteArray service, jbyteArray method, jbyteArray req, jint ttl) {
    args a = { .env = e };
    mud_id id;
    pack k = { 0 };
    int32_t r = mud_call(MEM, (size_t) len, (uint32_t) now, take(&a, service), take(&a, method), take(&a, req), (uint32_t) ttl, &id);
    release(&a);
    put_i32(&k, r);
    if (r == 0) put(&k, id.bytes, 12);
    return done(e, &k);
}
static jint publish(JNIEnv* e, jclass c, jlong mem, jlong len, jint now, jbyteArray topic, jbyteArray payload, jint ttl) { args a = { .env = e }; int32_t r = mud_publish(MEM, (size_t) len, (uint32_t) now, take(&a, topic), take(&a, payload), (uint32_t) ttl); release(&a); return r; }
static jint listen(JNIEnv* e, jclass c, jlong mem, jlong len, jbyteArray topic) { args a = { .env = e }; int32_t r = mud_listen(MEM, (size_t) len, take(&a, topic)); release(&a); return r; }
static jint reply(JNIEnv* e, jclass c, jlong mem, jlong len, jint now, jbyteArray id, jbyteArray topic, jbyteArray method, jbyteArray replyTo, jbyteArray bytes, jint ttl, jint seqIn, jint endIn, jint seq, jint status, jbyteArray detail, jbyteArray payload, jint end) {
    args a = { .env = e };
    mud_incoming in = { .kind = MUD_CALL };
    mud_buf i = take(&a, id);
    if (i.len == 12) memcpy(in.id.bytes, i.ptr, 12);
    in.topic = take(&a, topic); in.method = take(&a, method); in.reply_to = take(&a, replyTo); in.bytes = take(&a, bytes);
    in.ttl_secs = (uint32_t) ttl; in.seq = (uint32_t) seqIn; in.end = endIn;
    int32_t r = i.len == 12 ? mud_reply(MEM, (size_t) len, (uint32_t) now, &in, (uint32_t) seq, (uint32_t) status, take(&a, detail), take(&a, payload), end) : MUD_EINVAL;
    release(&a);
    return r;
}
static jbyteArray deliver(JNIEnv* e, jclass c, jlong mem, jlong len, jint now, jbyteArray topic, jbyteArray wire) {
    args a = { .env = e };
    uint8_t plain[MUD_MSG];
    mud_mutbuf scratch = { plain, MUD_MSG };
    mud_incoming in;
    pack k = { 0 };
    int32_t r = mud_deliver(MEM, (size_t) len, (uint32_t) now, take(&a, topic), take(&a, wire), scratch, &in);
    put_incoming(&k, r, &in);   // the verdict's topic points into the pinned argument: packed before it is released
    release(&a);
    return done(e, &k);
}
static jbyteArray tick(JNIEnv* e, jclass c, jlong mem, jlong len, jint now) { mud_incoming in; pack k = { 0 }; put_incoming(&k, mud_tick(MEM, (size_t) len, (uint32_t) now, &in), &in); return done(e, &k); }
static jbyteArray outgoing(JNIEnv* e, jclass c, jlong mem, jlong len, jint now) {
    uint8_t buf[MUD_MAX_TOPIC + MUD_MSG];
    mud_mutbuf scratch = { buf, sizeof buf };
    mud_out o;
    pack k = { 0 };
    int32_t r = mud_outgoing(MEM, (size_t) len, (uint32_t) now, scratch, &o);
    put_i32(&k, r);
    if (r > 0) { put_i32(&k, (int32_t) o.ttl_secs); put_buf(&k, o.topic); put_buf(&k, o.wire); }
    return done(e, &k);
}
static jint sent(JNIEnv* e, jclass c, jlong mem, jlong len) { return mud_sent(MEM, (size_t) len); }
static jint pressure(JNIEnv* e, jclass c, jlong mem, jlong len) { return (jint) mud_pressure(MEM, (size_t) len); }
static jint pending(JNIEnv* e, jclass c, jlong mem, jlong len) { return (jint) mud_pending(MEM, (size_t) len); }
static jint queued(JNIEnv* e, jclass c, jlong mem, jlong len) { return (jint) mud_queued(MEM, (size_t) len); }
static jint count(JNIEnv* e, jclass c, jlong mem, jlong len, jint which) { return (jint) mud_count(MEM, (size_t) len, which); }

static jint internet(JNIEnv* e, jclass c) { return mud_internet(); }
static jlong udpNew(JNIEnv* e, jclass c, jbyteArray bind, jbyteArray peers, jintArray code) { args a = { .env = e }; int32_t r = 0; mud_udp* u = mud_udp_new(take(&a, bind), take(&a, peers), &r); release(&a); set_code(e, code, r); return (jlong) (intptr_t) u; }
static void udpFree(JNIEnv* e, jclass c, jlong udp) { mud_udp_free(UDP); }
static jint udpPort(JNIEnv* e, jclass c, jlong udp) { return mud_udp_port(UDP); }
static jint udpSend(JNIEnv* e, jclass c, jlong udp, jbyteArray topic, jbyteArray wire) { args a = { .env = e }; int32_t r = mud_udp_send(UDP, take(&a, topic), take(&a, wire)); release(&a); return r; }
static jint udpListen(JNIEnv* e, jclass c, jlong udp, jbyteArray topic) { args a = { .env = e }; int32_t r = mud_udp_listen(UDP, take(&a, topic)); release(&a); return r; }
static jbyteArray received(JNIEnv* e, int32_t r, mud_buf topic, mud_buf wire) { pack k = { 0 }; put_i32(&k, r); if (r == 1) { put_buf(&k, topic); put_buf(&k, wire); } return done(e, &k); }
static jbyteArray udpPoll(JNIEnv* e, jclass c, jlong udp) { uint8_t buf[MUD_MAX_TOPIC + MUD_MSG]; mud_mutbuf s = { buf, sizeof buf }; mud_buf t = { 0 }, w = { 0 }; return received(e, mud_udp_poll(UDP, s, &t, &w), t, w); }

static jlong simNew(JNIEnv* e, jclass c, jbyteArray text, jintArray line) { args a = { .env = e }; int32_t l = 0; mud_sim* s = mud_sim_new(take(&a, text), &l); release(&a); set_code(e, line, l); return (jlong) (intptr_t) s; }
static void simFree(JNIEnv* e, jclass c, jlong sim) { mud_sim_free(SIM); }
static jint simPeers(JNIEnv* e, jclass c, jlong sim) { return mud_sim_peers(SIM); }
static jlong simLastEventMs(JNIEnv* e, jclass c, jlong sim) { return (jlong) mud_sim_last_event_ms(SIM); }
static jlong simNowMs(JNIEnv* e, jclass c, jlong sim) { return (jlong) mud_sim_now_ms(SIM); }
static void simStep(JNIEnv* e, jclass c, jlong sim, jlong dt) { mud_sim_step(SIM, (uint64_t) dt); }
static void simNote(JNIEnv* e, jclass c, jlong sim, jbyteArray text) { args a = { .env = e }; mud_sim_note(SIM, take(&a, text)); release(&a); }
static void simSilent(JNIEnv* e, jclass c, jlong sim, jint radio, jint on) { mud_sim_silent(SIM, radio, on); }
static jint simSend(JNIEnv* e, jclass c, jlong sim, jint radio, jbyteArray topic, jbyteArray wire, jint ttl) { args a = { .env = e }; int32_t r = mud_sim_send(SIM, radio, take(&a, topic), take(&a, wire), (uint32_t) ttl); release(&a); return r; }
static jint simListen(JNIEnv* e, jclass c, jlong sim, jint radio, jbyteArray topic) { args a = { .env = e }; int32_t r = mud_sim_listen(SIM, radio, take(&a, topic)); release(&a); return r; }
static jbyteArray simPoll(JNIEnv* e, jclass c, jlong sim, jint radio) { uint8_t buf[MUD_MAX_TOPIC + MUD_MSG]; mud_mutbuf s = { buf, sizeof buf }; mud_buf t = { 0 }, w = { 0 }; return received(e, mud_sim_poll(SIM, radio, s, &t, &w), t, w); }
static jbyteArray simLog(JNIEnv* e, jclass c, jlong sim) {
    mud_mutbuf none = { (uint8_t*) "", 0 };
    int32_t n = mud_sim_log(SIM, none);
    uint8_t* out = malloc((size_t) n + 1);
    mud_mutbuf all = { out, (size_t) n };
    pack k = { 0 };
    mud_sim_log(SIM, all);
    put(&k, out, (size_t) n);
    free(out);
    return done(e, &k);
}
static jint simEvents(JNIEnv* e, jclass c, jlong sim) { return mud_sim_events(SIM); }
static jbyteArray simEvent(JNIEnv* e, jclass c, jlong sim, jint i) {
    mud_event ev;
    pack k = { 0 };
    int32_t r = mud_sim_event(SIM, i, &ev);
    put_i32(&k, r);
    if (r == 0) { put_i64(&k, (int64_t) ev.at_ms); put_i32(&k, ev.kind); put_i64(&k, (int64_t) ev.a); put_i64(&k, (int64_t) ev.b); put_buf(&k, ev.text); }
    return done(e, &k);
}

static const JNINativeMethod METHODS[] = {
    { "size", "()J", (void*) size }, { "hubSize", "()J", (void*) hubSize }, { "alloc", "(J)J", (void*) alloc }, { "free", "(J)V", (void*) free_ },
    { "newPeer", "(JJ[B[BII[BIII)I", (void*) newPeer }, { "save", "(JJ)[B", (void*) save }, { "resume", "(JJ[BI)I", (void*) resume }, { "renew", "(JJI)I", (void*) renew }, { "drop", "(JJ)I", (void*) drop },
    { "stage", "(JJ[B)I", (void*) stage }, { "rotate", "(JJ)I", (void*) rotate }, { "retire", "(JJ)I", (void*) retire }, { "roster", "(JJI[B[B)I", (void*) roster }, { "revoke", "(JJI)I", (void*) revoke }, { "opened", "(JJ)I", (void*) opened }, { "publicKey", "([B)[B", (void*) publicKey },
    { "call", "(JJI[B[B[BI)[B", (void*) call }, { "publish", "(JJI[B[BI)I", (void*) publish }, { "listen", "(JJ[B)I", (void*) listen }, { "reply", "(JJI[B[B[B[B[BIIIII[B[BI)I", (void*) reply },
    { "deliver", "(JJI[B[B)[B", (void*) deliver }, { "tick", "(JJI)[B", (void*) tick }, { "outgoing", "(JJI)[B", (void*) outgoing }, { "sent", "(JJ)I", (void*) sent },
    { "pressure", "(JJ)I", (void*) pressure }, { "pending", "(JJ)I", (void*) pending }, { "queued", "(JJ)I", (void*) queued }, { "count", "(JJI)I", (void*) count },
    { "internet", "()I", (void*) internet }, { "udpNew", "([B[B[I)J", (void*) udpNew }, { "udpFree", "(J)V", (void*) udpFree }, { "udpPort", "(J)I", (void*) udpPort }, { "udpSend", "(J[B[B)I", (void*) udpSend }, { "udpListen", "(J[B)I", (void*) udpListen }, { "udpPoll", "(J)[B", (void*) udpPoll },
    { "simNew", "([B[I)J", (void*) simNew }, { "simFree", "(J)V", (void*) simFree }, { "simPeers", "(J)I", (void*) simPeers }, { "simLastEventMs", "(J)J", (void*) simLastEventMs }, { "simNowMs", "(J)J", (void*) simNowMs }, { "simStep", "(JJ)V", (void*) simStep }, { "simNote", "(J[B)V", (void*) simNote }, { "simSilent", "(JII)V", (void*) simSilent },
    { "simSend", "(JI[B[BI)I", (void*) simSend }, { "simListen", "(JI[B)I", (void*) simListen }, { "simPoll", "(JI)[B", (void*) simPoll }, { "simLog", "(J)[B", (void*) simLog }, { "simEvents", "(J)I", (void*) simEvents }, { "simEvent", "(JI)[B", (void*) simEvent },
};

JNIEXPORT jint JNI_OnLoad(JavaVM* vm, void* reserved) {
    JNIEnv* env;
    if ((*vm)->GetEnv(vm, (void**) &env, JNI_VERSION_1_6) != JNI_OK) return JNI_ERR;
    jclass c = (*env)->FindClass(env, "mudrpc/Mud$Native");
    if (c == NULL || (*env)->RegisterNatives(env, c, METHODS, sizeof METHODS / sizeof *METHODS) != 0) return JNI_ERR;
    return JNI_VERSION_1_6;
}
