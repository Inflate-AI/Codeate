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

fn parse_id(raw: &str) -> Result<codeate_core::EntityId, String> {
    serde_json::from_value(serde_json::Value::String(raw.to_string()))
        .map_err(|err| format!("invalid id '{raw}': {err}"))
}

fn symbol_json(symbol: &codeate_core::SymbolEntity) -> serde_json::Value {
    serde_json::json!({
        "id": symbol.id,
        "kind": symbol.kind.as_str(),
        "name": symbol.name,
    })
}

fn syntax_json(entity: &codeate_core::SyntaxEntity) -> serde_json::Value {
    serde_json::json!({
        "id": entity.id,
        "kind": entity.kind.as_str(),
        "name": entity.name,
    })
}

fn print_json(value: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(value).expect("value is serializable"));
}

fn print_symbol_list(matches: &[&codeate_core::SymbolEntity], empty_message: &str, json: bool) {
    if json {
        let value: Vec<serde_json::Value> = matches.iter().copied().map(symbol_json).collect();
        print_json(&serde_json::Value::Array(value));
    } else if matches.is_empty() {
        println!("{empty_message}");
    } else {
        for symbol in matches {
            println!("{}  {}  {}", symbol.id.as_str(), symbol.kind.as_str(), symbol.name);
        }
    }
}

fn require_symbol(engine: &codeate_core::QueryEngine, id: &codeate_core::EntityId) -> Result<(), String> {
    if engine.symbol_by_id(id).is_none() {
        Err(format!("no symbol with id {}", id.as_str()))
    } else {
        Ok(())
    }
}

fn cmd_symbol(engine: &codeate_core::QueryEngine, name: &str, json: bool) {
    let matches = engine.symbols_named(name);
    print_symbol_list(&matches, &format!("no symbols named '{name}'"), json);
}

fn cmd_definition(
    engine: &codeate_core::QueryEngine,
    id: &codeate_core::EntityId,
    json: bool,
) -> Result<(), String> {
    require_symbol(engine, id)?;
    let definition = engine.definition_of(id);
    if json {
        let value = definition.map(syntax_json).unwrap_or(serde_json::Value::Null);
        print_json(&value);
    } else {
        match definition {
            Some(entity) => println!(
                "{}  {}  {}",
                entity.id.as_str(),
                entity.kind.as_str(),
                entity.name
            ),
            None => println!("no definition found"),
        }
    }
    Ok(())
}

fn run_query(args: QueryArgs) -> Result<(), String> {
    let stored = load_store(&args.store)?;
    let (graph, _integrity) = stored.into_graph();
    let engine = codeate_core::QueryEngine::new(&graph);
    let json = args.json;

    match args.kind {
        QueryKind::Symbol { name } => {
            cmd_symbol(&engine, &name, json);
            Ok(())
        }
        QueryKind::Definition { id } => cmd_definition(&engine, &parse_id(&id)?, json),
        QueryKind::Callers { id } => {
            let id = parse_id(&id)?;
            require_symbol(&engine, &id)?;
            print_symbol_list(&engine.callers_of(&id), "no callers", json);
            Ok(())
        }
        QueryKind::Callees { id } => {
            let id = parse_id(&id)?;
            require_symbol(&engine, &id)?;
            print_symbol_list(&engine.callees_of(&id), "no callees", json);
            Ok(())
        }
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
