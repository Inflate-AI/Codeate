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
    /// Always run the full pipeline, even if a Code Graph already exists at
    /// the target path. By default, an existing graph is patched
    /// incrementally (Phase 13) instead of rebuilt from scratch.
    #[arg(long)]
    full: bool,
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
    Definition {
        #[arg(long)]
        id: String,
    },
    /// Symbols that call this symbol.
    Callers {
        #[arg(long)]
        id: String,
    },
    /// Symbols this symbol calls.
    Callees {
        #[arg(long)]
        id: String,
    },
    /// Everything this symbol/module transitively depends on.
    Dependencies {
        #[arg(long)]
        id: String,
    },
    /// Everything that transitively depends on this symbol/module.
    Dependents {
        #[arg(long)]
        id: String,
    },
    /// Symbols that implement this interface/trait.
    Implementers {
        #[arg(long)]
        id: String,
    },
    /// Symbols that reference this symbol.
    References {
        #[arg(long)]
        id: String,
    },
    /// Everything transitively affected by a change to this symbol.
    Impact {
        #[arg(long)]
        id: String,
    },
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

    let existing = if args.full {
        None
    } else if out_path.exists() {
        Some(codeate_core::store::load(&out_path).map_err(|err| err.to_string())?)
    } else {
        None
    };

    let (stored, mode) = match existing {
        Some(previous) => {
            let (stored, report) = previous
                .update_incremental(&repository)
                .map_err(|err| err.to_string())?;
            if !report.is_clean() {
                return Err(format!(
                    "incremental analysis produced an inconsistent graph: {report:?}"
                ));
            }
            (stored, "incrementally")
        }
        None => {
            let analysis =
                codeate_core::analyze_repository(&repository).map_err(|err| err.to_string())?;
            (
                codeate_core::StoredGraph::from_analysis(&analysis, &repository.files),
                "fully",
            )
        }
    };

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    codeate_core::store::save(&out_path, &stored).map_err(|err| err.to_string())?;

    println!(
        "analyzed {mode}: {} files -> {} syntax entities, {} symbols, {} edges",
        repository.files.len(),
        stored.syntax_entities.len(),
        stored.symbols.len(),
        stored.edges.len(),
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
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("value is serializable")
    );
}

fn node_json(node: &codeate_core::Node) -> serde_json::Value {
    match node {
        codeate_core::Node::Symbol(symbol) => symbol_json(symbol),
        codeate_core::Node::Syntax(entity) => syntax_json(entity),
    }
}

fn print_id_list(
    graph: &codeate_core::Graph,
    ids: &[codeate_core::EntityId],
    empty_message: &str,
    json: bool,
) {
    if json {
        let value: Vec<serde_json::Value> = ids
            .iter()
            .filter_map(|id| graph.node(id))
            .map(node_json)
            .collect();
        print_json(&serde_json::Value::Array(value));
    } else if ids.is_empty() {
        println!("{empty_message}");
    } else {
        for id in ids {
            match graph.node(id) {
                Some(codeate_core::Node::Symbol(s)) => {
                    println!("{}  {}  {}", s.id.as_str(), s.kind.as_str(), s.name)
                }
                Some(codeate_core::Node::Syntax(s)) => {
                    println!("{}  {}  {}", s.id.as_str(), s.kind.as_str(), s.name)
                }
                None => println!("{}  (unknown node)", id.as_str()),
            }
        }
    }
}

fn print_symbol_list(matches: &[&codeate_core::SymbolEntity], empty_message: &str, json: bool) {
    if json {
        let value: Vec<serde_json::Value> = matches.iter().copied().map(symbol_json).collect();
        print_json(&serde_json::Value::Array(value));
    } else if matches.is_empty() {
        println!("{empty_message}");
    } else {
        for symbol in matches {
            println!(
                "{}  {}  {}",
                symbol.id.as_str(),
                symbol.kind.as_str(),
                symbol.name
            );
        }
    }
}

fn require_symbol(
    engine: &codeate_core::QueryEngine,
    id: &codeate_core::EntityId,
) -> Result<(), String> {
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

fn print_syntax_list(matches: &[&codeate_core::SyntaxEntity], empty_message: &str, json: bool) {
    if json {
        let value: Vec<serde_json::Value> = matches.iter().copied().map(syntax_json).collect();
        print_json(&serde_json::Value::Array(value));
    } else if matches.is_empty() {
        println!("{empty_message}");
    } else {
        for entity in matches {
            println!(
                "{}  {}  {}",
                entity.id.as_str(),
                entity.kind.as_str(),
                entity.name
            );
        }
    }
}

fn module_tree_json(tree: &codeate_core::ModuleTree) -> serde_json::Value {
    serde_json::json!({
        "module": syntax_json(&tree.module),
        "children": tree.children.iter().map(module_tree_json).collect::<Vec<_>>(),
    })
}

fn print_module_tree(tree: &codeate_core::ModuleTree, depth: usize) {
    println!(
        "{}{}  {}",
        "  ".repeat(depth),
        tree.module.id.as_str(),
        tree.module.name
    );
    for child in &tree.children {
        print_module_tree(child, depth + 1);
    }
}

fn cmd_definition(
    engine: &codeate_core::QueryEngine,
    id: &codeate_core::EntityId,
    json: bool,
) -> Result<(), String> {
    require_symbol(engine, id)?;
    let definition = engine.definition_of(id);
    if json {
        let value = definition
            .map(syntax_json)
            .unwrap_or(serde_json::Value::Null);
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
            let id = parse_id(&id)?;
            require_symbol(&engine, &id)?;
            print_id_list(
                &graph,
                &engine.dependencies_of(&id),
                "no dependencies",
                json,
            );
            Ok(())
        }
        QueryKind::Dependents { id } => {
            let id = parse_id(&id)?;
            require_symbol(&engine, &id)?;
            print_id_list(&graph, &engine.dependents_of(&id), "no dependents", json);
            Ok(())
        }
        QueryKind::Implementers { id } => {
            let id = parse_id(&id)?;
            require_symbol(&engine, &id)?;
            print_symbol_list(&engine.implementers_of(&id), "no implementers", json);
            Ok(())
        }
        QueryKind::References { id } => {
            let id = parse_id(&id)?;
            require_symbol(&engine, &id)?;
            print_symbol_list(&engine.references_to(&id), "no references", json);
            Ok(())
        }
        QueryKind::Impact { id } => {
            let id = parse_id(&id)?;
            require_symbol(&engine, &id)?;
            print_id_list(&graph, &engine.impact_of(&id), "no impact", json);
            Ok(())
        }
        QueryKind::EntryPoints => {
            print_syntax_list(&engine.entry_points(), "no entry points", json);
            Ok(())
        }
        QueryKind::ModuleTree => {
            let roots = engine.module_tree();
            if json {
                let value: Vec<serde_json::Value> = roots.iter().map(module_tree_json).collect();
                print_json(&serde_json::Value::Array(value));
            } else if roots.is_empty() {
                println!("no modules");
            } else {
                for root in &roots {
                    print_module_tree(root, 0);
                }
            }
            Ok(())
        }
    }
}
