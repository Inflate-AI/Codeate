use codeate_api::Codebase;

struct TempRepo {
    root: std::path::PathBuf,
}

impl TempRepo {
    fn new(files: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "codeate-api-test-{}-{}",
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

/// End-to-end coverage (Phase 16 task 5) mirroring
/// `codeate-cli/tests/cli.rs`'s `full_pipeline_end_to_end_covers_every_query_kind_in_both_modes`:
/// the same fixture, taken through `Codebase::open` and every query method
/// instead of the CLI subcommands, asserting equivalent results.
#[test]
fn codebase_open_and_every_query_method_cover_the_full_pipeline() {
    let repo = TempRepo::new(&[
        (
            "src/main.rs",
            "fn main() { crate::shapes::run(); }\nmod shapes;",
        ),
        (
            "src/shapes.rs",
            "pub trait Shape { fn area(&self) -> f64; }\n\
             pub struct Circle;\n\
             impl Shape for Circle { fn area(&self) -> f64 { helper() } }\n\
             fn helper() -> f64 { 0.0 }\n\
             pub fn run() { helper(); }",
        ),
    ]);

    let codebase = Codebase::open(&repo.root).expect("open succeeds");

    assert!(codebase.store_path().exists());
    assert_eq!(codebase.root(), repo.root);
    assert_eq!(codebase.files().len(), 2);

    let shape = codebase
        .symbol("Shape")
        .into_iter()
        .next()
        .expect("Shape symbol");
    let helper = codebase
        .symbol("helper")
        .into_iter()
        .next()
        .expect("helper symbol");
    let run = codebase
        .symbol("run")
        .into_iter()
        .next()
        .expect("run symbol");

    assert_eq!(codebase.symbol_by_id(&shape.id), Some(shape.clone()));

    // definition() follows DEFINED_IN back to the declaring module, not the
    // symbol's own entity -- matches QueryEngine::definition_of and the
    // CLI's `query definition` behavior.
    let definition = codebase.definition(&shape.id).expect("Shape definition");
    assert_eq!(definition.name, "src/shapes.rs");

    assert!(codebase.callers(&helper.id).iter().any(|s| s.name == "run"));
    assert!(codebase.callees(&run.id).iter().any(|s| s.name == "helper"));

    // This fixture's cross-file reference (`crate::shapes::run()`, a fully
    // qualified path, and `mod shapes;`) doesn't resolve to an IMPORTS edge
    // the way a `use` statement would -- mirrors the CLI's own e2e test,
    // which only asserts these calls succeed, not that the result is
    // non-empty (a dedicated fixture with a `use` import, exercised
    // separately, proves cross-file DEPENDS_ON/IMPORTS actually resolves).
    let _ = codebase.dependencies(&run.id);
    let _ = codebase.dependents(&helper.id);

    assert!(codebase
        .implementers(&shape.id)
        .iter()
        .any(|s| s.name == "Circle"));
    assert!(codebase
        .references(&helper.id)
        .iter()
        .any(|s| s.name == "run"));
    let impact = codebase.impact(&helper.id);
    assert!(!impact.is_empty());

    let entry_points = codebase.entry_points();
    assert_eq!(entry_points.len(), 1);
    assert_eq!(entry_points[0].name, "main");

    // `mod shapes;` (an external-file declaration, not an inline `mod { }`
    // body) doesn't link main.rs's module to shapes.rs's own -- each file
    // is its own root in the module tree (a known adapter-scope boundary,
    // not something Phase 16's API layer changes).
    let module_tree = codebase.module_tree();
    let root_names: Vec<&str> = module_tree.iter().map(|t| t.module.name.as_str()).collect();
    assert!(root_names.contains(&"src/main.rs"));
    assert!(root_names.contains(&"src/shapes.rs"));
}

/// `dependencies`/`dependents` actually resolve cross-file when a `use`
/// import connects the two files (unlike the fully-qualified-path/`mod`
/// case in the test above).
#[test]
fn dependencies_and_dependents_follow_a_cross_file_use_import() {
    let repo = TempRepo::new(&[
        ("src/shape.rs", "pub trait Shape { fn area(&self) -> f64; }"),
        (
            "src/circle.rs",
            "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { 0.0 } }",
        ),
    ]);
    let codebase = Codebase::open(&repo.root).expect("open succeeds");

    let circle_module = codebase
        .symbol("src/circle.rs")
        .into_iter()
        .next()
        .expect("circle module symbol");
    let shape = codebase
        .symbol("Shape")
        .into_iter()
        .next()
        .expect("Shape symbol");

    assert!(codebase
        .dependencies(&circle_module.id)
        .iter()
        .any(|id| id == &shape.id));
    let dependents: Vec<_> = codebase.dependents(&shape.id);
    assert!(!dependents.is_empty());
}

/// A repository with a real `Cargo.toml` exercises the Phase 14 query
/// methods too.
#[test]
fn codebase_exposes_phase_14_repository_level_queries() {
    let repo = TempRepo::new(&[
        (
            "Cargo.toml",
            "[package]\nname = \"widgets\"\n\n[dependencies]\nserde = \"1\"\n",
        ),
        (
            "src/main.rs",
            "fn add(a: i32, b: i32) -> i32 { a + b }\nfn main() { add(1, 2); }\n\n#[test]\nfn it_adds() { assert_eq!(add(1, 1), 2); }\n",
        ),
    ]);

    let codebase = Codebase::open(&repo.root).expect("open succeeds");

    assert!(codebase.packages().iter().any(|e| e.name == "widgets"));
    assert!(codebase
        .package_dependencies()
        .iter()
        .any(|e| e.name == "serde"));
    assert!(codebase
        .build_targets()
        .iter()
        .any(|e| e.name == "bin:widgets"));
    assert!(codebase.tests().iter().any(|e| e.name == "it_adds"));
    assert!(!codebase.manifest_entry_points().is_empty());
    // No profile sections or route/generated markers in this fixture.
    assert!(codebase.configurations().is_empty());
    assert!(codebase.api_endpoints().is_empty());
    assert!(codebase.generated_files().is_empty());
}

/// `refresh` picks up a source change on disk and `save` persists the
/// current state on demand -- mirroring the CLI's incremental `analyze`
/// behavior.
#[test]
fn refresh_picks_up_a_source_change_and_save_persists_on_demand() {
    let repo = TempRepo::new(&[("src/lib.rs", "pub struct Point { pub x: i32 }")]);

    let mut codebase = Codebase::open(&repo.root).expect("open succeeds");
    assert!(codebase.symbol("Line").is_empty());

    std::fs::write(
        repo.root.join("src/lib.rs"),
        "pub struct Point { pub x: i32 }\npub struct Line { pub a: i32 }",
    )
    .expect("rewrite lib.rs");

    codebase.refresh().expect("refresh succeeds");
    assert!(!codebase.symbol("Line").is_empty());

    codebase.save().expect("save succeeds");
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(codebase.store_path()).expect("read store"))
            .expect("store is valid json");
    assert!(stored["syntax_entities"]
        .as_array()
        .expect("syntax_entities array")
        .iter()
        .any(|e| e["name"] == "Line"));
}
