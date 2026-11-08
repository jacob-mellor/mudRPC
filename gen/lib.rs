//! `mud gen`: parse `.proto` files, emit thin stubs over the one codec. No protoc anywhere.
#![warn(missing_docs, clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::missing_errors_doc, // each function's doc says what its errors mean in prose
    clippy::missing_panics_doc, // `build` runs in a build script, where a panic is the error report
)]
pub mod c;
pub mod parse;
pub mod python;
pub mod rust;

/// For a product's `build.rs`: every `.proto` in `dir` becomes `$OUT_DIR/<stem>.rs`. Include with
/// `include!(concat!(env!("OUT_DIR"), "/<stem>.rs"));`. A parse error fails the build naming file and line.
pub fn build(dir: impl AsRef<std::path::Path>) {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("build.rs only"));
    for entry in std::fs::read_dir(&dir).expect("proto dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("proto") { continue; }
        println!("cargo:rerun-if-changed={}", path.display());
        let text = std::fs::read_to_string(&path).expect("read proto");
        let schema = parse::parse(&text).unwrap_or_else(|(line, msg)| panic!("{}:{line}: {msg}", path.display()));
        std::fs::write(out.join(path.with_extension("rs").file_name().unwrap()), rust::emit(&schema)).expect("write stub");
    }
}
