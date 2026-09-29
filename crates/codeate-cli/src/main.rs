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

    let result = match cli.command {
        Command::Analyze(args) => run_analyze(args),
        Command::Query(args) => run_query(args),
    };

    if let Err(message) = result {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn run_analyze(args: AnalyzeArgs) -> Result<(), String> {
    let out_path = args
        .out
        .unwrap_or_else(|| args.path.join(".codeate").join("graph.json"));

    let repository = codeate_core::ingest(&args.path).map_err(|err| err.to_string())?;
    let registry = codeate_core::default_registry();
    let repository =
        codeate_core::detect_languages(&repository, &registry).map_err(|err| err.to_string())?;
    let analysis = codeate_core::analyze_repository(&repository).map_err(|err| err.to_string())?;
    let stored = codeate_core::StoredGraph::from_analysis(&analysis);

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    codeate_core::store::save(&out_path, &stored).map_err(|err| err.to_string())?;

    println!(
        "analyzed {} files -> {} syntax entities, {} symbols, {} edges",
        repository.files.len(),
        analysis.syntax_entities.len(),
        analysis.symbols.len(),
        analysis.edges.len(),
    );
    println!("graph written to {}", out_path.display());

    Ok(())
}

fn load_store(path: &std::path::Path) -> Result<codeate_core::StoredGraph, String> {
    if !path.exists() {
        return Err(format!(
            "no graph found at {} -- run `codeate analyze` first",
            path.display()
        ));
    }
    codeate_core::store::load(path).map_err(|err| err.to_string())
}

fn run_query(args: QueryArgs) -> Result<(), String> {
    let stored = load_store(&args.store)?;
    let (graph, _integrity) = stored.into_graph();
    let _engine = codeate_core::QueryEngine::new(&graph);
    let _json = args.json;

    match args.kind {
        QueryKind::Symbol { name } => Err(format!("query symbol {name}: not yet implemented")),
        QueryKind::Definition { id } => {
            Err(format!("query definition --id {id}: not yet implemented"))
        }
        QueryKind::Callers { id } => Err(format!("query callers --id {id}: not yet implemented")),
        QueryKind::Callees { id } => Err(format!("query callees --id {id}: not yet implemented")),
        QueryKind::Dependencies { id } => {
            Err(format!("query dependencies --id {id}: not yet implemented"))
        }
        QueryKind::Dependents { id } => {
            Err(format!("query dependents --id {id}: not yet implemented"))
        }
        QueryKind::Implementers { id } => {
            Err(format!("query implementers --id {id}: not yet implemented"))
        }
        QueryKind::References { id } => {
            Err(format!("query references --id {id}: not yet implemented"))
        }
        QueryKind::Impact { id } => Err(format!("query impact --id {id}: not yet implemented")),
        QueryKind::EntryPoints => Err("query entry-points: not yet implemented".to_string()),
        QueryKind::ModuleTree => Err("query module-tree: not yet implemented".to_string()),
    }
}
