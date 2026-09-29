use clap::Parser;

#[derive(Parser)]
#[command(name = "codeate", version, about = "Deterministic code intelligence")]
struct Cli {}

fn main() {
    Cli::parse();
}
