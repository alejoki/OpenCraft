//! Runnable entry point for the device-free external reuse proof.
//!
//! Prints the aggregated [`engine_reuse_proof::report::ProofReport`] and exits 0
//! on its own. The bin creates no GPU device or surface, spawns no process and
//! touches no file, so it runs the same on a headless CI runner as on a desktop.

fn main() {
    print!("{}", engine_reuse_proof::run());
}
