//! mud gen <file.proto>... [--lang rust|python|c] [-o dir]   Stubs to stdout, or one file per .proto into dir.
//! mud provision <dir> [n]   n devices (default 1) into the fleet at dir: fleet.key once, then device-<sender> and a roster line each.
//! mud provision <dir> --key   a rotation's next key, fleet.key.next, beside the fleet's own.
#![warn(clippy::pedantic, clippy::nursery)]
use std::io::Write;
use std::path::Path;

const USAGE: &str = "usage: mud gen <file.proto>... [--lang rust|python|c] [-o dir] | mud provision <dir> [n] | mud provision <dir> --key";
fn usage() -> ! { eprintln!("{USAGE}"); std::process::exit(2) }

fn main() {
    let (mut lang, mut out, mut files) = ("rust".to_string(), None, Vec::new());
    let mut it = std::env::args().skip(1).peekable();
    if it.peek().is_some_and(|a| a == "provision") {
        let a: Vec<String> = it.skip(1).collect();
        let done = match a.as_slice() {
            [dir, k] if k == "--key" => next_key(Path::new(dir)),
            _ => {
                let n = a.get(1).map_or(Some(1), |n| n.parse().ok().filter(|&n| n > 0));
                let (Some(dir), Some(n), ..=2) = (a.first(), n, a.len()) else { usage() };
                provision(Path::new(dir), n)
            }
        };
        if let Err(e) = done { eprintln!("mud provision: {e}"); std::process::exit(1) }
        return;
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "gen" => {}
            "--lang" => lang = it.next().unwrap_or_default(),
            "-o" => out = it.next(),
            _ => files.push(a),
        }
    }
    if files.is_empty() || !["rust", "python", "c"].contains(&lang.as_str()) { usage() }
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_else(|e| { eprintln!("{f}: {e}"); std::process::exit(2) });
        let schema = mudrpc_gen::parse::parse(&text).unwrap_or_else(|(line, msg)| { eprintln!("{f}:{line}: {msg}"); std::process::exit(1) });
        let (code, ext) = match lang.as_str() {
            "rust" => (mudrpc_gen::rust::emit(&schema), "rs"),
            "python" => (mudrpc_gen::python::emit(&schema), "py"),
            _ => (mudrpc_gen::c::emit(&schema).unwrap_or_else(|e| { eprintln!("{f}: {e}"); std::process::exit(1) }), "h"),
        };
        match &out {
            Some(dir) => { std::fs::create_dir_all(dir).unwrap_or_else(|e| { eprintln!("{dir}: {e}"); std::process::exit(2) }); std::fs::write(std::path::Path::new(dir).join(std::path::Path::new(&f).with_extension(ext).file_name().unwrap()), code).unwrap() }
            None => print!("{code}"),
        }
    }
}

/// A new fleet writes `fleet.key` and numbers from 1; an old one is never rewritten and numbers after its roster's highest.
/// Every target is checked before anything is written, and `create_new` refuses one that appeared since.
fn provision(dir: &Path, n: u32) -> Result<(), String> {
    let (key, roster) = (dir.join("fleet.key"), dir.join("roster"));
    let at = |p: &Path| { let p = p.display().to_string(); move |e: std::io::Error| format!("{p}: {e}") };
    let text = match std::fs::read_to_string(&roster) { Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(), r => r.map_err(at(&roster))? };
    let mut last = 0u32;
    for l in text.lines() { last = last.max(l.split(' ').next().and_then(|s| s.parse().ok()).ok_or_else(|| format!("{}: not `<sender> <public key>`: {l}", roster.display()))?); }
    let first = last.checked_add(1).filter(|f| f.checked_add(n - 1).is_some()).ok_or("no sender numbers left")?;
    let device = |s: u32| dir.join(format!("device-{s}"));
    let (senders, fresh) = (first..=first + (n - 1), !key.exists());
    let mut targets: Vec<_> = senders.clone().map(device).collect();
    if fresh { targets.extend([key.clone(), roster.clone()]); } // a new fleet's roster is new too
    if let Some(t) = targets.iter().find(|t| t.exists()) { return Err(format!("{} exists; nothing written", t.display())); }
    std::fs::create_dir_all(dir).map_err(at(dir))?;
    if fresh { secret(&key, &format!("key {}\n", hex(&random()?)))?; }
    for s in senders {
        let seed = random()?;
        let pk = mudrpc_auth::ed25519::public_key(&seed);
        if mudrpc_auth::ed25519::small_order(&pk) { return Err(format!("device-{s}: a public key every roster refuses")); }
        secret(&device(s), &format!("sender {s}\nepoch 1\nseed {}\n", hex(&seed)))?; // the device file first: its create_new is the number's lock
        let line = format!("{s} {}\n", hex(&pk));
        std::fs::OpenOptions::new().append(true).create(true).open(&roster).and_then(|mut f| f.write_all(line.as_bytes())).map_err(at(&roster))?;
        print!("{line}");
    }
    Ok(())
}

/// The key a fleet rotates to, staged on every unit by hand; `fleet.key` is never touched and a next key never replaced.
fn next_key(dir: &Path) -> Result<(), String> {
    if !dir.join("fleet.key").exists() { return Err(format!("{}: no fleet.key to rotate from", dir.display())); }
    secret(&dir.join("fleet.key.next"), &format!("key {}\n", hex(&random()?)))
}

/// A file only its owner reads, never one that exists.
fn secret(path: &Path, text: &str) -> Result<(), String> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)] std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path).and_then(|mut f| f.write_all(text.as_bytes())).map_err(|e| format!("{}: {e}", path.display()))
}

fn hex(b: &[u8; 32]) -> String { b.iter().flat_map(|x| [x >> 4, x & 15]).filter_map(|n| char::from_digit(n.into(), 16)).collect() }

/// 32 bytes from the operating system, all of them or an error; 32 equal bytes are refused, as `mud_new` refuses them.
fn random() -> Result<[u8; 32], String> {
    let mut b = [0u8; 32];
    #[cfg(unix)] std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b)).map_err(|e| format!("/dev/urandom: {e}"))?;
    #[cfg(windows)] {
        #[link(name = "advapi32")] extern "system" { fn SystemFunction036(buf: *mut u8, len: u32) -> u8; } // RtlGenRandom
        if unsafe { SystemFunction036(b.as_mut_ptr(), 32) } == 0 { return Err("RtlGenRandom failed".into()); }
    }
    #[cfg(not(any(unix, windows)))] return Err("no operating-system random source on this platform".into());
    if b.iter().all(|&x| x == b[0]) { return Err("the random source gave 32 equal bytes".into()); }
    Ok(b)
}
