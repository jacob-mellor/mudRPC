//! Python stubs: a dataclass per message with encode/decode in pure Python, constants per enum, and a module-level
//! function per service method over `mud.Peer`.
//!
//! Standard library only, typed: a field's annotation is its Python type, a function's its arguments and return; `mud` is
//! imported for the service functions' types alone. oneof members are plain fields: set at most one.
use crate::parse::{Field, Schema, Ty};
use std::fmt::Write;

fn ident(name: &str) -> String { name.replace('.', "_") }

const KEYWORDS: &[&str] = &["False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "match", "case", "type"];

/// A field name that is a Python keyword gets a trailing underscore, as PEP 8 suggests.
fn field(name: &str) -> String { if KEYWORDS.contains(&name) { format!("{name}_") } else { name.to_string() } }

const HELPERS: &str = r#"
def _uv(v, out):
    while v > 0x7F:
        out.append((v & 0x7F) | 0x80)
        v >>= 7
    out.append(v)

def _tag(n, wt, out): _uv((n << 3) | wt, out)
def _ld(n, b, out): _tag(n, 2, out); _uv(len(b), out); out += b
def _u64(v): return v & 0xFFFFFFFFFFFFFFFF
def _i32(v): v &= 0xFFFFFFFF; return v - (1 << 32) if v & 0x80000000 else v
def _i64(v): v = _u64(v); return v - (1 << 64) if v & (1 << 63) else v
def _zz32(v): return ((v << 1) ^ (v >> 31)) & 0xFFFFFFFF
def _zz64(v): return ((v << 1) ^ (v >> 63)) & 0xFFFFFFFFFFFFFFFF
def _unzz(v): return (v >> 1) ^ -(v & 1)

def _rv(b, i):
    v = shift = 0
    while True:
        if i >= len(b): raise ValueError("truncated")
        c = b[i]; i += 1
        v |= (c & 0x7F) << shift
        if c < 0x80: return v, i
        shift += 7
        if shift > 63: raise ValueError("varint overflow")

def _fields(b):
    i = 0
    while i < len(b):
        tag, i = _rv(b, i)
        num, wt = tag >> 3, tag & 7
        if num == 0: raise ValueError("field 0")
        if wt == 0: v, i = _rv(b, i)
        elif wt == 1: v = int.from_bytes(b[i:i + 8], "little"); i += 8
        elif wt == 5: v = int.from_bytes(b[i:i + 4], "little"); i += 4
        elif wt == 2: n, i = _rv(b, i); v = bytes(b[i:i + n]); i += n
        else: raise ValueError("bad wire type")
        if i > len(b): raise ValueError("truncated")
        yield num, wt, v

def _packed(b):
    i = 0
    while i < len(b):
        v, i = _rv(b, i)
        yield v

def _fixed(b, size):
    for i in range(0, len(b) - len(b) % size, size): yield int.from_bytes(b[i:i + size], "little")
"#;

/// The Python type of one value of `t`.
fn py_type(t: &Ty) -> String {
    match t { Ty::Bool => "bool".into(), Ty::Float | Ty::Double => "float".into(), Ty::String => "str".into(), Ty::Bytes => "bytes".into(), Ty::Message(m) => ident(m), _ => "int".into() }
}

/// A field's annotation: a list when repeated, `None` until a message is set.
fn annotation(f: &Field) -> String {
    if f.repeated { format!("list[{}]", py_type(&f.ty)) } else if matches!(f.ty, Ty::Message(_)) { format!("{} | None", py_type(&f.ty)) } else { py_type(&f.ty) }
}

/// The distinct messages of `names` as one union, in order.
fn union<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let mut u: Vec<String> = Vec::new();
    for n in names.map(|n| ident(n)) { if !u.contains(&n) { u.push(n); } }
    if u.is_empty() { "object".into() } else { u.join(" | ") }
}

fn py_default(f: &Field) -> String {
    if f.repeated { return "field(default_factory=list)".into(); }
    match f.ty { Ty::Bool => "False".into(), Ty::String => "\"\"".into(), Ty::Bytes => "b\"\"".into(), Ty::Float | Ty::Double => "0.0".into(), Ty::Message(_) => "None".into(), _ => "0".into() }
}

/// Python statements that append one value `v` of type `t` as field `n`.
fn write_one(t: &Ty, n: u32, v: &str) -> String {
    match t {
        Ty::Bool => format!("_tag({n}, 0, out); _uv(1 if {v} else 0, out)"),
        Ty::Int32 | Ty::Int64 | Ty::Enum(_) => format!("_tag({n}, 0, out); _uv(_u64({v}), out)"),
        Ty::Uint32 | Ty::Uint64 => format!("_tag({n}, 0, out); _uv({v}, out)"),
        Ty::Sint32 => format!("_tag({n}, 0, out); _uv(_zz32({v}), out)"),
        Ty::Sint64 => format!("_tag({n}, 0, out); _uv(_zz64({v}), out)"),
        Ty::Fixed32 => format!("_tag({n}, 5, out); out += ({v}).to_bytes(4, \"little\")"),
        Ty::Sfixed32 => format!("_tag({n}, 5, out); out += ({v} & 0xFFFFFFFF).to_bytes(4, \"little\")"),
        Ty::Float => format!("_tag({n}, 5, out); out += struct.pack(\"<f\", {v})"),
        Ty::Fixed64 => format!("_tag({n}, 1, out); out += ({v}).to_bytes(8, \"little\")"),
        Ty::Sfixed64 => format!("_tag({n}, 1, out); out += _u64({v}).to_bytes(8, \"little\")"),
        Ty::Double => format!("_tag({n}, 1, out); out += struct.pack(\"<d\", {v})"),
        Ty::String | Ty::Message(_) => format!("_ld({n}, {v}.encode(), out)"),
        Ty::Bytes => format!("_ld({n}, {v}, out)"),
    }
}

/// (wire type, Python expression converting the raw `v`)
fn read_one(t: &Ty) -> (u8, String) {
    match t {
        Ty::Bool => (0, "v != 0".into()), Ty::Int32 | Ty::Enum(_) => (0, "_i32(v)".into()), Ty::Int64 => (0, "_i64(v)".into()),
        Ty::Uint32 | Ty::Uint64 => (0, "v".into()), Ty::Sint32 | Ty::Sint64 => (0, "_unzz(v)".into()),
        Ty::Fixed32 => (5, "v".into()), Ty::Sfixed32 => (5, "_i32(v)".into()), Ty::Float => (5, "struct.unpack(\"<f\", v.to_bytes(4, \"little\"))[0]".into()),
        Ty::Fixed64 => (1, "v".into()), Ty::Sfixed64 => (1, "_i64(v)".into()), Ty::Double => (1, "struct.unpack(\"<d\", v.to_bytes(8, \"little\"))[0]".into()),
        Ty::String => (2, "v.decode()".into()), Ty::Bytes => (2, "v".into()), Ty::Message(m) => (2, format!("{}.decode(v)", ident(m))),
    }
}

/// The Python module for a schema, over `mud.Peer`.
#[must_use]
pub fn emit(s: &Schema) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "# Generated by mud gen. Do not edit. oneof members are plain fields: set at most one.\nfrom __future__ import annotations\nfrom dataclasses import dataclass, field\nimport struct\ntry: import mud   # the wrapper, for the service functions' types; the messages need the standard library alone\nexcept ImportError: pass\n{HELPERS}");
    for e in &s.enums {
        let _ = writeln!(o, "\nclass {}:", ident(&e.name));
        for (v, n) in &e.values { let _ = writeln!(o, "    {v} = {n}"); }
    }
    // dependencies first so decode() can reference nested classes at call time; Python resolves names lazily, so order is cosmetic
    for m in &s.messages {
        let name = ident(&m.name);
        let _ = writeln!(o, "\n@dataclass\nclass {name}:");
        if m.fields.is_empty() { let _ = writeln!(o, "    pass"); }
        for f in &m.fields { let _ = writeln!(o, "    {}: {} = {}", field(&f.name), annotation(f), py_default(f)); }
        let _ = writeln!(o, "\n    def encode(self) -> bytes:\n        out = bytearray()");
        for f in &m.fields {
            let n = field(&f.name);
            if f.repeated { let _ = writeln!(o, "        for v in self.{n}: {}", write_one(&f.ty, f.number, "v")); }
            else if matches!(f.ty, Ty::Message(_)) { let _ = writeln!(o, "        if self.{n} is not None: {}", write_one(&f.ty, f.number, &format!("self.{n}"))); }
            else { let _ = writeln!(o, "        if self.{n}: {}", write_one(&f.ty, f.number, &format!("self.{n}"))); }
        }
        let _ = writeln!(o, "        return bytes(out)\n\n    @classmethod\n    def decode(cls, b: bytes) -> {name}:\n        m = cls()\n        for num, wt, v in _fields(b):");
        for f in &m.fields {
            let (wt, conv) = read_one(&f.ty);
            let fname = field(&f.name);
            if f.repeated {
                let _ = writeln!(o, "            if num == {} and wt == {wt}: m.{}.append({conv})", f.number, fname);
                match wt {
                    0 => { let _ = writeln!(o, "            elif num == {} and wt == 2: m.{}.extend({} for v in _packed(v))", f.number, fname, conv); }
                    5 => { let _ = writeln!(o, "            elif num == {} and wt == 2: m.{}.extend({} for v in _fixed(v, 4))", f.number, fname, conv); }
                    1 => { let _ = writeln!(o, "            elif num == {} and wt == 2: m.{}.extend({} for v in _fixed(v, 8))", f.number, fname, conv); }
                    _ => {}
                }
            } else {
                let _ = writeln!(o, "            if num == {} and wt == {wt}: m.{} = {conv}", f.number, fname);
            }
        }
        let _ = writeln!(o, "        return m\n");
    }
    for svc in &s.services {
        let topic = if s.package.is_empty() { svc.name.clone() } else { format!("{}.{}", s.package, svc.name) };
        let _ = writeln!(o, "\nclass {}:\n    TOPIC = {topic:?}\n\n    @staticmethod\n    def respond(peer: mud.Peer, now: int, call: mud.Incoming, resp: {}, status: int = 0, end: bool = True, seq: int = 0, detail: str = \"\") -> None:\n        \"\"\"Answer a call from this service. Stream: seq 0..n, end on the last.\"\"\"\n        peer.reply(now, call, status, resp.encode(), end, seq, detail)\n", svc.name, union(svc.methods.iter().filter(|m| m.topic.is_none()).map(|m| &m.output)));
        for m in &svc.methods {
            let ttl = m.ttl_secs.map_or_else(|| "ttl_secs".to_string(), |t| t.to_string());
            let sig = if m.ttl_secs.is_some() { "" } else { ", ttl_secs: int" };
            if let Some(t) = &m.topic {
                let _ = writeln!(o, "    @staticmethod\n    def {}(peer: mud.Peer, now: int, req: {}{sig}) -> None:\n        \"\"\"Publishes to {t:?}; no reply.\"\"\"\n        peer.publish(now, {t:?}, req.encode(), {ttl})\n", m.name, ident(&m.input));
            } else {
                let _ = writeln!(o, "    @staticmethod\n    def {}(peer: mud.Peer, now: int, req: {}{sig}) -> bytes:\n        \"\"\"Returns the call id; the reply arrives through deliver() with that id, or expires.\"\"\"\n        return peer.call(now, {}.TOPIC, {:?}, req.encode(), {ttl})\n", m.name, ident(&m.input), svc.name, m.name);
            }
        }
        let _ = writeln!(o, "    @staticmethod\n    def request(incoming: mud.Incoming) -> tuple[str, {}] | None:\n        \"\"\"(method, decoded request) for a call on this service, else None.\"\"\"\n        decoders = {{", union(svc.methods.iter().map(|m| &m.input)));
        for m in &svc.methods { let _ = writeln!(o, "            {:?}: {},", m.name, ident(&m.input)); }
        let _ = writeln!(o, "        }}\n        d = decoders.get(incoming.method)\n        return (incoming.method, d.decode(incoming.bytes)) if d else None\n");
    }
    o
}
