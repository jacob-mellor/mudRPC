//! Per-sender authentication behind `Sealer` (SPEC.md layer 1, authentication): SHA-512 and Ed25519 from the primary sources,
//! and the `Signed` sealer. The same rules as core: `no_std`, no allocation, no crates, no unsafe.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::many_single_char_names, // the letters are the RFC's and the paper's; a reader checks the code against them
    clippy::unreadable_literal, // the constants are compared by eye against the RFC and generated from their definitions; separators would make that harder
    clippy::missing_errors_doc, // each function's doc says what its errors mean in prose
    clippy::cast_possible_truncation, // a `try_from` would add a panic path the tripwire forbids; every narrowing cast here is of a value masked or bounded on its line
)]
pub mod ed25519;
pub mod sha512;
pub mod signed;
