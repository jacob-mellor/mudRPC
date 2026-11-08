# The foreign harnesses and the drop-ins' tests

`abi.rs` runs them under `cargo test -- --include-ignored`: a harness per language playing every scenario to the checked-in log, and a folder per language's gRPC with the test program the generated code runs in, its packages pinned.
