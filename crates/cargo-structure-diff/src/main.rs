//! The `csd` binary: parse argv and delegate to the shared pipeline.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(cargo_structure_diff::run(&args));
}
