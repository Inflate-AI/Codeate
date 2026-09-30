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

/// Phase 17.2 task 7: extend the cross-adapter fixture with an equivalent
/// TypeScript one. TypeScript's `IMPLEMENTS` can be explicit (`class ...
/// implements Shape`) or, like Go's, structurally inferred with no
/// `implements` clause at all -- this fixture deliberately uses the
/// structural form, so the *query behavior* for interface satisfaction
/// matches Go's own structural `IMPLEMENTS` (and Rust's/Python's explicit
/// `IMPLEMENTS`/`EXTENDS`) exactly, including across files.
///
/// Like the Go fixture, this one keeps `helper`'s call site in the same
/// file as its declaration: cross-file bare-name resolution is a known,
/// pre-existing limitation shared by every adapter's own `ScopeTree`-based
/// call resolution (each file only sees its own entities), not something
/// specific to TypeScript. `IMPORTS`/`DEPENDS_ON` *is* exercised here,
/// though (unlike the Go fixture) -- TypeScript's `import` is explicit, so
/// `circle.ts`'s `import { Shape } from "./shape"` is a real IMPORTS site,
/// even though the resolved target is an external-module placeholder
/// rather than `shape.ts` itself (Codeate doesn't resolve relative import
/// specifiers against repository files yet -- see
/// `adapters::typescript::resolve_imports`'s own documented gap) -- so this
/// fixture checks for the IMPORTS edge itself, not a resolved
/// `DEPENDS_ON`/`dependencies_of` relationship the way the Rust/Python
/// fixture does.
#[test]
fn typescript_adapter_produces_equivalent_implements_query_behavior_to_go_rust_and_python() {
    let ts_repo = TempRepo::new(&[
        (
            "shape.ts",
            "export interface Shape {\n  area(): number;\n}\n",
        ),
        (
            "circle.ts",
            "import { Shape } from \"./shape\";\n\nexport class Circle {\n  area(): number {\n    return helper();\n  }\n}\n\nfunction helper(): number {\n  return 0;\n}\n",
        ),
    ]);

    let ts_result = analyze(&ts_repo);
    let (ts_graph, ts_report) = Graph::build(
        ts_result.syntax_entities.clone(),
        ts_result.symbols.clone(),
        ts_result.edges.clone(),
    );
    assert!(ts_report.is_clean());

    let ts_engine = QueryEngine::new(&ts_graph);

    // Structural relationship: Circle satisfies Shape across files, exactly
    // as Go's cross-file structural IMPLEMENTS does.
    let ts_shape = ts_engine
        .symbols_named("Shape")
        .into_iter()
        .next()
        .expect("TypeScript Shape symbol");
    let ts_impact = ts_engine.impact_of(&ts_shape.id);
    assert!(
        ts_impact
            .iter()
            .any(|id| ts_graph.node(id).is_some_and(|n| n.name() == "Circle")),
        "TypeScript: Circle should be in Shape's impact set via structural IMPLEMENTS"
    );

    // Same-file call: Circle.area calls helper (both declared in circle.ts).
    let ts_helper = ts_engine
        .symbols_named("helper")
        .into_iter()
        .next()
        .expect("TypeScript helper symbol");
    let ts_callers = ts_engine.callers_of(&ts_helper.id);
    assert!(
        ts_callers.iter().any(|s| s.name == "area"),
        "TypeScript: area should call helper"
    );

    for kind in [
        EdgeKind::Calls,
        EdgeKind::References,
        EdgeKind::Implements,
        EdgeKind::Imports,
    ] {
        assert!(
            ts_result.edges.iter().any(|e| e.kind == kind),
            "TypeScript: expected at least one {kind:?} edge"
        );
    }
}
