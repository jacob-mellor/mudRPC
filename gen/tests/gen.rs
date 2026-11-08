//! The parser on every build; the generated stubs compiled and run against protoc in e2e.
#![warn(clippy::pedantic, clippy::nursery)]
use mudrpc_gen::parse::{self, Ty};
use mudrpc_gen::{c, python, rust};

const NAV: &str = r#"
syntax = "proto3";
package nav;
import "google/protobuf/descriptor.proto";
import "mud_options.proto";
option java_package = "io.example";   // ignored

// A waypoint.
message Waypoint { double lat = 1; double lon = 2; float alt = 3; uint32 speed = 4; }

message Mission {
  string name = 1;
  repeated Waypoint points = 2;
  repeated uint32 ids = 3;
  Status status = 4;
  enum Status { UNKNOWN = 0; READY = 1; }
  message Meta { bytes blob = 1; sint64 offset = 2; sint32 small = 3; fixed64 big = 4; sfixed32 neg = 5; }
  Meta meta = 5;
  oneof kind { bool armed = 6; int64 code = 7; }
  reserved 8, 9;
  nav.Waypoint home = 10;
}
message Ack { bool ok = 1; }

service Flight {
  rpc Upload(Mission) returns (Ack) { option (mud.ttl_secs) = 3600; }
  rpc Frames(Ack) returns (stream Waypoint);
  rpc Position(Waypoint) returns (Ack) { option (mud.topic) = "nav.position"; option (mud.ttl_secs) = 5; }
}
"#;

#[test]
fn parses_the_sample_with_nesting_oneof_options_and_streams() {
    let s = parse::parse(NAV).unwrap();
    assert_eq!(s.package, "nav");
    let names: Vec<&str> = s.messages.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["Waypoint", "Mission.Meta", "Mission", "Ack"]);
    let mission = s.messages.iter().find(|m| m.name == "Mission").unwrap();
    let f = |n: &str| mission.fields.iter().find(|f| f.name == n).unwrap();
    assert_eq!((f("points").ty.clone(), f("points").repeated), (Ty::Message("Waypoint".into()), true));
    assert_eq!(f("status").ty, Ty::Enum("Mission.Status".into()));
    assert_eq!(f("meta").ty, Ty::Message("Mission.Meta".into()));
    assert_eq!(f("home").ty, Ty::Message("Waypoint".into()), "package-qualified names resolve");
    assert_eq!((f("armed").ty.clone(), f("code").number), (Ty::Bool, 7));
    assert_eq!(s.enums[0].values, [("UNKNOWN".to_string(), 0), ("READY".to_string(), 1)]);
    let svc = &s.services[0];
    assert_eq!(svc.name, "Flight");
    assert_eq!((svc.methods[0].ttl_secs, svc.methods[0].topic.clone()), (Some(3600), None));
    assert!(svc.methods[1].server_stream && !svc.methods[1].client_stream);
    assert_eq!(svc.methods[2].topic.as_deref(), Some("nav.position"));
}

#[test]
fn parses_the_shipped_options_file() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../proto/mud_options.proto")).unwrap();
    let s = parse::parse(&text).unwrap();
    assert_eq!((s.package.as_str(), s.messages.len()), ("mud", 0), "extend blocks are skipped");
}

#[test]
fn errors_name_the_line_and_never_panic() {
    let bad = |t: &str| parse::parse(t).unwrap_err();
    assert_eq!(bad("syntax = \"proto2\";").1, "only proto3");
    assert_eq!(bad("syntax = \"proto3\";\nmessage A {\n  map<string, int32> m = 1;\n}").0, 3);
    assert_eq!(bad("syntax = \"proto3\";\nmessage A {\n  Nope x = 1;\n}").0, 3);
    assert_eq!(bad("syntax = \"proto3\";\nmessage A {\n  int32 x = 0;\n}").0, 3);
    assert_eq!(bad("syntax = \"proto3\";\nmessage A {\n  int32 x = 19500;\n}").0, 3);
    assert_eq!(bad("syntax = \"proto3\";\nmessage A {\n  int32 x = 1\n}").0, 4);
    assert_eq!(bad("syntax = \"proto3\";\nenum E { A = 1; }").1, "the first enum value must be 0 in proto3");
    assert_eq!(bad("syntax = \"proto3\";\nservice S { rpc M(Nope) returns (Nope); }").1, "service S: unknown message type Nope");
    assert_eq!(bad("syntax = \"proto3\";\nmessage A { /* never closed").0, 2);
    let deep = format!("syntax = \"proto3\";\n{}{}", "message M {\n".repeat(10_000), "}".repeat(10_000));
    assert_eq!(bad(&deep), (34, "messages nested deeper than 32".into()), "a ten-thousand-deep file is a line number, not a stack overflow");
    assert_eq!(bad("syntax = \"proto3\";\nmessage \"A\" { int32 x = 1; }").1, "expected a name", "a name is an identifier: nothing a name carries can reach a C literal unescaped");
    assert_eq!(bad("syntax = \"proto3\";\nmessage A { string s = \"oops").0, 2);
    assert_eq!(bad("syntax = \"proto3\";\nmessage A {\n  int32 a = 1;\n  int32 b = 1;\n}").0, 4);
    assert_eq!(bad("syntax = \"proto3\";\nmessage A {\n  int32 a = 1;\n  int32 a = 2;\n}").0, 4);
    assert!(parse::parse("").is_err());
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..20_000 {
        let n = (rng % 60) as usize;
        let s: String = (0..n).map(|_| { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; (b' ' + (rng % 95) as u8) as char }).collect();
        let _ = parse::parse(&format!("syntax = \"proto3\";\n{s}"));
    }
}

#[test]
fn the_rust_stub_has_the_expected_shape() {
    let code = rust::emit(&parse::parse(NAV).unwrap());
    for needle in ["pub struct Mission {", "pub points: Vec<Waypoint>,", "pub meta: Option<Box<Mission_Meta>>,", "pub mod Mission_Status {", "pub const READY: i32 = 1;",
                   "pub mod Flight {", "pub const TOPIC: &str = \"nav.Flight\";", "pub fn Upload<", "peer.call(now, TOPIC, \"Upload\", &buf[..n], 3600)", "peer.publish(now, \"nav.position\", &buf[..n], 5)",
                   "Field::Bytes(3, mut b) => while !b.is_empty()", "pub fn request(call: &Call)", "Server stream"] {
        assert!(code.contains(needle), "missing {needle}\n{code}");
    }
    let kw = rust::emit(&parse::parse("syntax = \"proto3\"; message K { string type = 1; int32 r#ref = 2; }").unwrap_or_else(|_| parse::parse("syntax = \"proto3\"; message K { string type = 1; int32 match = 2; }").unwrap()));
    for needle in ["pub r#type: String", "pub r#match: i32", "m.r#type ="] {
        assert!(kw.contains(needle), "missing {needle}\n{kw}");
    }
}

/// Generate into a throwaway crate through `mudrpc_gen::build`, compile, round-trip, and cross-check the bytes with protoc
/// when it is installed. End to end: `cargo test -- --include-ignored`.
#[test]
fn the_python_stub_has_the_expected_shape() {
    let code = python::emit(&parse::parse(NAV).unwrap());
    for needle in ["class Mission:", "points: list[Waypoint] = field(default_factory=list)", "meta: Mission_Meta | None = None", "class Mission_Status:", "    READY = 1", "class Flight:", "TOPIC = \"nav.Flight\"",
                   "def Upload(peer: mud.Peer, now: int, req: Mission) -> bytes:", "return peer.call(now, Flight.TOPIC, \"Upload\", req.encode(), 3600)", "peer.publish(now, \"nav.position\", req.encode(), 5)", "elif num == 3 and wt == 2: m.ids.extend(v for v in _packed(v))", "def request(incoming: mud.Incoming) -> tuple[str, Mission | Ack | Waypoint] | None:"] {
        assert!(code.contains(needle), "missing {needle}\n{code}");
    }
    let kw = python::emit(&parse::parse("syntax = \"proto3\"; message K { string from = 1; int32 class = 2; }").unwrap());
    for needle in ["    from_: str", "    class_: int", "m.from_ = ", "if self.class_:"] { assert!(kw.contains(needle), "missing {needle}\n{kw}"); }
}

#[test]
fn the_c_stub_has_the_expected_shape_and_refuses_recursion() {
    let code = c::emit(&parse::parse(NAV).unwrap()).unwrap();
    for needle in ["typedef struct Mission {", "Waypoint points[MUD_REPEATED]; size_t points_len;", "int has_meta; Mission_Meta meta;", "mud_view name;", "Mission_Status_READY = 1,",
                   "static inline size_t Mission_encode(const Mission* m, uint8_t* o, size_t cap)", "static inline int Mission_decode(Mission* m, const uint8_t* b, size_t len)", "#define Flight_TOPIC \"nav.Flight\"",
                   "static inline int32_t Flight_Upload(void* peer, size_t len, uint32_t now, const Mission* req, mud_id* id)", "return mud_call(peer, len, now, service, method, payload, 3600, id);", "static inline int32_t Flight_Position(void* peer, size_t len, uint32_t now, const Waypoint* req)", "static inline int Flight_method(const mud_incoming* in)"] {
        assert!(code.contains(needle), "missing {needle}\n{code}");
    }
    let pos = code.find("typedef struct Mission_Meta").unwrap();
    assert!(pos < code.find("typedef struct Mission {").unwrap(), "children are declared before parents");
    for l in code.lines().filter(|l| l.trim_start().starts_with("if (num == ")) { assert!(l.trim_end().ends_with("continue; }"), "a decode branch that matched falls through: {l}"); }
    let rec = parse::parse("syntax = \"proto3\"; message Node { Node next = 1; }").unwrap();
    assert!(c::emit(&rec).unwrap_err().contains("recursive"));
}

/// Every field and function of the sample's Python module annotated with its type, as Python resolves the annotations.
const TYPED: &str = r#"
import mud, typing
hints = lambda o: typing.get_type_hints(o)
assert hints(nav.Waypoint) == {"lat": float, "lon": float, "alt": float, "speed": int}, hints(nav.Waypoint)
assert hints(nav.Mission) == {"name": str, "points": list[nav.Waypoint], "ids": list[int], "status": int, "meta": nav.Mission_Meta | None, "armed": bool, "code": int, "home": nav.Waypoint | None}, hints(nav.Mission)
assert hints(nav.Mission_Meta) == {"blob": bytes, "offset": int, "small": int, "big": int, "neg": int} and hints(nav.Ack) == {"ok": bool}
for c in (nav.Waypoint, nav.Mission_Meta, nav.Mission, nav.Ack): assert hints(c.encode) == {"return": bytes} and hints(c.decode) == {"b": bytes, "return": c}, c
call, none = {"peer": mud.Peer, "now": int}, type(None)
assert hints(nav.Flight.Upload) == {**call, "req": nav.Mission, "return": bytes} and hints(nav.Flight.Frames) == {**call, "req": nav.Ack, "ttl_secs": int, "return": bytes}
assert hints(nav.Flight.Position) == {**call, "req": nav.Waypoint, "return": none}, hints(nav.Flight.Position)
assert hints(nav.Flight.respond) == {**call, "call": mud.Incoming, "resp": nav.Ack | nav.Waypoint, "status": int, "end": bool, "seq": int, "detail": str, "return": none}, hints(nav.Flight.respond)
assert hints(nav.Flight.request) == {"incoming": mud.Incoming, "return": tuple[str, nav.Mission | nav.Ack | nav.Waypoint] | None}, hints(nav.Flight.request)
"#;

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn generated_stubs_compile_round_trip_and_agree_with_protoc() {
    let dir = env!("CARGO_MANIFEST_DIR");
    let tmp = std::env::temp_dir().join("mudrpc-gen-e2e");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("proto")).unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("proto/nav.proto"), NAV).unwrap();
    std::fs::copy(format!("{dir}/../proto/mud_options.proto"), tmp.join("proto/mud_options.proto")).unwrap();
    std::fs::write(tmp.join("Cargo.toml"), format!("[package]\nname = \"product\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nmudrpc-core = {{ path = {:?} }}\n[build-dependencies]\nmudrpc-gen = {{ path = {:?} }}\n[workspace]\n", format!("{dir}/../core"), dir)).unwrap();
    std::fs::write(tmp.join("build.rs"), "fn main() { mudrpc_gen::build(\"proto\"); }\n").unwrap();
    std::fs::write(tmp.join("src/main.rs"), r#"
mod nav { include!(concat!(env!("OUT_DIR"), "/nav.rs")); }
use nav::*;
fn main() {
    let m = Mission { name: "alpha".into(), points: vec![Waypoint { lat: 50.45, lon: 30.52, alt: 120.5, speed: 7 }, Waypoint::default()], ids: vec![1, 300, 70000],
        status: Mission_Status::READY, meta: Some(Box::new(Mission_Meta { blob: vec![1, 2, 3], offset: -5, small: -1, big: 1 << 40, neg: -9 })), armed: false, code: -3, home: None };
    let mut buf = [0u8; 512];
    let n = m.encode(&mut buf).unwrap();
    assert_eq!(Mission::decode(&buf[..n]).unwrap(), m);
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg == "encode" { use std::io::Write; std::io::stdout().write_all(&buf[..n]).unwrap(); return; }
    if arg == "decode" { use std::io::Read; let mut b = Vec::new(); std::io::stdin().read_to_end(&mut b).unwrap(); let d = Mission::decode(&b).unwrap(); assert_eq!(d, m, "protoc's bytes decode to the same message"); }
    println!("ok");
}
"#).unwrap();
    let run = |args: &[&str], stdin: Option<&[u8]>| {
        let mut c = std::process::Command::new("cargo");
        c.args(["run", "-q", "--"]).args(args).current_dir(&tmp).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = c.spawn().unwrap();
        if let Some(b) = stdin { use std::io::Write; child.stdin.take().unwrap().write_all(b).unwrap(); } else { drop(child.stdin.take()); }
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    };
    assert_eq!(String::from_utf8_lossy(&run(&[], None)), "ok\n");
    let ours = run(&["encode"], None);
    if std::process::Command::new("protoc").arg("--version").output().is_ok() {
        let decode = std::process::Command::new("protoc").args(["-I", tmp.join("proto").to_str().unwrap(), "--decode=nav.Mission", "nav.proto"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().and_then(|mut c| { use std::io::Write; c.stdin.take().unwrap().write_all(&ours)?; c.wait_with_output() }).unwrap();
        let text = String::from_utf8_lossy(&decode.stdout).to_string();
        for needle in ["name: \"alpha\"", "ids: 300", "ids: 70000", "status: READY", "offset: -5", "small: -1", "code: -3", "alt: 120.5", "neg: -9"] { assert!(text.contains(needle), "protoc did not see {needle} in\n{text}"); }
        let encode = std::process::Command::new("protoc").args(["-I", tmp.join("proto").to_str().unwrap(), "--encode=nav.Mission", "nav.proto"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().and_then(|mut c| { use std::io::Write; c.stdin.take().unwrap().write_all(text.as_bytes())?; c.wait_with_output() }).unwrap();
        assert!(!encode.stdout.is_empty());
        run(&["decode"], Some(&encode.stdout));
    } else {
        eprintln!("protoc not installed: interop check skipped");
    }
    // Python: decode what Rust encoded, re-encode, and let Rust decode that; equal values both ways; every field and function typed.
    std::fs::write(tmp.join("nav.py"), python::emit(&parse::parse(NAV).unwrap())).unwrap();
    std::fs::write(tmp.join("ours.bin"), &ours).unwrap();
    let script = r#"
import sys, nav
m = nav.Mission.decode(open(sys.argv[1], "rb").read())
assert m.name == "alpha" and m.ids == [1, 300, 70000] and m.status == nav.Mission_Status.READY and m.code == -3 and m.armed is False, m
assert m.points[0].speed == 7 and abs(m.points[0].alt - 120.5) < 1e-6 and abs(m.points[0].lat - 50.45) < 1e-9 and m.points[1] == nav.Waypoint(), m.points
assert m.meta.blob == b"\x01\x02\x03" and m.meta.offset == -5 and m.meta.small == -1 and m.meta.big == 1 << 40 and m.meta.neg == -9, m.meta
assert nav.Mission.decode(m.encode()) == m
sys.stdout.buffer.write(m.encode())
"#;
    std::fs::write(tmp.join("check.py"), [script, TYPED].concat()).unwrap();
    let py = std::process::Command::new("python3").args(["-I", "-c", &format!("import sys; sys.path[:0] = [{:?}, {:?}]; import runpy; sys.argv = [{:?}, {:?}]; runpy.run_path({:?}, run_name='__main__')", tmp, format!("{dir}/../bindings/python"), tmp.join("check.py"), tmp.join("ours.bin"), tmp.join("check.py"))]).output().unwrap();
    assert!(py.status.success(), "{}", String::from_utf8_lossy(&py.stderr));
    run(&["decode"], Some(&py.stdout));
    // C: the same, compiled with -Wall -Wextra -Werror against mudrpc.h and the static library.
    std::fs::write(tmp.join("nav.h"), c::emit(&parse::parse(NAV).unwrap()).unwrap()).unwrap();
    std::fs::write(tmp.join("r.h"), c::emit(&parse::parse("syntax = \"proto3\"; message R { repeated int32 v = 1; }").unwrap()).unwrap()).unwrap();
    std::fs::write(tmp.join("tri.h"), c::emit(&parse::parse("syntax = \"proto3\"; message T {} service Tri { rpc P(T) returns (T) { option (mud.topic) = \"a??/b??=c?\"; } }").unwrap()).unwrap()).unwrap();
    std::fs::write(tmp.join("check.c"), r#"
#include <stdio.h>
#include <stdlib.h>
#include "nav.h"
#include "r.h"
#include "tri.h"
int main(int argc, char** argv) {
    (void)argc; FILE* f = fopen(argv[1], "rb"); uint8_t in[1024]; size_t n = fread(in, 1, sizeof in, f); fclose(f);
    Mission m; if (Mission_decode(&m, in, n)) return 1;
    if (m.name.len != 5 || memcmp(m.name.ptr, "alpha", 5) || m.ids_len != 3 || m.ids[2] != 70000 || m.status != Mission_Status_READY || m.code != -3 || m.armed) return 2;
    if (m.points_len != 2 || m.points[0].speed != 7 || m.points[0].alt != 120.5f || m.points[0].lat != 50.45 || m.points[1].speed != 0) return 3;
    if (!m.has_meta || m.meta.blob.len != 3 || m.meta.offset != -5 || m.meta.small != -1 || m.meta.big != (1ULL << 40) || m.meta.neg != -9 || m.has_home) return 4;
    uint8_t out[1024]; size_t w = Mission_encode(&m, out, sizeof out); if (w == MUD_ERR) return 5;
    Mission again; if (Mission_decode(&again, out, w) || again.ids_len != 3 || again.meta.offset != -5) return 6;
    uint8_t tiny[8]; if (Mission_encode(&m, tiny, sizeof tiny) != MUD_ERR) return 7;
    uint8_t junk[] = { 0x08, 0x80 }; if (Mission_decode(&again, junk, 2) != -1) return 8;
    /* 1, 300, -1 unpacked and packed; then packed 5 6, unpacked 7, packed 9: a branch that fell through would reread the stale view */
    uint8_t un[] = { 0x08, 1, 0x08, 0xAC, 2, 0x08, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 1 }, pk[] = { 0x0A, 13, 1, 0xAC, 2, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 1 };
    uint8_t mix[] = { 0x0A, 2, 5, 6, 0x08, 7, 0x0A, 1, 9 }; R r, s;
    if (R_decode(&r, un, sizeof un) || R_decode(&s, pk, sizeof pk) || r.v_len != 3 || memcmp(&r, &s, sizeof r) || r.v[1] != 300 || r.v[2] != -1) return 9;
    if (R_decode(&r, mix, sizeof mix) || r.v_len != 4 || r.v[0] != 5 || r.v[1] != 6 || r.v[2] != 7 || r.v[3] != 9) return 10;
    fwrite(out, 1, w, stdout);
    return 0;
}
"#).unwrap();
    assert!(std::process::Command::new("cargo").args(["build", "-p", "mudrpc-c"]).current_dir(format!("{dir}/..")).status().unwrap().success());
    let mut cc = std::process::Command::new("cc");
    cc.args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I", &format!("{dir}/../bindings/c"), "-I", tmp.to_str().unwrap(), tmp.join("check.c").to_str().unwrap(), &format!("{dir}/../target/debug/libmudrpc.a"), "-o", tmp.join("check").to_str().unwrap()]);
    if cfg!(target_os = "linux") { cc.args(["-lpthread", "-ldl", "-lm"]); }
    let built = cc.output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let c_out = std::process::Command::new(tmp.join("check")).arg(tmp.join("ours.bin")).output().unwrap();
    assert!(c_out.status.success(), "C check exited {:?}", c_out.status.code());
    run(&["decode"], Some(&c_out.stdout));
}

/// A topic option is a string, not an identifier: it reaches a C literal, a Python docstring and a Rust constant, so
/// every emitter escapes it and the parser admits only printable ASCII, 1..=64 bytes.
#[test]
fn a_topic_option_is_printable_ascii_and_reaches_every_emitter_escaped() {
    let proto = |t: &str| format!("syntax = \"proto3\"; message A {{}} service S {{ rpc P(A) returns (A) {{ option (mud.topic) = \"{t}\"; }} }}");
    let s = parse::parse(&proto("a\\\"b\\\\c*/d'e")).unwrap();
    assert_eq!(s.services[0].methods[0].topic.as_deref(), Some("a\"b\\c*/d'e"));
    let (c_, py, rs) = (c::emit(&s).unwrap(), python::emit(&s), rust::emit(&s));
    assert!(c_.contains("(const uint8_t*)\"a\\\"b\\\\c*/d'e\", 10 }"), "{c_}");
    assert!(!c_.contains("/* publishes to"), "no topic in a comment, where */ would end it\n{c_}");
    assert!(py.contains("\"\"\"Publishes to \"a\\\"b\\\\c*/d'e\"; no reply.\"\"\"") && py.contains("peer.publish(now, \"a\\\"b\\\\c*/d'e\","), "{py}");
    assert!(rs.contains("peer.publish(now, \"a\\\"b\\\\c*/d'e\","), "{rs}");
    let tri = c::emit(&parse::parse(&proto("a??/b?c")).unwrap()).unwrap();
    assert!(tri.contains("(const uint8_t*)\"a\\?\\?/b\\?c\", 7 }"), "every ? escaped, so no trigraph forms\n{tri}");
    for bad in ["", "x\\ny", "x\\ty", "\u{e9}", &"t".repeat(65)] {
        let e = parse::parse(&proto(bad)).unwrap_err();
        assert!(e.1.contains("printable ASCII") && e.0 == 1, "{bad:?}: {e:?}");
    }
    assert!(parse::parse(&proto(&"t".repeat(64))).is_ok());
}
