use std::collections::HashMap;
use std::fs;

use crate::adapter::LanguageAdapter;
use crate::adapters::rust::{
    self, resolve_body, resolve_exports, resolve_implements, resolve_imports, resolve_overrides,
    resolve_supertraits, ResolutionSites,
};
use crate::adapters::RustAdapter;
use crate::error::Result;
use crate::language::LanguageId;
use crate::model::{Edge, EntityId, SymbolEntity, SyntaxEntity};
use crate::repository::Repository;
use crate::resolve::resolve_depends_on;
use crate::symbols::{build_symbol_table, ScopeTree};

/// The full canonical entity/edge stream for a repository: syntax entities,
/// resolved symbols, and every edge kind Phases 3-8 produce, resolved with
/// repository-wide (cross-file) context. Phase 9's `Graph` assembles this
/// into a queryable structure.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AnalysisResult {
    pub syntax_entities: Vec<SyntaxEntity>,
    pub symbols: Vec<SymbolEntity>,
    pub edges: Vec<Edge>,
}

/// Run the full Phase 4-8 pipeline over every Rust file in `repository`:
/// parse, extract, merge symbol tables repository-wide, then resolve
/// references/calls/instantiates (per file, task 2-4's scope-based lookup)
/// and structural relationships (extends/implements/overrides/imports/
/// exports/depends_on, resolved against the full repository's entities and
/// merged symbol table). Non-Rust files are skipped: only `RustAdapter`
/// exists so far (Phase 15+ generalizes this dispatch to other languages).
pub fn analyze_repository(repository: &Repository) -> Result<AnalysisResult> {
    let adapter = RustAdapter;
    let rust = LanguageId::known("rust");

    // Parse every Rust file up front and keep the ASTs alive: later
    // resolution stages (resolve_implements, resolve_imports, ...) borrow
    // directly into the `syn` AST, not just the canonical entities derived
    // from it.
    let mut parsed: Vec<(String, syn::File)> = Vec::new();
    for file in &repository.files {
        if file.language != rust {
            continue;
        }
        let source = fs::read_to_string(repository.root.join(&file.path))?;
        parsed.push((file.path.clone(), adapter.parse(&source)?));
    }

    struct PerFile<'a> {
        path: String,
        ast: &'a syn::File,
        entities: Vec<SyntaxEntity>,
        sites: ResolutionSites<'a>,
    }

    let mut per_file: Vec<PerFile> = Vec::new();
    let mut all_entities: Vec<SyntaxEntity> = Vec::new();
    let mut all_edges: Vec<Edge> = Vec::new();
    let mut owner_file: HashMap<EntityId, String> = HashMap::new();

    for (path, ast) in &parsed {
        let mut sites = ResolutionSites::default();
        let (entities, edges) = rust::extract_with_sites(ast, path, &mut sites);
        for entity in &entities {
            owner_file.insert(entity.id.clone(), path.clone());
        }
        all_entities.extend(entities.clone());
        all_edges.extend(edges);
        per_file.push(PerFile {
            path: path.clone(),
            ast,
            entities,
            sites,
        });
    }

    let symbol_table = build_symbol_table(&all_entities);
    let mut all_symbols: Vec<SymbolEntity> = symbol_table.symbols().cloned().collect();
    for entry in symbol_table.entries() {
        if let Some(file) = owner_file.get(&entry.syntax_id) {
            owner_file.insert(entry.symbol.id.clone(), file.clone());
        }
    }

    let mut module_symbol_for_file: HashMap<String, EntityId> = HashMap::new();
    for pf in &per_file {
        // `extract_with_sites` always pushes the file's own Module entity first.
        let file_module = &pf.entities[0];
        if let Some(symbol) = symbol_table.find_by_syntax_id(&file_module.id) {
            module_symbol_for_file.insert(pf.path.clone(), symbol.id.clone());
        }
    }

    let mut new_symbols: Vec<SymbolEntity> = Vec::new();
    let mut new_edges: Vec<Edge> = Vec::new();

    for pf in &per_file {
        let file_module = &pf.entities[0];
        let Some(module_symbol) = symbol_table.find_by_syntax_id(&file_module.id).cloned() else {
            continue;
        };
        let scope_tree = ScopeTree::build(file_module, &pf.entities, &all_edges);

        for (fn_entity, block, _scope) in &pf.sites.bodies {
            let Some(fn_symbol) = symbol_table.find_by_syntax_id(&fn_entity.id).cloned() else {
                continue;
            };
            let Some(scope) = scope_tree.scope_of(&fn_entity.id) else {
                continue;
            };
            let (symbols, edges) =
                resolve_body(block, &fn_symbol, &scope_tree, scope, &symbol_table);
            new_symbols.extend(symbols);
            new_edges.extend(edges);
        }

        for item_impl in pf.sites.impls.iter().copied() {
            let (symbols, edges) = resolve_implements(item_impl, &all_entities, &symbol_table);
            new_symbols.extend(symbols);
            new_edges.extend(edges);
        }

        for item_trait in pf.sites.traits.iter().copied() {
            let (symbols, edges) = resolve_supertraits(item_trait, &all_entities, &symbol_table);
            new_symbols.extend(symbols);
            new_edges.extend(edges);
        }

        for (method_entity, trait_name) in &pf.sites.trait_impl_methods {
            let Some(method_symbol) = symbol_table.find_by_syntax_id(&method_entity.id).cloned()
            else {
                continue;
            };
            let (symbols, edges) = resolve_overrides(
                &method_entity.name,
                &method_symbol,
                trait_name,
                &all_entities,
                &all_edges,
                &symbol_table,
            );
            new_symbols.extend(symbols);
            new_edges.extend(edges);
        }

        let (symbols, edges) =
            resolve_imports(pf.ast, &module_symbol, &all_entities, &symbol_table);
        new_symbols.extend(symbols);
        new_edges.extend(edges);

        let export_edges = resolve_exports(pf.ast, &module_symbol, &all_entities, &symbol_table);
        new_edges.extend(export_edges);
    }

    all_symbols.extend(new_symbols);
    all_edges.extend(new_edges);

    let depends_on = resolve_depends_on(&all_edges, &owner_file, &module_symbol_for_file);
    all_edges.extend(depends_on);

    Ok(AnalysisResult {
        syntax_entities: all_entities,
        symbols: all_symbols,
        edges: all_edges,
    })
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
