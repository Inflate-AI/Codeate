use std::collections::HashMap;
use std::fs;

use crate::adapter::LanguageAdapter;
use crate::adapters::RustAdapter;
use crate::error::Result;
use crate::language::LanguageId;
use crate::manifest::{BuildTargetKind, ManifestResult};
use crate::model::{Edge, EdgeKind, EntityId, SymbolEntity, SyntaxEntity, SyntaxKind};
use crate::pipeline::AnalysisResult;
use crate::repository::Repository;

/// Entities/edges Phase 14's source-level discovery (tests, entry points,
/// API endpoints, generated code) contributes to the graph, meant to be
/// merged alongside `pipeline::AnalysisResult` and `manifest::ManifestResult`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DiscoveryResult {
    pub entities: Vec<SyntaxEntity>,
    pub edges: Vec<Edge>,
    pub owner_file: HashMap<EntityId, String>,
}

/// Attribute macro names (`#[name(...)]` or bare `#[name]`) recognized as
/// marking a function as an HTTP route -- covers the common shape of
/// Actix-web/Rocket/Axum-style routing attributes. Best-effort, per the
/// README's "API Endpoint... representation (best-effort, adapter-dependent)".
const ROUTE_ATTRIBUTES: [&str; 6] = ["get", "post", "put", "delete", "patch", "route"];

/// Marker Codeate looks for in a file's first few lines to flag it as
/// machine-generated (the `@generated` convention used by protobuf/prost,
/// OpenAPI generators, etc.). Best-effort, per the README's "Generated Code
/// representation (best-effort, adapter-dependent)".
const GENERATED_MARKER: &str = "@generated";
const GENERATED_MARKER_SCAN_LINES: usize = 5;

/// Run Phase 14's source-level discovery over every Rust file in
/// `repository`, using `analysis` (Phase 4-8's output, for matching a
/// discovered site to its already-canonical `Function`/`Module` symbol) and
/// `manifest` (for `BuildTarget` -> root-file resolution) as context.
pub fn discover(
    repository: &Repository,
    analysis: &AnalysisResult,
    manifest: &ManifestResult,
) -> Result<DiscoveryResult> {
    let adapter = RustAdapter;
    let rust = LanguageId::known("rust");
    let mut result = DiscoveryResult::default();

    for file in &repository.files {
        if file.language != rust {
            continue;
        }
        let source = fs::read_to_string(repository.root.join(&file.path))?;

        if is_generated(&source) {
            add_generated_code(&mut result, &file.path);
        }

        let ast = match adapter.parse(&source) {
            Ok(ast) => ast,
            // A file this repository classifies as Rust but that the
            // adapter can't parse already failed loudly during the main
            // pipeline run; discovery degrades gracefully instead of
            // failing the whole repository-level pass over it again.
            Err(_) => continue,
        };

        let mut functions: Vec<&syn::ItemFn> = Vec::new();
        collect_fns(&ast.items, &mut functions);

        for item_fn in functions {
            let name = item_fn.sig.ident.to_string();

            if has_attribute(&item_fn.attrs, "test") {
                add_test(&mut result, analysis, &file.path, &name);
            }

            if let Some(route) = route_attribute(&item_fn.attrs) {
                add_api_endpoint(&mut result, analysis, &file.path, &name, route);
            }
        }
    }

    add_entry_points(&mut result, analysis, manifest);

    Ok(result)
}

fn collect_fns<'a>(items: &'a [syn::Item], out: &mut Vec<&'a syn::ItemFn>) {
    for item in items {
        match item {
            syn::Item::Fn(item_fn) => out.push(item_fn),
            syn::Item::Mod(item_mod) => {
                if let Some((_, nested)) = &item_mod.content {
                    collect_fns(nested, out);
                }
            }
            _ => {}
        }
    }
}

fn has_attribute(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident(name))
}

fn route_attribute(attrs: &[syn::Attribute]) -> Option<&'static str> {
    ROUTE_ATTRIBUTES
        .iter()
        .find(|name| has_attribute(attrs, name))
        .copied()
}

fn is_generated(source: &str) -> bool {
    source
        .lines()
        .take(GENERATED_MARKER_SCAN_LINES)
        .any(|line| line.contains(GENERATED_MARKER))
}

/// The `Function`/`Method` symbol Phase 4-8 resolved for `name` declared in
/// `path`, if any -- the anchor a discovered site (test, endpoint, entry
/// point) links back to via `REFERENCES`.
fn find_symbol<'a>(
    analysis: &'a AnalysisResult,
    path: &str,
    name: &str,
) -> Option<&'a SymbolEntity> {
    analysis.symbols.iter().find(|symbol| {
        symbol.name == name
            && analysis
                .owner_file
                .get(&symbol.id)
                .is_some_and(|owner| owner == path)
    })
}

fn add_test(result: &mut DiscoveryResult, analysis: &AnalysisResult, path: &str, name: &str) {
    let entity = SyntaxEntity::new(SyntaxKind::Test, path, name);
    result
        .owner_file
        .insert(entity.id.clone(), path.to_string());
    if let Some(symbol) = find_symbol(analysis, path, name) {
        result.edges.push(Edge::new(
            entity.id.clone(),
            symbol.id.clone(),
            EdgeKind::References,
        ));
    }
    result.entities.push(entity);
}

fn add_api_endpoint(
    result: &mut DiscoveryResult,
    analysis: &AnalysisResult,
    path: &str,
    name: &str,
    method: &str,
) {
    let entity = SyntaxEntity::new(SyntaxKind::ApiEndpoint, path, format!("{method}:{name}"));
    result
        .owner_file
        .insert(entity.id.clone(), path.to_string());
    if let Some(symbol) = find_symbol(analysis, path, name) {
        result.edges.push(Edge::new(
            entity.id.clone(),
            symbol.id.clone(),
            EdgeKind::References,
        ));
    }
    result.entities.push(entity);
}

fn add_generated_code(result: &mut DiscoveryResult, path: &str) {
    let entity = SyntaxEntity::new(SyntaxKind::GeneratedCode, path, path);
    result
        .owner_file
        .insert(entity.id.clone(), path.to_string());
    result.entities.push(entity);
}

fn add_entry_points(
    result: &mut DiscoveryResult,
    analysis: &AnalysisResult,
    manifest: &ManifestResult,
) {
    for target in &manifest.build_targets {
        let entry_symbol = match target.kind {
            BuildTargetKind::Bin => find_symbol(analysis, &target.path, "main"),
            BuildTargetKind::Lib => analysis
                .symbols
                .iter()
                .find(|symbol| symbol.name == target.path),
            BuildTargetKind::Test | BuildTargetKind::Bench | BuildTargetKind::Example => None,
        };
        let Some(entry_symbol) = entry_symbol else {
            continue;
        };

        let owner_path = analysis
            .owner_file
            .get(&entry_symbol.id)
            .cloned()
            .unwrap_or_else(|| target.path.clone());
        let entity = SyntaxEntity::new(SyntaxKind::EntryPoint, &owner_path, &target.path);
        result
            .owner_file
            .insert(entity.id.clone(), owner_path.clone());
        result.edges.push(Edge::new(
            target.id.clone(),
            entity.id.clone(),
            EdgeKind::Contains,
        ));
        result.edges.push(Edge::new(
            entity.id.clone(),
            entry_symbol.id.clone(),
            EdgeKind::References,
        ));
        result.entities.push(entity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{default_registry, detect_languages};
    use crate::ingest::ingest;
    use crate::manifest::{parse_manifests, BuildTargetInfo};
    use crate::model::{SymbolKind, SyntaxKind};
    use crate::pipeline::analyze_repository;

    struct TempRepo {
        root: std::path::PathBuf,
    }

    impl TempRepo {
        fn new(files: &[(&str, &str)]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "codeate-discovery-test-{}-{}",
                std::process::id(),
                unique_suffix()
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

        fn analyze(&self) -> (Repository, AnalysisResult, ManifestResult) {
            let repository = ingest(&self.root).expect("ingest succeeds");
            let registry = default_registry();
            let repository = detect_languages(&repository, &registry).expect("detect succeeds");
            let analysis = analyze_repository(&repository).expect("analyze succeeds");
            let manifest = parse_manifests(&repository).expect("manifest parse succeeds");
            (repository, analysis, manifest)
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn a_test_attributed_function_becomes_a_test_entity_linked_to_its_function_symbol() {
        let repo = TempRepo::new(&[(
            "src/lib.rs",
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n\n#[test]\nfn it_adds() { assert_eq!(add(1, 1), 2); }",
        )]);
        let (repository, analysis, manifest) = repo.analyze();

        let discovery = discover(&repository, &analysis, &manifest).expect("discover succeeds");

        let test_entity = discovery
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::Test && e.name == "it_adds")
            .expect("test entity");
        let function_symbol = analysis
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Function && s.name == "it_adds")
            .expect("function symbol for the test fn");
        assert!(discovery.edges.contains(&Edge::new(
            test_entity.id.clone(),
            function_symbol.id.clone(),
            EdgeKind::References
        )));
    }

    #[test]
    fn a_plain_function_is_not_treated_as_a_test() {
        let repo = TempRepo::new(&[("src/lib.rs", "pub fn helper() {}")]);
        let (repository, analysis, manifest) = repo.analyze();

        let discovery = discover(&repository, &analysis, &manifest).expect("discover succeeds");

        assert!(discovery
            .entities
            .iter()
            .all(|e| e.kind != SyntaxKind::Test));
    }

    #[test]
    fn a_route_attributed_function_becomes_an_api_endpoint() {
        let repo = TempRepo::new(&[("src/lib.rs", "#[get(\"/widgets\")]\nfn list_widgets() {}")]);
        let (repository, analysis, manifest) = repo.analyze();

        let discovery = discover(&repository, &analysis, &manifest).expect("discover succeeds");

        assert!(discovery
            .entities
            .iter()
            .any(|e| e.kind == SyntaxKind::ApiEndpoint && e.name == "get:list_widgets"));
    }

    #[test]
    fn a_leading_generated_marker_flags_the_whole_file() {
        let repo = TempRepo::new(&[(
            "src/generated.rs",
            "// @generated by prost-build\npub struct Widget;",
        )]);
        let (repository, analysis, manifest) = repo.analyze();

        let discovery = discover(&repository, &analysis, &manifest).expect("discover succeeds");

        assert!(discovery
            .entities
            .iter()
            .any(|e| e.kind == SyntaxKind::GeneratedCode && e.name == "src/generated.rs"));
    }

    #[test]
    fn a_file_with_no_generated_marker_is_not_flagged() {
        let repo = TempRepo::new(&[("src/lib.rs", "pub struct Widget;")]);
        let (repository, analysis, manifest) = repo.analyze();

        let discovery = discover(&repository, &analysis, &manifest).expect("discover succeeds");

        assert!(discovery
            .entities
            .iter()
            .all(|e| e.kind != SyntaxKind::GeneratedCode));
    }

    #[test]
    fn a_bin_target_s_main_function_becomes_an_entry_point_linked_to_its_build_target() {
        let repo = TempRepo::new(&[("src/main.rs", "fn helper() {}\nfn main() { helper(); }")]);
        let (repository, analysis, mut manifest) = repo.analyze();
        // No Cargo.toml in this fixture -- synthesize the build target info
        // that manifest parsing would have produced from one, so this test
        // isolates entry-point detection from manifest parsing (already
        // covered by manifest.rs's own tests).
        let build_target_entity =
            SyntaxEntity::new(SyntaxKind::BuildTarget, "Cargo.toml", "bin:widgets");
        manifest.build_targets.push(BuildTargetInfo {
            id: build_target_entity.id.clone(),
            kind: BuildTargetKind::Bin,
            path: "src/main.rs".to_string(),
        });

        let discovery = discover(&repository, &analysis, &manifest).expect("discover succeeds");

        let entry_point = discovery
            .entities
            .iter()
            .find(|e| e.kind == SyntaxKind::EntryPoint)
            .expect("entry point entity");
        let main_symbol = analysis
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Function && s.name == "main")
            .expect("main function symbol");
        assert!(discovery.edges.contains(&Edge::new(
            build_target_entity.id,
            entry_point.id.clone(),
            EdgeKind::Contains
        )));
        assert!(discovery.edges.contains(&Edge::new(
            entry_point.id.clone(),
            main_symbol.id.clone(),
            EdgeKind::References
        )));
    }

    #[test]
    fn a_lib_target_s_crate_root_module_becomes_an_entry_point() {
        let repo = TempRepo::new(&[("src/lib.rs", "pub fn run() {}")]);
        let (repository, analysis, mut manifest) = repo.analyze();
        let build_target_entity =
            SyntaxEntity::new(SyntaxKind::BuildTarget, "Cargo.toml", "lib:widgets");
        manifest.build_targets.push(BuildTargetInfo {
            id: build_target_entity.id.clone(),
            kind: BuildTargetKind::Lib,
            path: "src/lib.rs".to_string(),
        });

        let discovery = discover(&repository, &analysis, &manifest).expect("discover succeeds");

        assert!(discovery
            .entities
            .iter()
            .any(|e| e.kind == SyntaxKind::EntryPoint));
    }
}
