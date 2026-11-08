//! The transports a peer runs over, the simulator, and the harness every scenario and every binding runs.
#![warn(missing_docs, clippy::pedantic, clippy::nursery, clippy::undocumented_unsafe_blocks)]
#![allow(
    clippy::missing_errors_doc, // each function's doc says what its errors mean in prose
    clippy::missing_panics_doc, // the harness and the simulator are test tools; an `unwrap` there is an assertion
    clippy::many_single_char_names, // the harness is a script over a scenario: `s` the scenario, `t` the time, `p` a peer, `e` an event
    clippy::enum_glob_use, // `Tick`'s three variants read as words in the harness
)]
pub mod harness;
pub mod lan;
pub mod sim;
pub mod transport;
pub mod udp;
