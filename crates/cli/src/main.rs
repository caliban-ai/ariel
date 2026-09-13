//! `ariel` — the operator CLI for the Ariel chat bridge.
//!
//! At this stage it only reports its version.

use clap::Parser;

/// Ariel operator CLI.
#[derive(Debug, Parser)]
#[command(name = "ariel", version, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
