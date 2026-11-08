//! C stubs: a struct per message, `X_encode`/`X_decode` over inline varint helpers, no allocation.
//!
//! Repeated fields hold at most `MUD_REPEATED` items; strings and bytes are views into the decoded buffer; nested messages
//! are inline with a `has_` flag, so recursive message types are refused. Services become functions over the C ABI in mudrpc.h.
use crate::parse::{Message, Schema, Ty};
use std::fmt::Write;

fn ident(name: &str) -> String { name.replace('.', "_") }
/// A C string literal of printable ASCII, as the parser admits: `"`, `\` and `?` escaped, so no trigraph can form.
fn lit(s: &str) -> String { format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('?', "\\?")) }

const HELPERS: &str = r#"
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include "mudrpc.h"
#ifndef MUD_REPEATED
#define MUD_REPEATED 16
#endif
#ifndef MUD_GEN_HELPERS
#define MUD_GEN_HELPERS
#define MUD_ERR ((size_t)-1)
typedef struct { const uint8_t* ptr; size_t len; } mud_view;

static inline size_t mud__uv(uint64_t v, uint8_t* o, size_t cap, size_t p) {
    while (v > 0x7F) { if (p >= cap) return MUD_ERR; o[p++] = (uint8_t)(v | 0x80); v >>= 7; }
    if (p >= cap) return MUD_ERR;
    o[p++] = (uint8_t)v;
    return p;
}
static inline size_t mud__tag(uint32_t n, uint32_t wt, uint8_t* o, size_t cap, size_t p) { return mud__uv(((uint64_t)n << 3) | wt, o, cap, p); }
static inline size_t mud__ld(uint32_t n, const uint8_t* b, size_t len, uint8_t* o, size_t cap, size_t p) {
    p = mud__tag(n, 2, o, cap, p); if (p == MUD_ERR) return p;
    p = mud__uv(len, o, cap, p); if (p == MUD_ERR || p + len > cap) return MUD_ERR;
    memcpy(o + p, b, len);
    return p + len;
}
static inline size_t mud__fix(uint32_t n, uint32_t wt, uint64_t v, size_t size, uint8_t* o, size_t cap, size_t p) {
    p = mud__tag(n, wt, o, cap, p); if (p == MUD_ERR || p + size > cap) return MUD_ERR;
    for (size_t i = 0; i < size; i++) o[p + i] = (uint8_t)(v >> (8 * i));
    return p + size;
}
typedef size_t (*mud__writer)(const void*, uint8_t*, size_t, size_t);
static inline size_t mud__msg(uint32_t n, mud__writer w, const void* m, uint8_t* o, size_t cap, size_t p) {
    p = mud__tag(n, 2, o, cap, p); if (p == MUD_ERR || p + 1 > cap) return MUD_ERR;
    size_t at = p++, end = w(m, o, cap, p); if (end == MUD_ERR) return end;
    uint64_t len = end - at - 1; size_t extra = 0;
    for (uint64_t t = len; t > 0x7F; t >>= 7) extra++;
    if (end + extra > cap) return MUD_ERR;
    memmove(o + at + 1 + extra, o + at + 1, (size_t)len);
    mud__uv(len, o, cap, at);
    return end + extra;
}
static inline int mud__rv(const uint8_t* b, size_t len, size_t* i, uint64_t* v) {
    *v = 0;
    for (int s = 0; s < 64; s += 7) { if (*i >= len) return -1; uint8_t c = b[(*i)++]; *v |= (uint64_t)(c & 0x7F) << s; if (c < 0x80) return 0; }
    return -1;
}
/* one field: returns 1 and fills num/wt/v (and view for wt 2), 0 at end, -1 if malformed */
static inline int mud__next(const uint8_t* b, size_t len, size_t* i, uint32_t* num, uint32_t* wt, uint64_t* v, mud_view* view) {
    if (*i >= len) return 0;
    uint64_t tag; if (mud__rv(b, len, i, &tag)) return -1;
    *num = (uint32_t)(tag >> 3); *wt = (uint32_t)(tag & 7);
    if (*num == 0) return -1;
    if (*wt == 0) return mud__rv(b, len, i, v) ? -1 : 1;
    if (*wt == 1 || *wt == 5) { size_t n = *wt == 1 ? 8 : 4; if (*i + n > len) return -1; *v = 0; for (size_t k = 0; k < n; k++) *v |= (uint64_t)b[*i + k] << (8 * k); *i += n; return 1; }
    if (*wt == 2) { uint64_t n; if (mud__rv(b, len, i, &n) || n > len - *i) return -1; view->ptr = b + *i; view->len = (size_t)n; *i += (size_t)n; return 1; }
    return -1;
}
static inline int32_t mud__unzz32(uint64_t v) { return (int32_t)(((uint32_t)v >> 1) ^ (uint32_t)-(int32_t)((uint32_t)v & 1)); }
static inline int64_t mud__unzz64(uint64_t v) { return (int64_t)((v >> 1) ^ (uint64_t)-(int64_t)(v & 1)); }
static inline float mud__f32(uint64_t v) { uint32_t u = (uint32_t)v; float f; memcpy(&f, &u, 4); return f; }
static inline double mud__f64(uint64_t v) { double d; memcpy(&d, &v, 8); return d; }
static inline uint64_t mud__bits32(float f) { uint32_t u; memcpy(&u, &f, 4); return u; }
static inline uint64_t mud__bits64(double d) { uint64_t u; memcpy(&u, &d, 8); return u; }
#endif
"#;

fn c_ty(t: &Ty) -> String {
    match t {
        Ty::Bool => "int".into(), Ty::Int32 | Ty::Sint32 | Ty::Sfixed32 | Ty::Enum(_) => "int32_t".into(), Ty::Int64 | Ty::Sint64 | Ty::Sfixed64 => "int64_t".into(),
        Ty::Uint32 | Ty::Fixed32 => "uint32_t".into(), Ty::Uint64 | Ty::Fixed64 => "uint64_t".into(), Ty::Float => "float".into(), Ty::Double => "double".into(),
        Ty::String | Ty::Bytes => "mud_view".into(), Ty::Message(m) => ident(m),
    }
}

/// C statement writing one value `v` as field `n`, advancing `p`; `p = MUD_ERR` on overflow.
fn write_one(t: &Ty, n: u32, v: &str) -> String {
    let fix = |wt: u8, expr: String, size: u8| format!("p = mud__fix({n}, {wt}, {expr}, {size}, o, cap, p);");
    match t {
        Ty::Bool => format!("p = mud__tag({n}, 0, o, cap, p); if (p != MUD_ERR) p = mud__uv({v} ? 1 : 0, o, cap, p);"),
        Ty::Int32 | Ty::Enum(_) => format!("p = mud__tag({n}, 0, o, cap, p); if (p != MUD_ERR) p = mud__uv((uint64_t)(int64_t){v}, o, cap, p);"),
        Ty::Int64 => format!("p = mud__tag({n}, 0, o, cap, p); if (p != MUD_ERR) p = mud__uv((uint64_t){v}, o, cap, p);"),
        Ty::Uint32 | Ty::Uint64 => format!("p = mud__tag({n}, 0, o, cap, p); if (p != MUD_ERR) p = mud__uv({v}, o, cap, p);"),
        Ty::Sint32 => format!("p = mud__tag({n}, 0, o, cap, p); if (p != MUD_ERR) p = mud__uv((uint32_t)(({v} << 1) ^ ({v} >> 31)), o, cap, p);"),
        Ty::Sint64 => format!("p = mud__tag({n}, 0, o, cap, p); if (p != MUD_ERR) p = mud__uv((uint64_t)(({v} << 1) ^ ({v} >> 63)), o, cap, p);"),
        Ty::Fixed32 => fix(5, v.into(), 4), Ty::Sfixed32 => fix(5, format!("(uint32_t){v}"), 4), Ty::Float => fix(5, format!("mud__bits32({v})"), 4),
        Ty::Fixed64 => fix(1, v.into(), 8), Ty::Sfixed64 => fix(1, format!("(uint64_t){v}"), 8), Ty::Double => fix(1, format!("mud__bits64({v})"), 8),
        Ty::String | Ty::Bytes => format!("p = mud__ld({n}, {v}.ptr, {v}.len, o, cap, p);"),
        Ty::Message(m) => format!("p = mud__msg({n}, (mud__writer){}__write, &{v}, o, cap, p);", ident(m)),
    }
}

/// (wire type, C expression converting `v` or `view`)
fn read_one(t: &Ty) -> (u8, String) {
    match t {
        Ty::Bool => (0, "(v != 0)".into()), Ty::Int32 | Ty::Enum(_) => (0, "(int32_t)(uint32_t)v".into()), Ty::Int64 => (0, "(int64_t)v".into()),
        Ty::Uint32 => (0, "(uint32_t)v".into()), Ty::Uint64 => (0, "v".into()), Ty::Sint32 => (0, "mud__unzz32(v)".into()), Ty::Sint64 => (0, "mud__unzz64(v)".into()),
        Ty::Fixed32 => (5, "(uint32_t)v".into()), Ty::Sfixed32 => (5, "(int32_t)(uint32_t)v".into()), Ty::Float => (5, "mud__f32(v)".into()),
        Ty::Fixed64 => (1, "v".into()), Ty::Sfixed64 => (1, "(int64_t)v".into()), Ty::Double => (1, "mud__f64(v)".into()),
        Ty::String | Ty::Bytes => (2, "view".into()), Ty::Message(_) => (2, String::new()),
    }
}

fn is_default(t: &Ty, v: &str) -> String {
    match t { Ty::String | Ty::Bytes => format!("{v}.len == 0"), _ => format!("{v} == 0") }
}

/// Messages ordered so every inline child is declared first; a cycle cannot be inlined.
fn ordered(s: &Schema) -> Result<Vec<&Message>, String> {
    let (mut out, mut done): (Vec<&Message>, Vec<&str>) = (vec![], vec![]);
    let mut progress = true;
    while out.len() < s.messages.len() && progress {
        progress = false;
        for m in &s.messages {
            if done.contains(&m.name.as_str()) { continue; }
            let ready = m.fields.iter().all(|f| match &f.ty { Ty::Message(d) => done.contains(&d.as_str()) || d == &m.name, _ => true });
            if m.fields.iter().any(|f| matches!(&f.ty, Ty::Message(d) if d == &m.name)) { return Err(format!("message {} refers to itself; recursive types cannot be inline in C", m.name)); }
            if ready { out.push(m); done.push(&m.name); progress = true; }
        }
    }
    if out.len() < s.messages.len() { return Err("recursive message types cannot be inline in C".into()); }
    Ok(out)
}

/// The C header for a schema, over mudrpc.h; a recursive message is refused, since C cannot inline it.
pub fn emit(s: &Schema) -> Result<String, String> {
    let mut o = String::new();
    let _ = writeln!(o, "{HELPERS}");
    for e in &s.enums {
        let _ = writeln!(o, "enum {{");
        for (v, n) in &e.values { let _ = writeln!(o, "    {}_{v} = {n},", ident(&e.name)); }
        let _ = writeln!(o, "}};");
    }
    for m in ordered(s)? {
        let name = ident(&m.name);
        let _ = writeln!(o, "\ntypedef struct {name} {{");
        if m.fields.is_empty() { let _ = writeln!(o, "    int mud__empty;"); }
        for f in &m.fields {
            let t = c_ty(&f.ty);
            if f.repeated { let _ = writeln!(o, "    {t} {}[MUD_REPEATED]; size_t {}_len;", f.name, f.name); }
            else if matches!(f.ty, Ty::Message(_)) { let _ = writeln!(o, "    int has_{}; {t} {};", f.name, f.name); }
            else { let _ = writeln!(o, "    {t} {};", f.name); }
        }
        let _ = writeln!(o, "}} {name};\n\nstatic inline size_t {name}__write(const {name}* m, uint8_t* o, size_t cap, size_t p) {{");
        for f in &m.fields {
            if f.repeated { let _ = writeln!(o, "    for (size_t i = 0; i < m->{0}_len && p != MUD_ERR; i++) {{ {1} }}", f.name, write_one(&f.ty, f.number, &format!("m->{}[i]", f.name))); }
            else if matches!(f.ty, Ty::Message(_)) { let _ = writeln!(o, "    if (m->has_{0} && p != MUD_ERR) {{ {1} }}", f.name, write_one(&f.ty, f.number, &format!("m->{}", f.name))); }
            else { let _ = writeln!(o, "    if (!({0}) && p != MUD_ERR) {{ {1} }}", is_default(&f.ty, &format!("m->{}", f.name)), write_one(&f.ty, f.number, &format!("m->{}", f.name))); }
        }
        let _ = writeln!(o, "    (void)m; (void)o; (void)cap;\n    return p;\n}}\n\n/* bytes written, or MUD_ERR if `cap` is too small */\nstatic inline size_t {name}_encode(const {name}* m, uint8_t* o, size_t cap) {{ return {name}__write(m, o, cap, 0); }}\n");
        let _ = writeln!(o, "/* 0 ok, -1 malformed or more than MUD_REPEATED items. Views point into b. */\nstatic inline int {name}_decode({name}* m, const uint8_t* b, size_t len) {{\n    memset(m, 0, sizeof *m);\n    size_t i = 0; uint32_t num, wt; uint64_t v; mud_view view = {{ 0, 0 }}; int r;\n    while ((r = mud__next(b, len, &i, &num, &wt, &v, &view)) == 1) {{");
        for f in &m.fields {
            let (wt, conv) = read_one(&f.ty);
            let push = |expr: &str| format!("if (m->{0}_len >= MUD_REPEATED) return -1; m->{0}[m->{0}_len++] = {expr};", f.name);
            match (&f.ty, f.repeated) {
                (Ty::Message(d), true) => { let _ = writeln!(o, "        if (num == {} && wt == 2) {{ if (m->{1}_len >= MUD_REPEATED || {2}_decode(&m->{1}[m->{1}_len], view.ptr, view.len)) return -1; m->{1}_len++; continue; }}", f.number, f.name, ident(d)); }
                (Ty::Message(d), false) => { let _ = writeln!(o, "        if (num == {} && wt == 2) {{ if ({2}_decode(&m->{1}, view.ptr, view.len)) return -1; m->has_{1} = 1; continue; }}", f.number, f.name, ident(d)); }
                (t, true) => {
                    let _ = writeln!(o, "        if (num == {} && wt == {wt}) {{ {} continue; }}", f.number, push(&conv));
                    match wt {
                        0 => { let _ = writeln!(o, "        if (num == {} && wt == 2) {{ size_t j = 0; while (j < view.len) {{ if (mud__rv(view.ptr, view.len, &j, &v)) return -1; {} }} continue; }}", f.number, push(&conv)); }
                        5 | 1 => { let size = if wt == 5 { 4 } else { 8 }; let _ = writeln!(o, "        if (num == {} && wt == 2) {{ for (size_t j = 0; j + {size} <= view.len; j += {size}) {{ v = 0; for (size_t k = 0; k < {size}; k++) v |= (uint64_t)view.ptr[j + k] << (8 * k); {} }} continue; }}", f.number, push(&conv)); }
                        _ => { let _ = t; }
                    }
                }
                (_, false) => { let _ = writeln!(o, "        if (num == {} && wt == {wt}) {{ m->{} = {conv}; continue; }}", f.number, f.name); }
            }
        }
        let _ = writeln!(o, "    }}\n    (void)view; (void)v;\n    return r;\n}}");
    }
    for svc in &s.services {
        let topic = if s.package.is_empty() { svc.name.clone() } else { format!("{}.{}", s.package, svc.name) };
        let _ = writeln!(o, "\n#define {}_TOPIC \"{topic}\"", svc.name);
        for m in &svc.methods {
            let (req, resp) = (ident(&m.input), ident(&m.output));
            let ttl_param = if m.ttl_secs.is_some() { "" } else { ", uint32_t ttl_secs" };
            let ttl = m.ttl_secs.map_or_else(|| "ttl_secs".to_string(), |t| t.to_string());
            if let Some(t) = &m.topic {
                let _ = writeln!(o, "/* publishes; no reply */\nstatic inline int32_t {0}_{1}(void* peer, size_t len, uint32_t now, const {req}* req{ttl_param}) {{\n    uint8_t buf[MUD_MSG]; size_t n = {req}_encode(req, buf, sizeof buf); if (n == MUD_ERR) return MUD_ETOOLARGE;\n    mud_buf topic = {{ (const uint8_t*){3}, {2} }}, payload = {{ buf, n }};\n    return mud_publish(peer, len, now, topic, payload, {ttl});\n}}", svc.name, m.name, t.len(), lit(t));
            } else {
                let _ = writeln!(o, "static inline int32_t {0}_{1}(void* peer, size_t len, uint32_t now, const {req}* req{ttl_param}, mud_id* id) {{\n    uint8_t buf[MUD_MSG]; size_t n = {req}_encode(req, buf, sizeof buf); if (n == MUD_ERR) return MUD_ETOOLARGE;\n    mud_buf service = {{ (const uint8_t*){0}_TOPIC, {2} }}, method = {{ (const uint8_t*)\"{1}\", {3} }}, payload = {{ buf, n }};\n    return mud_call(peer, len, now, service, method, payload, {ttl}, id);\n}}\nstatic inline int32_t {0}_{1}_reply(void* peer, size_t len, uint32_t now, const mud_incoming* call, uint32_t seq, uint32_t status, mud_buf detail, const {resp}* resp, int32_t end) {{\n    uint8_t buf[MUD_MSG]; size_t n = {resp}_encode(resp, buf, sizeof buf); if (n == MUD_ERR) return MUD_ETOOLARGE;\n    mud_buf payload = {{ buf, n }};\n    return mud_reply(peer, len, now, call, seq, status, detail, payload, end);\n}}", svc.name, m.name, topic.len(), m.name.len());
            }
        }
        let _ = writeln!(o, "/* which method a MUD_CALL on this service names, in declaration order, or -1 */\nstatic inline int {0}_method(const mud_incoming* in) {{\n    if (in->topic.len != {1} || memcmp(in->topic.ptr, {0}_TOPIC, {1}) != 0) return -1;", svc.name, topic.len());
        for (i, m) in svc.methods.iter().enumerate() { let _ = writeln!(o, "    if (in->method.len == {} && memcmp(in->method.ptr, \"{}\", {}) == 0) return {i};", m.name.len(), m.name, m.name.len()); }
        let _ = writeln!(o, "    return -1;\n}}");
    }
    let guard = o.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));   // FNV-1a over the body: one guard per proto, not per package
    Ok(format!("/* Generated by mud gen. Do not edit. oneof members are plain fields: set at most one. */\n#ifndef MUD_GEN_{guard:016X}\n#define MUD_GEN_{guard:016X}{o}#endif\n"))
}
