//! The peer, the wire and the cipher: `no_std`, no allocation, no unsafe, no crates; time and randomness are inputs.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::many_single_char_names, // the letters are the RFC's; a reader checks the code against the RFC with them
    clippy::missing_errors_doc, // each function's doc says what its errors mean in prose
    clippy::cast_possible_truncation, // a `try_from` would add a panic path the tripwire forbids; every narrowing cast here is of a value masked or bounded on its line
)]
pub mod codec;
pub mod envelope;
pub mod rpc;
pub mod seal;
