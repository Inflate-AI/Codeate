use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "codeate-bench", version, about = "Phase 18.1 benchmark harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse and validate `benches/fixtures-manifest.json`.
    Fixtures(FixturesArgs),
    /// Render run-record JSON files into the Markdown results table.
    Metrics(MetricsArgs),
    /// Diff two `codeate analyze --full` runs over the same fixture and
    /// report whether they're identical.
    Determinism(DeterminismArgs),
}

#[derive(Args)]
struct FixturesArgs {
    #[command(subcommand)]
    action: FixturesAction,
}

#[derive(Subcommand)]
enum FixturesAction {
    /// Parse the manifest and report any validation errors.
    Verify {
        #[arg(long, default_value = "benches/fixtures-manifest.json")]
        manifest: PathBuf,
    },
}

#[derive(Args)]
struct MetricsArgs {
    #[command(subcommand)]
    action: MetricsAction,
}

#[derive(Subcommand)]
enum MetricsAction {
    /// Read every `*.json` run-record in `--input` and write the rendered
    /// results table to `--out`.
    Format {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Args)]
struct DeterminismArgs {
    #[command(subcommand)]
    action: DeterminismAction,
}

#[derive(Subcommand)]
enum DeterminismAction {
    /// Diff two stored Code Graphs and report any divergence.
    Diff { first: PathBuf, second: PathBuf },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Fixtures(args) => run_fixtures(args),
        Command::Metrics(args) => run_metrics(args),
        Command::Determinism(args) => run_determinism(args),
    }
}

fn run_fixtures(args: FixturesArgs) -> ExitCode {
    match args.action {
        FixturesAction::Verify { manifest } => {
            let json = match std::fs::read_to_string(&manifest) {
                Ok(json) => json,
                Err(err) => {
                    eprintln!("error: could not read {}: {err}", manifest.display());
                    return ExitCode::FAILURE;
                }
            };
            match codeate_bench::manifest::parse_manifest(&json) {
                Ok(parsed) => {
                    println!("{} fixture(s) valid", parsed.fixtures.len());
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn run_metrics(args: MetricsArgs) -> ExitCode {
    match args.action {
        MetricsAction::Format { input, out } => {
            let mut results = Vec::new();
            let entries = match std::fs::read_dir(&input) {
                Ok(entries) => entries,
                Err(err) => {
                    eprintln!("error: could not read {}: {err}", input.display());
                    return ExitCode::FAILURE;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let json = match std::fs::read_to_string(&path) {
                    Ok(json) => json,
                    Err(err) => {
                        eprintln!("error: could not read {}: {err}", path.display());
                        return ExitCode::FAILURE;
                    }
                };
                match serde_json::from_str::<codeate_bench::format::BenchResult>(&json) {
                    Ok(result) => results.push(result),
                    Err(err) => {
                        eprintln!("error: could not parse {}: {err}", path.display());
                        return ExitCode::FAILURE;
                    }
                }
            }
            let table = codeate_bench::format::format_results_table(&results);
            if let Err(err) = std::fs::write(&out, table) {
                eprintln!("error: could not write {}: {err}", out.display());
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
    }
}

fn run_determinism(args: DeterminismArgs) -> ExitCode {
    match args.action {
        DeterminismAction::Diff { first, second } => {
            let first_graph = match codeate_core::store::load(&first) {
                Ok(graph) => graph,
                Err(err) => {
                    eprintln!("error: could not load {}: {err}", first.display());
                    return ExitCode::FAILURE;
                }
            };
            let second_graph = match codeate_core::store::load(&second) {
                Ok(graph) => graph,
                Err(err) => {
                    eprintln!("error: could not load {}: {err}", second.display());
                    return ExitCode::FAILURE;
                }
            };
            let report = codeate_bench::determinism::diff(&first_graph, &second_graph);
            if report.is_clean() {
                println!("deterministic: {} and {} are identical", first.display(), second.display());
                ExitCode::SUCCESS
            } else {
                println!("NOT deterministic: {report:#?}");
                ExitCode::FAILURE
            }
        }
    }
}
