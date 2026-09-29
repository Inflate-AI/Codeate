use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "codeate", version, about = "Deterministic code intelligence")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the full ingest -> parse -> resolve -> graph pipeline over a
    /// repository and persist the resulting Code Graph.
    Analyze(AnalyzeArgs),
    /// Run a deterministic query against a previously analyzed Code Graph.
    Query(QueryArgs),
}

#[derive(Args)]
struct AnalyzeArgs {
    /// Repository root to analyze.
    path: PathBuf,
    /// Where to persist the Code Graph. Defaults to `<path>/.codeate/graph.json`.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args)]
struct QueryArgs {
    #[command(subcommand)]
    kind: QueryKind,
    /// Path to a Code Graph previously written by `codeate analyze`.
    #[arg(long, global = true, default_value = "./.codeate/graph.json")]
    store: PathBuf,
    /// Emit machine-readable JSON instead of human-readable text.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum QueryKind {
    /// Every symbol with this exact name, and the id to target it with.
    Symbol { name: String },
    /// The syntax entity that declares a symbol.
    Definition { #[arg(long)] id: String },
    /// Symbols that call this symbol.
    Callers { #[arg(long)] id: String },
    /// Symbols this symbol calls.
    Callees { #[arg(long)] id: String },
    /// Everything this symbol/module transitively depends on.
    Dependencies { #[arg(long)] id: String },
    /// Everything that transitively depends on this symbol/module.
    Dependents { #[arg(long)] id: String },
    /// Symbols that implement this interface/trait.
    Implementers { #[arg(long)] id: String },
    /// Symbols that reference this symbol.
    References { #[arg(long)] id: String },
    /// Everything transitively affected by a change to this symbol.
    Impact { #[arg(long)] id: String },
    /// Functions/methods named `main`.
    EntryPoints,
    /// The nested module hierarchy.
    ModuleTree,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Analyze(_args) => {
            eprintln!("codeate analyze: not yet implemented");
            std::process::exit(1);
        }
        Command::Query(_args) => {
            eprintln!("codeate query: not yet implemented");
            std::process::exit(1);
        }
    }
}
