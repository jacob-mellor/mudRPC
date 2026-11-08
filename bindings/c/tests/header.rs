//! The header against the code: every function `ffi.rs` exports is declared in `mudrpc.h` with as many parameters, and
//! back, the wasm allocator aside, and every numeric constant has one value in both. Read as text, with `str` methods.
#![warn(clippy::pedantic, clippy::nursery)]
use std::collections::BTreeMap;

/// A file's functions with their parameter counts, and its constants with their values (`None`: one it could not read).
type Surface = (BTreeMap<String, usize>, BTreeMap<String, Option<i64>>);

fn params(list: &str) -> usize { if matches!(list.trim(), "" | "void") { 0 } else { list.split(',').count() } }
fn value(v: &str, known: &BTreeMap<String, Option<i64>>) -> Option<i64> { v.trim().parse().ok().or_else(|| known.get(v.trim()).copied().flatten()) }

/// `mudrpc.h`: comments out, `#define`s read, then each statement to its `;`: an `enum { .. }` or a prototype.
fn header(h: &str) -> Surface {
    let (mut code, mut rest, mut c) = (String::new(), h, BTreeMap::new());
    while let Some(i) = rest.find("/*") { code += &rest[..i]; rest = rest[i..].split_once("*/").map_or("", |(_, r)| r); }
    code += rest;
    let code: String = code.lines().filter(|l| {
        let d = l.strip_prefix("#define ").map(|d| d.split_whitespace().collect::<Vec<_>>());
        if let Some([n, v]) = d.as_deref() { c.insert((*n).to_string(), v.parse().ok()); }
        !l.starts_with('#')
    }).collect::<Vec<_>>().join("\n");
    let mut f = BTreeMap::new();
    for st in code.split(';') {
        if let Some((_, body)) = st.split_once("enum {") {
            for (n, v) in body.trim_end_matches(|x: char| x == '}' || x.is_whitespace()).split(',').filter_map(|i| i.split_once('=')) { let v = value(v, &c); c.insert(n.trim().to_string(), v); }
        } else if let Some((head, args)) = st.split_once('(').filter(|_| !st.contains("typedef")) {
            let name = head.rsplit(|x: char| !(x.is_alphanumeric() || x == '_')).next().unwrap_or("");
            if name.starts_with("mud_") { f.insert(name.to_string(), params(args.rsplit_once(')').map_or(args, |a| a.0))); }
        }
    }
    (f, c)
}

/// `ffi.rs`: each `pub const MUD_..` and each `extern "C" fn`, but those built for wasm alone.
fn ffi(rs: &str) -> Surface {
    let (mut f, mut c, mut prev) = (BTreeMap::new(), BTreeMap::new(), "");
    for l in rs.lines() {
        if let Some((n, v)) = l.split_once("pub const MUD_").and_then(|(_, r)| r.split_once(':')).and_then(|(n, r)| Some((n, r.split_once('=')?.1))) { let v = value(v.trim_end().trim_end_matches(';'), &c); c.insert(format!("MUD_{n}"), v); }
        if let Some((n, a)) = l.split_once("extern \"C\" fn ").and_then(|(_, r)| r.split_once('(')).filter(|_| !prev.contains("wasm32")) { f.insert(n.to_string(), params(a.split_once(')').map_or(a, |a| a.0))); }
        prev = l;
    }
    (f, c)
}

/// Every disagreement between the two, one line each.
fn drift(h: &str, rs: &str) -> Vec<String> {
    let ((hf, hc), (rf, rc)) = (header(h), ffi(rs));
    let mut out = Vec::new();
    for (n, p) in &rf { if hf.get(n) != Some(p) { out.push(format!("{n}: {p} parameters in ffi.rs, {:?} in mudrpc.h", hf.get(n))); } }
    for n in hf.keys().filter(|n| !rf.contains_key(*n)) { out.push(format!("{n}: in mudrpc.h, not exported by ffi.rs")); }
    for (n, v) in &rc { if v.is_none() || hc.get(n) != Some(v) { out.push(format!("{n}: {v:?} in ffi.rs, {:?} in mudrpc.h", hc.get(n).copied().flatten())); } }
    for n in hc.keys().filter(|n| !rc.contains_key(*n)) { out.push(format!("{n}: in mudrpc.h, not in ffi.rs")); }
    if rf.len() < 40 || rc.len() < 25 { out.push(format!("read {} functions and {} constants: the parse missed some", rf.len(), rc.len())); }
    out
}

#[test]
fn the_header_and_the_ffi_agree_on_every_function_and_constant() {
    let read = |f: &str| std::fs::read_to_string(format!("{}/{f}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let (h, rs) = (read("mudrpc.h"), read("ffi.rs"));
    let d = drift(&h, &rs);
    assert!(d.is_empty(), "{}", d.join("\n"));
    assert_eq!(drift(&h.replace("MUD_ERATE = -9", "MUD_ERATE = -10"), &rs).len(), 1, "a planted value is found");
    assert_eq!(drift(&h.replace("int32_t mud_sent(void* mem, size_t len)", "int32_t mud_sent(void* mem)"), &rs).len(), 1, "and a planted parameter");
}

/// Compiles `header` alone with `compiler` and `std`, pedantic, every warning an error.
fn compiles(compiler: &str, std: &str, ext: &str, header: &str) -> bool {
    let dir = std::env::temp_dir().join(format!("mudrpc-header-{std}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mudrpc.h"), header).unwrap();
    let src = dir.join(format!("use.{ext}"));
    std::fs::write(&src, "#include \"mudrpc.h\"\nint main(void) { return 0; }\n").unwrap();
    std::process::Command::new(compiler).args([&format!("-std={std}"), "-pedantic", "-Wall", "-Wextra", "-Werror", "-fsyntax-only"]).arg(&src).stderr(std::process::Stdio::null()).status().unwrap().success()
}

#[test]
fn the_header_is_strict_c89_and_cpp17() {
    let h = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/mudrpc.h")).unwrap();
    assert!(compiles("cc", "c89", "c", &h), "C89");
    assert!(compiles("c++", "c++17", "cc", &h), "C++17");
    assert!(!compiles("cc", "c89", "c", &h.replace("#define MUDRPC_H", "#define MUDRPC_H\n// a C99 comment")), "a planted C99 line fails C89");
}
