use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::fs;

use crate::error::Result;
use crate::graph::Graph;
use crate::incremental::{affected_files, detect_changes};
use crate::language::LanguageId;
use crate::model::{Edge, EdgeKind, EntityId, SymbolEntity, SyntaxEntity};
use crate::repository::Repository;
use crate::resolve::resolve_depends_on;
use crate::symbols::{build_symbol_table, defined_in_edges};

/// The full canonical entity/edge stream for a repository: syntax entities,
/// resolved symbols, and every edge kind Phases 3-8 produce, resolved with
/// repository-wide (cross-file) context. Phase 9's `Graph` assembles this
/// into a queryable structure.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AnalysisResult {
    pub syntax_entities: Vec<SyntaxEntity>,
    pub symbols: Vec<SymbolEntity>,
    pub edges: Vec<Edge>,
    /// Which file each syntax entity and symbol was produced from, keyed by
    /// `EntityId`. Phase 13's incremental analysis uses this to find what to
    /// invalidate when a file changes.
    pub owner_file: HashMap<EntityId, String>,
}

/// Data reused from a previous analysis, carried into `run_pipeline` for a
/// file set that wasn't re-parsed. Empty (`CarryIn::default()`) for a full
/// `analyze_repository` run.
#[derive(Default)]
struct CarryIn {
    /// Syntax entities for files that were not re-parsed.
    entities: Vec<SyntaxEntity>,
    /// `CONTAINS`/`DECLARES` edges among `entities` (structural, produced
    /// directly by extraction -- safe to reuse verbatim).
    structural_edges: Vec<Edge>,
    /// Symbols resolved for the carried-in files in a previous run (base
    /// declared symbols are recomputed fresh from `entities` regardless, so
    /// this only needs to include the "extra" ones -- but re-including a
    /// base symbol too is harmless: they carry the same deterministic id).
    resolution_symbols: Vec<SymbolEntity>,
    /// Every non-structural, non-`DEFINED_IN`, non-`DEPENDS_ON` edge
    /// resolved for the carried-in files in a previous run. `DEFINED_IN` and
    /// `DEPENDS_ON` are always recomputed fresh (cheap, and must reflect the
    /// current full entity/edge set).
    resolution_edges: Vec<Edge>,
    owner_file: HashMap<EntityId, String>,
}

/// Run the full Phase 4-8 pipeline. When `files_filter` is `Some`, only
/// those paths are parsed and re-resolved; every other file's data comes
/// from `carry_in` instead. When `files_filter` is `None`, every file with a
/// registered adapter is parsed and `carry_in` is expected to be empty (the
/// full-analysis case). A file whose language has no registered adapter is
/// skipped, the same way Phase 2's `LanguageId::Unknown` already is.
fn run_pipeline(
    repository: &Repository,
    files_filter: Option<&HashSet<String>>,
    carry_in: CarryIn,
) -> Result<AnalysisResult> {
    let adapters = crate::adapters::default_adapters();

    // Parse every file that needs (re-)processing up front and keep the
    // ASTs alive: `resolve_file` borrows directly into them, not just the
    // canonical entities `extract` derives.
    struct PerFile {
        path: String,
        language: LanguageId,
        ast: Box<dyn Any>,
    }

    let mut per_file: Vec<PerFile> = Vec::new();
    for file in &repository.files {
        if !adapters.contains_key(&file.language) {
            continue;
        }
        if let Some(filter) = files_filter {
            if !filter.contains(&file.path) {
                continue;
            }
        }
        let source = fs::read_to_string(repository.root.join(&file.path))?;
        let adapter = &adapters[&file.language];
        per_file.push(PerFile {
            path: file.path.clone(),
            language: file.language.clone(),
            ast: adapter.parse_dyn(&source)?,
        });
    }

    let mut all_entities: Vec<SyntaxEntity> = carry_in.entities;
    let mut all_edges: Vec<Edge> = carry_in.structural_edges;
    let mut owner_file: HashMap<EntityId, String> = carry_in.owner_file;
    // The file's own root module entity, keyed by path -- `extract`'s
    // contract (see `LanguageAdapter::extract`) guarantees it's always the
    // first entity a file's own extraction returns.
    let mut file_module_for: HashMap<String, SyntaxEntity> = HashMap::new();

    for pf in &per_file {
        let adapter = &adapters[&pf.language];
        let (entities, edges) = adapter.extract_dyn(pf.ast.as_ref(), &pf.path);
        for entity in &entities {
            owner_file.insert(entity.id.clone(), pf.path.clone());
        }
        if let Some(first) = entities.first() {
            file_module_for.insert(pf.path.clone(), first.clone());
        }
        all_entities.extend(entities);
        all_edges.extend(edges);
    }

    let symbol_table = build_symbol_table(&all_entities);
    let mut all_symbols: Vec<SymbolEntity> = symbol_table.symbols().cloned().collect();
    for entry in symbol_table.entries() {
        if let Some(file) = owner_file.get(&entry.syntax_id) {
            owner_file.insert(entry.symbol.id.clone(), file.clone());
        }
    }
    all_edges.extend(defined_in_edges(&symbol_table, &all_edges));

    // Module symbols for every file with entities in this run -- both
    // re-parsed files (via `per_file`) and carried-in ones (via
    // `all_entities`, which already includes their Module entity).
    let mut module_symbol_for_file: HashMap<String, EntityId> = HashMap::new();
    for entity in &all_entities {
        if entity.kind != crate::model::SyntaxKind::Module {
            continue;
        }
        let Some(path) = owner_file.get(&entity.id) else {
            continue;
        };
        if let Some(symbol) = symbol_table.find_by_syntax_id(&entity.id) {
            module_symbol_for_file.insert(path.clone(), symbol.id.clone());
        }
    }

    let mut new_symbols: Vec<SymbolEntity> = Vec::new();
    let mut new_edges: Vec<Edge> = Vec::new();

    for pf in &per_file {
        let Some(file_module) = file_module_for.get(&pf.path) else {
            continue;
        };
        let Some(module_symbol) = symbol_table.find_by_syntax_id(&file_module.id).cloned() else {
            continue;
        };
        let adapter = &adapters[&pf.language];
        let (symbols, edges) = adapter.resolve_file_dyn(
            pf.ast.as_ref(),
            &pf.path,
            &all_entities,
            &symbol_table,
            &all_edges,
            &module_symbol,
            file_module,
        );
        for symbol in &symbols {
            owner_file.insert(symbol.id.clone(), pf.path.clone());
        }
        new_symbols.extend(symbols);
        new_edges.extend(edges);
    }

    all_symbols.extend(new_symbols);
    all_edges.extend(new_edges);
    all_symbols.extend(carry_in.resolution_symbols);
    all_edges.extend(carry_in.resolution_edges);

    // `carry_in.resolution_symbols` and the fresh `symbol_table` above both
    // carry a base symbol for every kept entity (by design -- see `CarryIn`'s
    // own doc comment), so the same `EntityId` legitimately appears in both.
    // Same id always means same content here (Phase 3's determinism
    // guarantee), so the first occurrence is kept and every later one
    // dropped, rather than letting them accumulate into the final result --
    // caught via Phase 18.1's real-world benchmarking, where this
    // dropped-nowhere concatenation inflated a large repo's raw symbol count
    // by ~44% after a single-file edit (compounding further on each
    // subsequent incremental run, since the next run's carry-in is this
    // run's already-duplicated output).
    let mut seen_symbol_ids: HashSet<EntityId> = HashSet::with_capacity(all_symbols.len());
    all_symbols.retain(|symbol| seen_symbol_ids.insert(symbol.id.clone()));

    let depends_on = resolve_depends_on(&all_edges, &owner_file, &module_symbol_for_file);
    all_edges.extend(depends_on);

    Ok(AnalysisResult {
        syntax_entities: all_entities,
        symbols: all_symbols,
        edges: all_edges,
        owner_file,
    })
}

/// Run the full Phase 4-8 pipeline over every file in `repository` whose
/// language has a registered adapter (`adapters::default_adapters`): parse,
/// extract, merge symbol tables repository-wide, then resolve each file
/// (references/calls/instantiates/structural relationships, resolved
/// against the full repository's entities and merged symbol table) through
/// its own adapter's `LanguageAdapter::resolve_file`. A file whose language
/// has no registered adapter is skipped, not an error.
pub fn analyze_repository(repository: &Repository) -> Result<AnalysisResult> {
    run_pipeline(repository, None, CarryIn::default())
}

/// Phase 13: re-analyze `repository` incrementally against `previous_files`
/// (the file set as of `previous`'s analysis) and `previous` (its result).
/// Only files that changed, were added, or depend on a file that did are
/// re-parsed and re-resolved; everything else is carried over unchanged. If
/// nothing changed, `previous` is returned as-is.
///
/// The result is identical to what a full `analyze_repository` would
/// produce for the same `repository` -- this is an optimization, not a
/// different analysis.
pub fn analyze_incremental(
    repository: &Repository,
    previous_files: &[crate::repository::File],
    previous: &AnalysisResult,
) -> Result<AnalysisResult> {
    let changes = detect_changes(previous_files, &repository.files);
    if changes.is_empty() {
        return Ok(previous.clone());
    }

    let (previous_graph, _) = Graph::build(
        previous.syntax_entities.clone(),
        previous.symbols.clone(),
        previous.edges.clone(),
    );
    let affected = affected_files(&changes, &previous_graph, &previous.owner_file);

    let repo_paths: HashSet<String> = repository.files.iter().map(|f| f.path.clone()).collect();
    // Only re-parse files that both need re-analysis and still exist.
    let files_filter: HashSet<String> = affected
        .into_iter()
        .filter(|path| repo_paths.contains(path))
        .collect();
    let kept_paths: HashSet<String> = repo_paths.difference(&files_filter).cloned().collect();

    let kept_owner_file: HashMap<EntityId, String> = previous
        .owner_file
        .iter()
        .filter(|(_, path)| kept_paths.contains(path.as_str()))
        .map(|(id, path)| (id.clone(), path.clone()))
        .collect();
    let kept_ids: HashSet<&EntityId> = kept_owner_file.keys().collect();

    let kept_entities: Vec<SyntaxEntity> = previous
        .syntax_entities
        .iter()
        .filter(|entity| kept_ids.contains(&entity.id))
        .cloned()
        .collect();
    let kept_structural_edges: Vec<Edge> = previous
        .edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::Contains | EdgeKind::Declares))
        .filter(|edge| kept_ids.contains(&edge.from) && kept_ids.contains(&edge.to))
        .cloned()
        .collect();
    let kept_resolution_symbols: Vec<SymbolEntity> = previous
        .symbols
        .iter()
        .filter(|symbol| kept_ids.contains(&symbol.id))
        .cloned()
        .collect();
    let kept_resolution_edges: Vec<Edge> = previous
        .edges
        .iter()
        .filter(|edge| {
            !matches!(
                edge.kind,
                EdgeKind::Contains | EdgeKind::Declares | EdgeKind::DefinedIn | EdgeKind::DependsOn
            )
        })
        .filter(|edge| kept_ids.contains(&edge.from) && kept_ids.contains(&edge.to))
        .cloned()
        .collect();

    let carry_in = CarryIn {
        entities: kept_entities,
        structural_edges: kept_structural_edges,
        resolution_symbols: kept_resolution_symbols,
        resolution_edges: kept_resolution_edges,
        owner_file: kept_owner_file,
    };

    run_pipeline(repository, Some(&files_filter), carry_in)
}

/// Merge Phase 14's repository-level state (manifest parsing + source
/// discovery) into an already-computed Phase 4-8 `analysis`, given the same
/// `repository` it was produced from. `analysis` may come from either
/// `analyze_repository` or `analyze_incremental` -- Phase 14's own state is
/// always recomputed fresh over the full current repository rather than
/// incrementally (manifest parsing is cheap; only worth optimizing later if
/// it shows up as a real cost).
pub fn merge_repository_state(
    mut analysis: AnalysisResult,
    repository: &Repository,
) -> Result<AnalysisResult> {
    let manifest = crate::manifest::parse_manifests(repository)?;
    let discovery = crate::discovery::discover(repository, &analysis, &manifest)?;

    analysis.syntax_entities.extend(manifest.entities);
    analysis.symbols.extend(manifest.symbols);
    analysis.edges.extend(manifest.edges);
    analysis.owner_file.extend(manifest.owner_file);

    analysis.syntax_entities.extend(discovery.entities);
    analysis.edges.extend(discovery.edges);
    analysis.owner_file.extend(discovery.owner_file);

    Ok(analysis)
}

/// `analyze_repository` followed by `merge_repository_state` -- the full
/// Phase 4-8 + Phase 14 pipeline in one call.
pub fn analyze_repository_state(repository: &Repository) -> Result<AnalysisResult> {
    merge_repository_state(analyze_repository(repository)?, repository)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EdgeKind, SyntaxKind};
    use std::fs;

    fn write_repo(files: &[(&str, &str)]) -> tempfile_repo::TempRepo {
        tempfile_repo::TempRepo::new(files)
    }

    // A tiny, dependency-free temp-directory helper: create a unique
    // directory under the OS temp dir, write the given (relative path,
    // content) pairs into it, and remove it on drop.
    mod tempfile_repo {
        use std::path::PathBuf;

        pub struct TempRepo {
            pub root: PathBuf,
        }

        impl TempRepo {
            pub fn new(files: &[(&str, &str)]) -> Self {
                let root = std::env::temp_dir().join(format!(
                    "codeate-pipeline-test-{}-{}",
                    std::process::id(),
                    super::unique_suffix()
                ));
                std::fs::create_dir_all(&root).expect("create temp repo root");
                for (path, content) in files {
                    let full = root.join(path);
                    if let Some(parent) = full.parent() {
                        std::fs::create_dir_all(parent).expect("create temp repo subdir");
                    }
                    std::fs::write(&full, content).expect("write temp repo file");
                }
                Self { root }
            }
        }

        impl Drop for TempRepo {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
    }

    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn repo_of(root: &std::path::Path, rust_files: &[&str]) -> Repository {
        let files = rust_files
            .iter()
            .map(|path| {
                let full = root.join(path);
                let size = fs::metadata(&full).unwrap().len();
                crate::File::new(*path, "irrelevant-hash", size)
                    .with_language(LanguageId::known("rust"))
            })
            .collect();
        Repository::new(root, files)
    }

    // Like `repo_of`, but with real content hashes (via `ingest`) so
    // `analyze_incremental`'s change detection has something meaningful to
    // compare against.
    fn hashed_repo(root: &std::path::Path) -> Repository {
        let repository = crate::ingest::ingest(root).expect("ingest succeeds");
        let registry = crate::detect::default_registry();
        crate::detect::detect_languages(&repository, &registry).expect("detect succeeds")
    }

    #[test]
    fn analyze_repository_state_merges_manifest_and_discovery_into_the_graph() {
        let repo = write_repo(&[
            (
                "Cargo.toml",
                "[package]\nname = \"widgets\"\n\n[dependencies]\nserde = \"1\"\n",
            ),
            ("src/main.rs", "fn main() {}\n\n#[test]\nfn it_works() {}"),
        ]);
        let repository = hashed_repo(&repo.root);

        let result = analyze_repository_state(&repository).expect("analysis succeeds");

        assert!(result
            .syntax_entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Package && e.name == "widgets"));
        assert!(result
            .syntax_entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Dependency && e.name == "serde"));
        assert!(result
            .syntax_entities
            .iter()
            .any(|e| e.kind == SyntaxKind::BuildTarget && e.name == "bin:widgets"));
        assert!(result
            .syntax_entities
            .iter()
            .any(|e| e.kind == SyntaxKind::EntryPoint));
        assert!(result
            .syntax_entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Test && e.name == "it_works"));

        let (graph, report) = Graph::build(
            result.syntax_entities.clone(),
            result.symbols.clone(),
            result.edges.clone(),
        );
        assert!(report.is_clean());
        assert!(graph.node_count() > 0);
    }

    #[test]
    fn analyzes_a_single_file_producing_syntax_entities_symbols_and_edges() {
        let repo = write_repo(&[("src/lib.rs", "struct Point { x: i32 }\nfn run() {}")]);
        let repository = repo_of(&repo.root, &["src/lib.rs"]);

        let result = analyze_repository(&repository).expect("analysis succeeds");

        assert!(result
            .syntax_entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Struct && e.name == "Point"));
        assert!(result.symbols.iter().any(|s| s.name == "Point"));
        assert!(result
            .syntax_entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Function && e.name == "run"));
    }

    #[test]
    fn resolves_a_cross_file_implements_edge_through_the_pipeline() {
        let repo = write_repo(&[
            ("src/shape.rs", "pub trait Shape { fn area(&self) -> f64; }"),
            (
                "src/circle.rs",
                "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 0.0 } }",
            ),
        ]);
        let repository = repo_of(&repo.root, &["src/shape.rs", "src/circle.rs"]);

        let result = analyze_repository(&repository).expect("analysis succeeds");

        let circle = result
            .syntax_entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Struct && e.name == "Circle")
            .expect("Circle entity");
        let shape = result
            .syntax_entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Interface && e.name == "Shape")
            .expect("Shape entity");
        let circle_symbol = result
            .symbols
            .iter()
            .find(|s| s.name == "Circle")
            .expect("Circle symbol");
        let shape_symbol = result
            .symbols
            .iter()
            .find(|s| s.name == "Shape")
            .expect("Shape symbol");
        let _ = (circle, shape);

        assert!(result.edges.contains(&Edge::new(
            circle_symbol.id.clone(),
            shape_symbol.id.clone(),
            EdgeKind::Implements
        )));

        // The cross-file `use` also resolves to a real IMPORTS edge, and the
        // two files depend on each other via that import.
        assert!(result
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Imports && e.to == shape_symbol.id));
        assert!(result.edges.iter().any(|e| e.kind == EdgeKind::DependsOn));
    }

    #[test]
    fn every_symbol_and_syntax_entity_is_attributed_to_its_owning_file() {
        let repo = write_repo(&[
            ("src/shape.rs", "pub trait Shape { fn area(&self) -> f64; }"),
            (
                "src/circle.rs",
                "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 0.0 } }",
            ),
        ]);
        let repository = repo_of(&repo.root, &["src/shape.rs", "src/circle.rs"]);

        let result = analyze_repository(&repository).expect("analysis succeeds");

        let circle_symbol = result
            .symbols
            .iter()
            .find(|s| s.name == "Circle")
            .expect("Circle symbol");
        let shape_symbol = result
            .symbols
            .iter()
            .find(|s| s.name == "Shape")
            .expect("Shape symbol");

        assert_eq!(
            result.owner_file.get(&circle_symbol.id).map(String::as_str),
            Some("src/circle.rs")
        );
        assert_eq!(
            result.owner_file.get(&shape_symbol.id).map(String::as_str),
            Some("src/shape.rs")
        );
        for entity in &result.syntax_entities {
            assert!(
                result.owner_file.contains_key(&entity.id),
                "every syntax entity should have a recorded owning file: {entity:?}"
            );
        }
    }

    #[test]
    fn incremental_analysis_with_no_changes_returns_the_previous_result_unchanged() {
        let repo = write_repo(&[("src/lib.rs", "struct Point { x: i32 }")]);
        let repository = hashed_repo(&repo.root);

        let full = analyze_repository(&repository).expect("full analysis succeeds");
        let incremental = analyze_incremental(&repository, &repository.files, &full)
            .expect("incremental analysis succeeds");

        assert_eq!(incremental, full);
    }

    #[test]
    fn incremental_reanalysis_of_a_modified_file_matches_a_full_rebuild() {
        let repo = write_repo(&[
            ("src/shape.rs", "pub trait Shape { fn area(&self) -> f64; }"),
            (
                "src/circle.rs",
                "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 0.0 } }",
            ),
        ]);
        let previous_repository = hashed_repo(&repo.root);
        let previous = analyze_repository(&previous_repository).expect("initial analysis");

        fs::write(
            repo.root.join("src/circle.rs"),
            "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 1.0 } }",
        )
        .expect("rewrite circle.rs");
        let updated_repository = hashed_repo(&repo.root);

        let incremental =
            analyze_incremental(&updated_repository, &previous_repository.files, &previous)
                .expect("incremental analysis succeeds");
        let full_rebuild = analyze_repository(&updated_repository).expect("full rebuild succeeds");

        let (incremental_graph, _) = Graph::build(
            incremental.syntax_entities.clone(),
            incremental.symbols.clone(),
            incremental.edges.clone(),
        );
        let (full_graph, _) = Graph::build(
            full_rebuild.syntax_entities.clone(),
            full_rebuild.symbols.clone(),
            full_rebuild.edges.clone(),
        );

        assert_eq!(
            incremental_graph.canonical_form(),
            full_graph.canonical_form()
        );
    }

    #[test]
    fn incremental_reanalysis_after_adding_a_file_matches_a_full_rebuild() {
        let repo = write_repo(&[("src/shape.rs", "pub trait Shape { fn area(&self) -> f64; }")]);
        let previous_repository = hashed_repo(&repo.root);
        let previous = analyze_repository(&previous_repository).expect("initial analysis");

        fs::write(
            repo.root.join("src/circle.rs"),
            "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 0.0 } }",
        )
        .expect("write circle.rs");
        let updated_repository = hashed_repo(&repo.root);

        let incremental =
            analyze_incremental(&updated_repository, &previous_repository.files, &previous)
                .expect("incremental analysis succeeds");
        let full_rebuild = analyze_repository(&updated_repository).expect("full rebuild succeeds");

        let (incremental_graph, _) = Graph::build(
            incremental.syntax_entities.clone(),
            incremental.symbols.clone(),
            incremental.edges.clone(),
        );
        let (full_graph, _) = Graph::build(
            full_rebuild.syntax_entities.clone(),
            full_rebuild.symbols.clone(),
            full_rebuild.edges.clone(),
        );

        assert_eq!(
            incremental_graph.canonical_form(),
            full_graph.canonical_form()
        );
    }

    #[test]
    fn incremental_reanalysis_of_a_modified_file_does_not_duplicate_carried_in_symbols() {
        // Regression test: `Graph::build`'s `HashMap<EntityId, Node>` silently
        // dedupes by id, so the tests above (which only compare
        // `canonical_form()`) can't catch a bug in the *raw* `AnalysisResult`
        // vectors themselves. This repo's unchanged `shape.rs` must not get
        // a second copy of its own base symbol merely because `circle.rs`
        // was edited -- found via real-world benchmarking (Phase 18.1) on a
        // large multi-crate repository, where editing any single file
        // inflated the raw symbol count by ~44%.
        let repo = write_repo(&[
            ("src/shape.rs", "pub trait Shape { fn area(&self) -> f64; }"),
            (
                "src/circle.rs",
                "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 0.0 } }",
            ),
        ]);
        let previous_repository = hashed_repo(&repo.root);
        let previous = analyze_repository(&previous_repository).expect("initial analysis");

        fs::write(
            repo.root.join("src/circle.rs"),
            "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 1.0 } }",
        )
        .expect("rewrite circle.rs");
        let updated_repository = hashed_repo(&repo.root);

        let incremental =
            analyze_incremental(&updated_repository, &previous_repository.files, &previous)
                .expect("incremental analysis succeeds");
        let full_rebuild = analyze_repository(&updated_repository).expect("full rebuild succeeds");

        let unique_count: usize = {
            let set: HashSet<&EntityId> = incremental.symbols.iter().map(|s| &s.id).collect();
            set.len()
        };
        assert_eq!(
            incremental.symbols.len(),
            unique_count,
            "incremental re-analysis produced duplicate symbol entries for the same EntityId"
        );
        assert_eq!(
            incremental.symbols.len(),
            full_rebuild.symbols.len(),
            "incremental re-analysis's raw symbol count should match a full rebuild's, not just its deduped Graph form"
        );
    }

    #[test]
    fn incremental_reanalysis_after_removing_a_file_matches_a_full_rebuild() {
        let repo = write_repo(&[
            ("src/shape.rs", "pub trait Shape { fn area(&self) -> f64; }"),
            (
                "src/circle.rs",
                "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 0.0 } }",
            ),
        ]);
        let previous_repository = hashed_repo(&repo.root);
        let previous = analyze_repository(&previous_repository).expect("initial analysis");

        std::fs::remove_file(repo.root.join("src/circle.rs")).expect("remove circle.rs");
        let updated_repository = hashed_repo(&repo.root);

        let incremental =
            analyze_incremental(&updated_repository, &previous_repository.files, &previous)
                .expect("incremental analysis succeeds");
        let full_rebuild = analyze_repository(&updated_repository).expect("full rebuild succeeds");

        let (incremental_graph, _) = Graph::build(
            incremental.syntax_entities.clone(),
            incremental.symbols.clone(),
            incremental.edges.clone(),
        );
        let (full_graph, _) = Graph::build(
            full_rebuild.syntax_entities.clone(),
            full_rebuild.symbols.clone(),
            full_rebuild.edges.clone(),
        );

        assert_eq!(
            incremental_graph.canonical_form(),
            full_graph.canonical_form()
        );
    }

    #[test]
    fn incremental_analysis_stays_correct_across_a_sequence_of_edits() {
        fn assert_matches_full_rebuild(repository: &Repository, incremental: &AnalysisResult) {
            let full_rebuild = analyze_repository(repository).expect("full rebuild succeeds");
            let (incremental_graph, _) = Graph::build(
                incremental.syntax_entities.clone(),
                incremental.symbols.clone(),
                incremental.edges.clone(),
            );
            let (full_graph, _) = Graph::build(
                full_rebuild.syntax_entities.clone(),
                full_rebuild.symbols.clone(),
                full_rebuild.edges.clone(),
            );
            assert_eq!(
                incremental_graph.canonical_form(),
                full_graph.canonical_form()
            );
        }

        let repo = write_repo(&[
            ("src/shape.rs", "pub trait Shape { fn area(&self) -> f64; }"),
            (
                "src/circle.rs",
                "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 0.0 } }",
            ),
        ]);

        // Step 0: initial full analysis.
        let mut previous_repository = hashed_repo(&repo.root);
        let mut previous = analyze_repository(&previous_repository).expect("initial analysis");

        // Step 1: modify a file that a dependent (circle.rs) imports from.
        fs::write(
            repo.root.join("src/shape.rs"),
            "pub trait Shape { fn area(&self) -> f64; fn perimeter(&self) -> f64 { 0.0 } }",
        )
        .expect("modify shape.rs");
        let repository = hashed_repo(&repo.root);
        let incremental = analyze_incremental(&repository, &previous_repository.files, &previous)
            .expect("incremental step 1 succeeds");
        assert_matches_full_rebuild(&repository, &incremental);
        previous_repository = repository;
        previous = incremental;

        // Step 2: add a new file that depends on both existing files.
        fs::write(
            repo.root.join("src/square.rs"),
            "use crate::shape::Shape;\nuse crate::circle::Circle;\npub struct Square;\nimpl Shape for Square { fn area(&self) -> f64 { 0.0 } }",
        )
        .expect("add square.rs");
        let repository = hashed_repo(&repo.root);
        let incremental = analyze_incremental(&repository, &previous_repository.files, &previous)
            .expect("incremental step 2 succeeds");
        assert_matches_full_rebuild(&repository, &incremental);
        previous_repository = repository;
        previous = incremental;

        // Step 3: remove a file that others depended on.
        std::fs::remove_file(repo.root.join("src/circle.rs")).expect("remove circle.rs");
        let repository = hashed_repo(&repo.root);
        let incremental = analyze_incremental(&repository, &previous_repository.files, &previous)
            .expect("incremental step 3 succeeds");
        assert_matches_full_rebuild(&repository, &incremental);
        previous_repository = repository;
        previous = incremental;

        // Step 4: "rename" square.rs -> pentagon.rs (remove + add).
        let contents = fs::read_to_string(repo.root.join("src/square.rs")).unwrap();
        std::fs::remove_file(repo.root.join("src/square.rs")).expect("remove square.rs");
        fs::write(repo.root.join("src/pentagon.rs"), &contents).expect("add pentagon.rs");
        let repository = hashed_repo(&repo.root);
        let incremental = analyze_incremental(&repository, &previous_repository.files, &previous)
            .expect("incremental step 4 succeeds");
        assert_matches_full_rebuild(&repository, &incremental);
    }

    #[test]
    fn a_reference_to_an_undeclared_name_gets_an_explicit_unresolved_symbol() {
        let repo = write_repo(&[("src/lib.rs", "use other_crate::Thing;\nfn run() {}")]);
        let repository = repo_of(&repo.root, &["src/lib.rs"]);

        let result = analyze_repository(&repository).expect("analysis succeeds");

        assert!(result
            .symbols
            .iter()
            .any(|s| s.name.starts_with("external-crate:")));
    }
}
