//! fork: the reading's transcript, nothing of the copy's own used: built against the original's core and
//! against this copy's (see fork_engine/harness.rs and FORK.md).
//!
//! `cargo run --release -p vi-translator-core --example fork_engine_equivalence -- play out.jsonl`, then
//! `… -- compare original.jsonl fork.jsonl`.

#[path = "fork_engine/harness.rs"]
mod harness;

fn main() {
    harness::main(harness::NO_HOOKS);
}
