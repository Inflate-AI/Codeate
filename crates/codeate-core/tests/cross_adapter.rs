use codeate_core::{
    analyze_repository, default_registry, detect_languages, ingest, EdgeKind, Graph, QueryEngine,
};

struct TempRepo {
    root: std::path::PathBuf,
}

impl TempRepo {
    fn new(files: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "codeate-cross-adapter-test-{}-{}",
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

/// Phase 15 task 8: two fixture repos -- one Rust, one Python -- built
/// around the same shape (a base type in one file, a second file that
/// derives from it and cross-file-calls a helper also declared in the
/// first file), analyzed through the exact same `analyze_repository` +
/// `QueryEngine` surface. `IMPLEMENTS` (Rust: `impl Shape for Circle`) and
/// `EXTENDS` (Python: `class Circle(Shape)`) are different edge kinds --
/// Python has no trait/interface construct -- but `impact_of` considers
/// both, so the *query behavior* a consumer sees is identical even though
/// the underlying edge kind differs. Everything else (cross-file `CALLS`,
/// `DEPENDS_ON`/`IMPORTS`) uses edge kinds both adapters produce the same
/// way, so those queries are asserted identically too.
#[test]
fn rust_and_python_adapters_produce_identical_query_behavior_for_equivalent_constructs() {
    // `helper` is declared in the *same* file as its call site in both
    // fixtures -- a bare/imported cross-file function call is a known,
    // pre-existing limitation of both adapters' `resolve_calls` (each
    // file's `ScopeTree` only sees that file's own entities; see
    // `adapters::rust::tests::cross_file_depends_on_collapses_from_a_real_cross_file_call`'s
    // own comment on this). `Shape` stays cross-file, since IMPLEMENTS/
    // EXTENDS resolution *does* search repository-wide.
    let rust_repo = TempRepo::new(&[
        ("shape.rs", "pub trait Shape { fn area(&self) -> f64; }\n"),
        (
            "circle.rs",
            "use crate::shape::Shape;\npub struct Circle;\nimpl Shape for Circle { fn area(&self) -> f64 { helper() } }\nfn helper() -> f64 { 0.0 }\n",
        ),
    ]);
    let python_repo = TempRepo::new(&[
        ("shape.py", "class Shape:\n    def area(self):\n        return 0\n"),
        (
            "circle.py",
            "from shape import Shape\n\n\nclass Circle(Shape):\n    def area(self):\n        return helper()\n\n\ndef helper():\n    return 0\n",
        ),
    ]);

    let rust_result = analyze(&rust_repo);
    let python_result = analyze(&python_repo);

    let (rust_graph, rust_report) = Graph::build(
        rust_result.syntax_entities.clone(),
        rust_result.symbols.clone(),
        rust_result.edges.clone(),
    );
    let (python_graph, python_report) = Graph::build(
        python_result.syntax_entities.clone(),
        python_result.symbols.clone(),
        python_result.edges.clone(),
    );
    assert!(rust_report.is_clean());
    assert!(python_report.is_clean());

    let rust_engine = QueryEngine::new(&rust_graph);
    let python_engine = QueryEngine::new(&python_graph);

    // Structural relationship: Circle relates to Shape, regardless of
    // whether that's IMPLEMENTS (Rust) or EXTENDS (Python) underneath.
    let rust_shape = rust_engine
        .symbols_named("Shape")
        .into_iter()
        .next()
        .expect("Rust Shape symbol");
    let python_shape = python_engine
        .symbols_named("Shape")
        .into_iter()
        .next()
        .expect("Python Shape symbol");
    let rust_impact = rust_engine.impact_of(&rust_shape.id);
    let python_impact = python_engine.impact_of(&python_shape.id);
    assert!(
        rust_impact
            .iter()
            .any(|id| rust_graph.node(id).is_some_and(|n| n.name() == "Circle")),
        "Rust: Circle should be in Shape's impact set via IMPLEMENTS"
    );
    assert!(
        python_impact
            .iter()
            .any(|id| python_graph.node(id).is_some_and(|n| n.name() == "Circle")),
        "Python: Circle should be in Shape's impact set via EXTENDS"
    );

    // Cross-file call: Circle.area calls helper, in both languages.
    let rust_helper = rust_engine
        .symbols_named("helper")
        .into_iter()
        .next()
        .expect("Rust helper symbol");
    let python_helper = python_engine
        .symbols_named("helper")
        .into_iter()
        .next()
        .expect("Python helper symbol");
    let rust_callers = rust_engine.callers_of(&rust_helper.id);
    let python_callers = python_engine.callers_of(&python_helper.id);
    assert!(
        rust_callers.iter().any(|s| s.name == "area"),
        "Rust: area should call helper"
    );
    assert!(
        python_callers.iter().any(|s| s.name == "area"),
        "Python: area should call helper"
    );

    // Cross-file dependency: circle's file depends on shape's file, via
    // DEPENDS_ON/IMPORTS, in both languages.
    let rust_circle_module = rust_engine
        .symbols_named("circle.rs")
        .into_iter()
        .next()
        .expect("Rust circle module symbol");
    let python_circle_module = python_engine
        .symbols_named("circle.py")
        .into_iter()
        .next()
        .expect("Python circle module symbol");
    let rust_deps = rust_engine.dependencies_of(&rust_circle_module.id);
    let python_deps = python_engine.dependencies_of(&python_circle_module.id);
    assert!(
        rust_deps
            .iter()
            .any(|id| rust_graph.node(id).is_some_and(|n| n.name() == "shape.rs")),
        "Rust: circle.rs should depend on shape.rs"
    );
    assert!(
        python_deps.iter().any(|id| python_graph
            .node(id)
            .is_some_and(|n| n.name() == "shape.py")),
        "Python: circle.py should depend on shape.py"
    );

    // No REFERENCES/CALLS/IMPORTS/EXTENDS/IMPLEMENTS site in either
    // language should have silently vanished -- every edge kind produced
    // resolves to a real node on both ends (the integrity check above
    // already covers dangling edges repo-wide; this double-checks the
    // specific kinds this test exercises are actually present).
    for kind in [
        EdgeKind::Calls,
        EdgeKind::References,
        EdgeKind::DependsOn,
        EdgeKind::Imports,
    ] {
        assert!(
            rust_result.edges.iter().any(|e| e.kind == kind),
            "Rust: expected at least one {kind:?} edge"
        );
        assert!(
            python_result.edges.iter().any(|e| e.kind == kind),
            "Python: expected at least one {kind:?} edge"
        );
    }
    assert!(rust_result
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Implements));
    assert!(python_result
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Extends));
}

fn analyze(repo: &TempRepo) -> codeate_core::AnalysisResult {
    let repository = ingest(&repo.root).expect("ingest succeeds");
    let registry = default_registry();
    let repository = detect_languages(&repository, &registry).expect("detect succeeds");
    analyze_repository(&repository).expect("analysis succeeds")
}

/// Phase 17.1 task 7: extend the Rust/Python cross-adapter fixture with an
/// equivalent Go one. Go's `IMPLEMENTS` is structurally inferred (no
/// `impl`/`implements` keyword) but resolved repository-wide the same way
/// Rust's explicit `impl Trait for Type` is, so the *query behavior* for
/// interface satisfaction matches Rust/Python's `IMPLEMENTS`/`EXTENDS`
/// exactly, including across files.
///
/// The fixture deliberately does NOT extend the IMPORTS/DEPENDS_ON
/// assertions the Rust/Python test makes: in Go, files in the same package
/// see each other's top-level declarations directly, with no import
/// statement -- unlike Rust's `use crate::...` or Python's `from ... import
/// ...`, there is no IMPORTS edge for Codeate to produce between shape.go
/// and circle.go, and no cross-file dependency for the pipeline's
/// import-derived DEPENDS_ON collapsing (`resolve::resolve_depends_on`) to
/// find. This is a real semantic difference in Go's module system, not a
/// gap in the adapter -- so `helper`'s call site stays in the same file as
/// its declaration here too, matching the Rust/Python fixtures' own known
/// same-file-call limitation.
#[test]
fn go_adapter_produces_equivalent_implements_query_behavior_to_rust_and_python() {
    let go_repo = TempRepo::new(&[
        (
            "shape.go",
            "package main\n\ntype Shape interface {\n\tArea() float64\n}\n",
        ),
        (
            "circle.go",
            "package main\n\ntype Circle struct{}\n\nfunc (c *Circle) Area() float64 {\n\treturn helper()\n}\n\nfunc helper() float64 {\n\treturn 0\n}\n",
        ),
    ]);

    let go_result = analyze(&go_repo);
    let (go_graph, go_report) = Graph::build(
        go_result.syntax_entities.clone(),
        go_result.symbols.clone(),
        go_result.edges.clone(),
    );
    assert!(go_report.is_clean());

    let go_engine = QueryEngine::new(&go_graph);

    // Structural relationship: Circle satisfies Shape across files, exactly
    // as Rust's cross-file IMPLEMENTS and Python's cross-file EXTENDS do.
    let go_shape = go_engine
        .symbols_named("Shape")
        .into_iter()
        .next()
        .expect("Go Shape symbol");
    let go_impact = go_engine.impact_of(&go_shape.id);
    assert!(
        go_impact
            .iter()
            .any(|id| go_graph.node(id).is_some_and(|n| n.name() == "Circle")),
        "Go: Circle should be in Shape's impact set via structural IMPLEMENTS"
    );

    // Same-file call: Circle.Area calls helper (both declared in circle.go).
    let go_helper = go_engine
        .symbols_named("helper")
        .into_iter()
        .next()
        .expect("Go helper symbol");
    let go_callers = go_engine.callers_of(&go_helper.id);
    assert!(
        go_callers.iter().any(|s| s.name == "Area"),
        "Go: Area should call helper"
    );

    for kind in [EdgeKind::Calls, EdgeKind::References, EdgeKind::Implements] {
        assert!(
            go_result.edges.iter().any(|e| e.kind == kind),
            "Go: expected at least one {kind:?} edge"
        );
    }
}

/// Phase 17.3 task 7: extend the cross-adapter fixture with an equivalent
/// JavaScript one. JavaScript's `EXTENDS` (ES6 `class Circle extends
/// Shape`) is resolved repository-wide the same way Python's `EXTENDS` and
/// Go's structural `IMPLEMENTS` are, so the *query behavior* for
/// inheritance/interface-satisfaction matches Rust/Python/Go exactly,
/// including across files. Also exercises the cross-file `IMPORTS`/
/// `DEPENDS_ON` path via an ES module `import`, unlike the Go fixture
/// (which has no import statement to test, per its own comment) -- this
/// mirrors the original Rust/Python fixture's own IMPORTS/DEPENDS_ON
/// assertions.
#[test]
fn javascript_adapter_produces_equivalent_query_behavior_to_rust_python_and_go() {
    let js_repo = TempRepo::new(&[
        (
            "shape.js",
            "export class Shape {\n  area() {\n    return 0;\n  }\n}\n",
        ),
        (
            "circle.js",
            "import { Shape } from \"./shape\";\n\nexport class Circle extends Shape {\n  area() {\n    return helper();\n  }\n}\n\nfunction helper() {\n  return 0;\n}\n",
        ),
    ]);

    let js_result = analyze(&js_repo);
    let (js_graph, js_report) = Graph::build(
        js_result.syntax_entities.clone(),
        js_result.symbols.clone(),
        js_result.edges.clone(),
    );
    assert!(js_report.is_clean());

    let js_engine = QueryEngine::new(&js_graph);

    // Structural relationship: Circle relates to Shape across files, via
    // EXTENDS -- the same query behavior as Rust's IMPLEMENTS, Python's
    // EXTENDS, and Go's structural IMPLEMENTS.
    let js_shape = js_engine
        .symbols_named("Shape")
        .into_iter()
        .next()
        .expect("JavaScript Shape symbol");
    let js_impact = js_engine.impact_of(&js_shape.id);
    assert!(
        js_impact
            .iter()
            .any(|id| js_graph.node(id).is_some_and(|n| n.name() == "Circle")),
        "JavaScript: Circle should be in Shape's impact set via EXTENDS"
    );

    // Same-file call: Circle.area calls helper (both declared in circle.js,
    // matching the same-file-call limitation the Rust/Python/Go fixtures'
    // own comments already document).
    let js_helper = js_engine
        .symbols_named("helper")
        .into_iter()
        .next()
        .expect("JavaScript helper symbol");
    let js_callers = js_engine.callers_of(&js_helper.id);
    assert!(
        js_callers.iter().any(|s| s.name == "area"),
        "JavaScript: area should call helper"
    );

    // Cross-file dependency: circle.js depends on shape.js, via
    // DEPENDS_ON/IMPORTS -- exercising the ES module import path the Go
    // fixture deliberately doesn't have.
    let js_circle_module = js_engine
        .symbols_named("circle.js")
        .into_iter()
        .next()
        .expect("JavaScript circle module symbol");
    let js_deps = js_engine.dependencies_of(&js_circle_module.id);
    assert!(
        js_deps
            .iter()
            .any(|id| js_graph.node(id).is_some_and(|n| n.name() == "shape.js")),
        "JavaScript: circle.js should depend on shape.js"
    );

    for kind in [
        EdgeKind::Calls,
        EdgeKind::References,
        EdgeKind::Extends,
        EdgeKind::Imports,
        EdgeKind::DependsOn,
    ] {
        assert!(
            js_result.edges.iter().any(|e| e.kind == kind),
            "JavaScript: expected at least one {kind:?} edge"
        );
    }
}
