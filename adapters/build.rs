// The feature `internet` takes two keys: the feature, which any crate in the build can set, and the builder's own
// environment, `MUDRPC_INTERNET=listen-on-public-addresses`, which none can. One without the other fails the build, and
// the message does not name the value: whoever builds it public reads why first.
fn main() {
    println!("cargo:rerun-if-env-changed=MUDRPC_INTERNET");
    if std::env::var_os("CARGO_FEATURE_INTERNET").is_none() { return; }
    if std::env::var("MUDRPC_INTERNET").as_deref() != Ok("listen-on-public-addresses") {
        eprintln!("mudrpc-adapters: the feature `internet` is on, perhaps set by another crate in this build, but the builder has not set MUDRPC_INTERNET as adapters/SKILL.md says; no public listener is built without both");
        std::process::exit(1);
    }
    println!("cargo:warning=mudrpc-adapters is built with `internet`: its UDP transport may listen on a public address");
}
