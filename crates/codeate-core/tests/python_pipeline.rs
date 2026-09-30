use codeate_core::{
    analyze_repository, default_registry, detect_languages, ingest, EdgeKind, SyntaxKind,
};

struct TempRepo {
    root: std::path::PathBuf,
}

impl TempRepo {
    fn new(files: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "codeate-python-pipeline-test-{}-{}",
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

/// The Python adapter, run through the real repository pipeline (not called
/// directly) -- proves Task 1's generalized dispatch actually reaches
/// `PythonAdapter::resolve_file`, not just `parse`/`extract`.
#[test]
fn analyze_repository_resolves_a_python_file_through_the_real_pipeline() {
    let repo = TempRepo::new(&[(
        "shapes.py",
        "class Shape:\n    def area(self):\n        return 0\n\n\nclass Circle(Shape):\n    def area(self):\n        return helper()\n\n\ndef helper():\n    return 0\n",
    )]);
    let repository = ingest(&repo.root).expect("ingest succeeds");
    let registry = default_registry();
    let repository = detect_languages(&repository, &registry).expect("detect succeeds");

    let result = analyze_repository(&repository).expect("analysis succeeds");

    assert!(result
        .symbols
        .iter()
        .any(|s| s.name == "Shape" && s.kind == codeate_core::SymbolKind::Type));
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
    assert!(result.edges.contains(&codeate_core::Edge::new(
        circle_symbol.id.clone(),
        shape_symbol.id.clone(),
        EdgeKind::Extends
    )));

    let helper_symbol = result
        .symbols
        .iter()
        .find(|s| s.name == "helper")
        .expect("helper symbol");
    assert!(result
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Calls && e.to == helper_symbol.id));
}

/// A Rust file and a Python file in the same repository, both resolved
/// through the same `analyze_repository` call -- the actual proof that the
/// pipeline dispatches per-file rather than assuming one language.
#[test]
fn a_repository_with_both_rust_and_python_files_resolves_both() {
    let repo = TempRepo::new(&[
        ("src/lib.rs", "pub struct Point { pub x: i32 }"),
        ("script.py", "class Circle:\n    pass\n"),
    ]);
    let repository = ingest(&repo.root).expect("ingest succeeds");
    let registry = default_registry();
    let repository = detect_languages(&repository, &registry).expect("detect succeeds");

    let result = analyze_repository(&repository).expect("analysis succeeds");

    assert!(result
        .syntax_entities
        .iter()
        .any(|e| e.kind == SyntaxKind::Struct && e.name == "Point"));
    assert!(result
        .syntax_entities
        .iter()
        .any(|e| e.kind == SyntaxKind::Class && e.name == "Circle"));
}
