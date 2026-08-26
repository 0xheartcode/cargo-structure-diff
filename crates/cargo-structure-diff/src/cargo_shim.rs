//! The `cargo-structure-diff` binary: the cargo subcommand surface.
//!
//! Cargo invokes it as `cargo-structure-diff structure-diff <args>`, injecting the subcommand name
//! as the first argument. Drop that if present, then delegate to the same `run` the `csd` binary
//! uses.

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("structure-diff") {
        args.remove(0);
    }
    std::process::exit(cargo_structure_diff::run(&args));
}
