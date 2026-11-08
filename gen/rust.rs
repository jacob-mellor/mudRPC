//! Rust stubs: a struct per message with encode/decode over `mudrpc_core::codec`, constants per enum, and a module per
//! service with one function per method and a request parser for servers.
//!
//! Needs `alloc`. Members of a `oneof` are
//! plain fields: set at most one, as protobuf requires; a decoder keeps the last one it reads.
use crate::parse::{Field, Schema, Ty};
use std::fmt::Write;

fn ident(name: &str) -> String { name.replace('.', "_") }

const KEYWORDS: &[&str] = &["as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen"];

/// A field name that is a Rust keyword gets the raw prefix.
fn field(name: &str) -> String { if KEYWORDS.contains(&name) { format!("r#{name}") } else { name.to_string() } }

fn rust_ty(t: &Ty) -> String {
    match t {
        Ty::Bool => "bool".into(), Ty::Int32 | Ty::Sint32 | Ty::Sfixed32 | Ty::Enum(_) => "i32".into(), Ty::Int64 | Ty::Sint64 | Ty::Sfixed64 => "i64".into(),
        Ty::Uint32 | Ty::Fixed32 => "u32".into(), Ty::Uint64 | Ty::Fixed64 => "u64".into(), Ty::Float => "f32".into(), Ty::Double => "f64".into(),
        Ty::String => "String".into(), Ty::Bytes => "Vec<u8>".into(), Ty::Message(m) => ident(m),
    }
}

fn field_ty(f: &Field) -> String {
    let t = rust_ty(&f.ty);
    if f.repeated { format!("Vec<{t}>") } else if matches!(f.ty, Ty::Message(_)) { format!("Option<Box<{t}>>") } else { t }
}

/// (how to write one value `v`, the default test for a singular field)
fn write_one(t: &Ty, n: u32, v: &str) -> (String, String) {
    match t {
        Ty::Bool => (format!("w.varint({n}, {v} as u64)?"), v.to_string()),
        Ty::Int32 | Ty::Enum(_) => (format!("w.varint({n}, {v} as i64 as u64)?"), format!("{v} != 0")),
        Ty::Int64 | Ty::Uint32 => (format!("w.varint({n}, {v} as u64)?"), format!("{v} != 0")),
        Ty::Uint64 => (format!("w.varint({n}, {v})?"), format!("{v} != 0")),
        Ty::Sint32 => (format!("w.varint({n}, (({v} << 1) ^ ({v} >> 31)) as u32 as u64)?"), format!("{v} != 0")),
        Ty::Sint64 => (format!("w.varint({n}, (({v} << 1) ^ ({v} >> 63)) as u64)?"), format!("{v} != 0")),
        Ty::Fixed32 => (format!("w.fixed32({n}, {v})?"), format!("{v} != 0")),
        Ty::Sfixed32 => (format!("w.fixed32({n}, {v} as u32)?"), format!("{v} != 0")),
        Ty::Float => (format!("w.fixed32({n}, {v}.to_bits())?"), format!("{v} != 0.0")),
        Ty::Fixed64 => (format!("w.fixed64({n}, {v})?"), format!("{v} != 0")),
        Ty::Sfixed64 => (format!("w.fixed64({n}, {v} as u64)?"), format!("{v} != 0")),
        Ty::Double => (format!("w.fixed64({n}, {v}.to_bits())?"), format!("{v} != 0.0")),
        Ty::String => (format!("w.bytes({n}, {v}.as_bytes())?"), format!("!{v}.is_empty()")),
        Ty::Bytes => (format!("w.bytes({n}, &{v})?"), format!("!{v}.is_empty()")),
        Ty::Message(_) => (format!("w.message({n}, |w| {v}.write(w))?"), String::new()),
    }
}

/// (match pattern for one wire field, conversion of the bound `v` to the Rust value)
fn read_one(t: &Ty, n: u32) -> (String, String) {
    match t {
        Ty::Bool => (format!("Field::Varint({n}, v)"), "v != 0".into()),
        Ty::Int32 | Ty::Enum(_) => (format!("Field::Varint({n}, v)"), "v as i32".into()),
        Ty::Int64 => (format!("Field::Varint({n}, v)"), "v as i64".into()),
        Ty::Uint32 => (format!("Field::Varint({n}, v)"), "v as u32".into()),
        Ty::Uint64 => (format!("Field::Varint({n}, v)"), "v".into()),
        Ty::Sint32 => (format!("Field::Varint({n}, v)"), "((v as u32 >> 1) as i32) ^ -((v & 1) as i32)".into()),
        Ty::Sint64 => (format!("Field::Varint({n}, v)"), "((v >> 1) as i64) ^ -((v & 1) as i64)".into()),
        Ty::Fixed32 => (format!("Field::Fixed32({n}, v)"), "v".into()),
        Ty::Sfixed32 => (format!("Field::Fixed32({n}, v)"), "v as i32".into()),
        Ty::Float => (format!("Field::Fixed32({n}, v)"), "f32::from_bits(v)".into()),
        Ty::Fixed64 => (format!("Field::Fixed64({n}, v)"), "v".into()),
        Ty::Sfixed64 => (format!("Field::Fixed64({n}, v)"), "v as i64".into()),
        Ty::Double => (format!("Field::Fixed64({n}, v)"), "f64::from_bits(v)".into()),
        Ty::String => (format!("Field::Bytes({n}, v)"), "core::str::from_utf8(v).map_err(|_| codec::Error::Utf8)?.into()".into()),
        Ty::Bytes => (format!("Field::Bytes({n}, v)"), "v.to_vec()".into()),
        Ty::Message(m) => (format!("Field::Bytes({n}, v)"), format!("{}::decode(v)?", ident(m))),
    }
}

/// Packed repeated scalars arrive as one length-delimited field; protoc emits them that way.
fn read_packed(t: &Ty, n: u32, f: &str) -> Option<String> {
    let conv = read_one(t, n).1;
    Some(match t {
        Ty::Fixed32 | Ty::Sfixed32 | Ty::Float => format!("Field::Bytes({n}, b) => for c in b.chunks_exact(4) {{ let v = u32::from_le_bytes(c.try_into().unwrap()); m.{f}.push({conv}); }},"),
        Ty::Fixed64 | Ty::Sfixed64 | Ty::Double => format!("Field::Bytes({n}, b) => for c in b.chunks_exact(8) {{ let v = u64::from_le_bytes(c.try_into().unwrap()); m.{f}.push({conv}); }},"),
        Ty::String | Ty::Bytes | Ty::Message(_) => return None,
        _ => format!("Field::Bytes({n}, mut b) => while !b.is_empty() {{ let (v, rest) = codec::varint(b)?; m.{f}.push({conv}); b = rest; }},"),
    })
}

/// The Rust source for a schema, for `include!` or `build`.
#[must_use]
pub fn emit(s: &Schema) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "// Generated by mud gen. Do not edit. oneof members are plain fields: set at most one.\nextern crate alloc;\n#[allow(unused_imports)]\nuse alloc::{{boxed::Box, string::String, vec::Vec}};\nuse mudrpc_core::codec::{{self, Field, Reader, Writer}};\n");
    let _ = writeln!(o, "/// Anything with a protobuf encoding.\npub trait Encode {{ fn encode(&self, buf: &mut [u8]) -> Result<usize, codec::Error>; }}\n");
    for e in &s.enums {
        let _ = writeln!(o, "#[allow(non_snake_case, non_upper_case_globals, non_camel_case_types)]\npub mod {} {{", ident(&e.name));
        for (v, n) in &e.values { let _ = writeln!(o, "    pub const {v}: i32 = {n};"); }
        let _ = writeln!(o, "}}\n");
    }
    for m in &s.messages {
        let name = ident(&m.name);
        let _ = writeln!(o, "#[allow(non_camel_case_types, non_snake_case)]\n#[derive(Debug, Clone, Default, PartialEq)]\npub struct {name} {{");
        for f in &m.fields { let _ = writeln!(o, "    pub {}: {},", field(&f.name), field_ty(f)); }
        let _ = writeln!(o, "}}\n\n#[allow(non_snake_case, unused, clippy::all)]\nimpl {name} {{\n    pub fn write(&self, w: &mut Writer) -> Result<(), codec::Error> {{");
        for f in &m.fields {
            let copy = !matches!(f.ty, Ty::String | Ty::Bytes | Ty::Message(_));
            if f.repeated {
                let (one, _) = write_one(&f.ty, f.number, if copy { "(*v)" } else { "v" });
                let _ = writeln!(o, "        for v in &self.{} {{ {one}; }}", field(&f.name));
            } else if matches!(f.ty, Ty::Message(_)) {
                let (one, _) = write_one(&f.ty, f.number, "v");
                let _ = writeln!(o, "        if let Some(v) = &self.{} {{ {one}; }}", field(&f.name));
            } else {
                let (one, test) = write_one(&f.ty, f.number, &format!("self.{}", field(&f.name)));
                let _ = writeln!(o, "        if {test} {{ {one}; }}");
            }
        }
        let _ = writeln!(o, "        Ok(())\n    }}\n\n    pub fn decode(b: &[u8]) -> Result<Self, codec::Error> {{\n        let mut m = Self::default();\n        let mut r = Reader::new(b);\n        while let Some(f) = r.next()? {{\n            match f {{");
        for f in &m.fields {
            let (pat, conv) = read_one(&f.ty, f.number);
            if f.repeated {
                let _ = writeln!(o, "                {pat} => m.{}.push({conv}),", field(&f.name));
                if let Some(packed) = read_packed(&f.ty, f.number, &field(&f.name)) { let _ = writeln!(o, "                {packed}"); }
            } else if matches!(f.ty, Ty::Message(_)) {
                let _ = writeln!(o, "                {pat} => m.{} = Some(Box::new({conv})),", field(&f.name));
            } else {
                let _ = writeln!(o, "                {pat} => m.{} = {conv},", field(&f.name));
            }
        }
        let _ = writeln!(o, "                _ => {{}}\n            }}\n        }}\n        Ok(m)\n    }}\n}}\n\nimpl Encode for {name} {{\n    fn encode(&self, buf: &mut [u8]) -> Result<usize, codec::Error> {{ let mut w = Writer::new(buf); self.write(&mut w)?; Ok(w.len()) }}\n}}\n");
    }
    for svc in &s.services {
        let topic = if s.package.is_empty() { svc.name.clone() } else { format!("{}.{}", s.package, svc.name) };
        let _ = writeln!(o, "#[allow(non_snake_case, dead_code)]\npub mod {} {{\n    use super::*;\n    use mudrpc_core::envelope::Id;\n    use mudrpc_core::rpc::{{self, Call, Peer}};\n    use mudrpc_core::seal::Sealer;\n\n    pub const TOPIC: &str = {topic:?};\n", svc.name);
        let g = "<S: Sealer, const MSG: usize, const PENDING: usize, const SEEN: usize, const SENDERS: usize, const OUT: usize>";
        let peer = "peer: &mut Peer<S, MSG, PENDING, SEEN, SENDERS, OUT>";
        let _ = writeln!(o, "    /// Answer a call from this service: status gRPC's number, detail its text or \"\". Stream: seq 0..n, end on the last.\n    #[allow(clippy::too_many_arguments)]\n    pub fn respond{g}({peer}, now: u32, call: &Call, seq: u32, status: u32, detail: &str, resp: &impl Encode, end: bool) -> Result<(), rpc::Error> {{\n        let mut buf = [0u8; MSG];\n        let n = resp.encode(&mut buf).map_err(|_| rpc::Error::TooLarge)?;\n        peer.reply(now, call, seq, status, detail, &buf[..n], end)\n    }}\n");
        for m in &svc.methods {
            let (ttl_param, ttl) = m.ttl_secs.map_or_else(|| (", ttl_secs: u32".into(), "ttl_secs".into()), |t| (String::new(), t.to_string()));
            let note = match (m.client_stream, m.server_stream) { (true, true) => "Bidirectional stream.", (true, false) => "Client stream: repeat with the same id is not yet supported; one call per item.", (false, true) => "Server stream: expect several replies with seq, the last with end.", _ => "Unary." };
            if let Some(t) = &m.topic {
                let _ = writeln!(o, "    /// {note} Publishes to {t:?}; no reply.\n    pub fn {}{g}({peer}, now: u32, req: &{}{ttl_param}) -> Result<(), rpc::Error> {{\n        let mut buf = [0u8; MSG];\n        let n = req.encode(&mut buf).map_err(|_| rpc::Error::TooLarge)?;\n        peer.publish(now, {t:?}, &buf[..n], {ttl})\n    }}\n", m.name, ident(&m.input));
            } else {
                let _ = writeln!(o, "    /// {note}\n    pub fn {}{g}({peer}, now: u32, req: &{}{ttl_param}) -> Result<Id, rpc::Error> {{\n        let mut buf = [0u8; MSG];\n        let n = req.encode(&mut buf).map_err(|_| rpc::Error::TooLarge)?;\n        peer.call(now, TOPIC, {:?}, &buf[..n], {ttl})\n    }}\n", m.name, ident(&m.input), m.name);
            }
        }
        let _ = writeln!(o, "    /// A call on this service, decoded. None if the method is not one of ours.\n    #[allow(clippy::large_enum_variant)]\n    #[derive(Debug, Clone, PartialEq)]\n    pub enum Request {{");
        for m in &svc.methods { let _ = writeln!(o, "        {}({}),", m.name, ident(&m.input)); }
        let _ = writeln!(o, "    }}\n\n    pub fn request(call: &Call) -> Option<Result<Request, codec::Error>> {{\n        match call.method {{");
        for m in &svc.methods { let _ = writeln!(o, "            {:?} => Some({}::decode(call.bytes).map(Request::{})),", m.name, ident(&m.input), m.name); }
        let _ = writeln!(o, "            _ => None,\n        }}\n    }}\n}}\n");
    }
    o
}
