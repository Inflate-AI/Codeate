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

/// Phase 17.7 task 7: extend the fixture with a C one. C has no interface-
/// satisfaction construct at all (no classes/interfaces/inheritance -- see
/// `adapters::c`'s own module doc comment), so unlike every prior fixture in
/// this file there is no EXTENDS/IMPLEMENTS-equivalence to test. Instead
/// this fixture proves the same CALLS/REFERENCES query behavior
/// (`callers_of`) holds for C's free functions as it does for every other
/// adapter's methods/functions: `compute_area` calls `helper`, same file,
/// same as the Rust/Python/Go/TypeScript/JavaScript/Java fixtures' own
/// same-file-call shape.
#[test]
fn c_adapter_produces_equivalent_calls_query_behavior_to_every_other_adapter() {
    let c_repo = TempRepo::new(&[(
        "shape.c",
        "struct Circle {\n  double radius;\n};\n\ndouble helper(void) {\n  return 0;\n}\n\ndouble compute_area(struct Circle c) {\n  return helper();\n}\n",
    )]);

    let c_result = analyze(&c_repo);
    let (c_graph, c_report) = Graph::build(
        c_result.syntax_entities.clone(),
        c_result.symbols.clone(),
        c_result.edges.clone(),
    );
    assert!(c_report.is_clean());

    let c_engine = QueryEngine::new(&c_graph);

    // Same-file call: compute_area calls helper, exactly the same
    // CALLS/callers_of query shape as every other language's fixture in
    // this file.
    let c_helper = c_engine
        .symbols_named("helper")
        .into_iter()
        .next()
        .expect("C helper symbol");
    let c_callers = c_engine.callers_of(&c_helper.id);
    assert!(
        c_callers.iter().any(|s| s.name == "compute_area"),
        "C: compute_area should call helper"
    );

    for kind in [EdgeKind::Calls, EdgeKind::References] {
        assert!(
            c_result.edges.iter().any(|e| e.kind == kind),
            "C: expected at least one {kind:?} edge"
        );
    }

    // The correct absence: no EXTENDS/IMPLEMENTS anywhere in this fixture's
    // edges -- C has no construct for either, unlike every other adapter
    // in this suite.
    assert!(!c_result.edges.iter().any(|e| e.kind == EdgeKind::Extends));
    assert!(!c_result.edges.iter().any(|e| e.kind == EdgeKind::Implements));
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

/// Phase 17.4 task 7: extend the fixture with a Java one. Java's
/// `IMPLEMENTS` is read directly off the explicit `implements` keyword (no
/// method-set comparison, unlike Go's structural inference) but resolved
/// repository-wide the same way -- so the *query behavior* for interface
/// satisfaction matches Rust's explicit `impl Trait for Type`, Python's
/// `EXTENDS`, and Go's structural `IMPLEMENTS` exactly, including across
/// files. This is the clearest same-vs-different pairing in the whole suite:
/// Java and Go both produce `IMPLEMENTS` edges for "a type satisfies an
/// interface," arrived at by opposite means (explicit keyword vs. structural
/// inference), yet `impact_of` sees no difference between them.
#[test]
fn java_adapter_produces_equivalent_implements_query_behavior_to_rust_python_and_go() {
    let java_repo = TempRepo::new(&[
        (
            "Shape.java",
            "public interface Shape {\n    double area();\n}\n",
        ),
        (
            "Circle.java",
            "public class Circle implements Shape {\n    public double area() {\n        return helper();\n    }\n\n    double helper() {\n        return 0;\n    }\n}\n",
        ),
    ]);

    let java_result = analyze(&java_repo);
    let (java_graph, java_report) = Graph::build(
        java_result.syntax_entities.clone(),
        java_result.symbols.clone(),
        java_result.edges.clone(),
    );
    assert!(java_report.is_clean());

    let java_engine = QueryEngine::new(&java_graph);

    // Structural relationship: Circle satisfies Shape across files, exactly
    // as Rust's cross-file IMPLEMENTS, Python's cross-file EXTENDS, and Go's
    // cross-file structural IMPLEMENTS all do.
    let java_shape = java_engine
        .symbols_named("Shape")
        .into_iter()
        .next()
        .expect("Java Shape symbol");
    let java_impact = java_engine.impact_of(&java_shape.id);
    assert!(
        java_impact
            .iter()
            .any(|id| java_graph.node(id).is_some_and(|n| n.name() == "Circle")),
        "Java: Circle should be in Shape's impact set via explicit IMPLEMENTS"
    );

    // Same-file call: Circle.area calls helper (both declared in Circle.java,
    // matching the same-file-call limitation the Rust/Python/Go fixtures
    // already document for their own adapters' scope-tree reach).
    let java_helper = java_engine
        .symbols_named("helper")
        .into_iter()
        .next()
        .expect("Java helper symbol");
    let java_callers = java_engine.callers_of(&java_helper.id);
    assert!(
        java_callers.iter().any(|s| s.name == "area"),
        "Java: area should call helper"
    );

    for kind in [EdgeKind::Calls, EdgeKind::References, EdgeKind::Implements] {
        assert!(
            java_result.edges.iter().any(|e| e.kind == kind),
            "Java: expected at least one {kind:?} edge"
        );
    }
}

/// Phase 17.5 task 7: extend the fixture with a C# one. C# has no separate
/// `implements` keyword the way Java does -- `Circle : IShape` uses the same
/// base-list syntax a base *class* would -- so this exercises
/// `adapters::csharp::resolve_structural`'s own disambiguation (an
/// `I`-prefixed, capitalized name with no matching `Class`/`Struct` entity
/// in the repository resolves as an interface) rather than an explicit
/// keyword. Once resolved, though, the *query behavior* is identical to
/// every other adapter's own IMPLEMENTS/EXTENDS: `impact_of` sees no
/// difference between Java's keyword-driven IMPLEMENTS, Go's structural
/// inference, and C#'s base-list disambiguation.
#[test]
fn csharp_adapter_produces_equivalent_implements_query_behavior_to_java_go_rust_and_python() {
    let csharp_repo = TempRepo::new(&[
        (
            "IShape.cs",
            "public interface IShape {\n    double Area();\n}\n",
        ),
        (
            "Circle.cs",
            "public class Circle : IShape {\n    public double Area() {\n        return Helper();\n    }\n\n    double Helper() {\n        return 0;\n    }\n}\n",
        ),
    ]);

    let csharp_result = analyze(&csharp_repo);
    let (csharp_graph, csharp_report) = Graph::build(
        csharp_result.syntax_entities.clone(),
        csharp_result.symbols.clone(),
        csharp_result.edges.clone(),
    );
    assert!(csharp_report.is_clean());

    let csharp_engine = QueryEngine::new(&csharp_graph);

    // Structural relationship: Circle satisfies IShape across files, exactly
    // as Java's cross-file explicit IMPLEMENTS, Go's cross-file structural
    // IMPLEMENTS, and Rust's/Python's cross-file IMPLEMENTS/EXTENDS all do.
    let csharp_shape = csharp_engine
        .symbols_named("IShape")
        .into_iter()
        .next()
        .expect("C# IShape symbol");
    let csharp_impact = csharp_engine.impact_of(&csharp_shape.id);
    assert!(
        csharp_impact
            .iter()
            .any(|id| csharp_graph.node(id).is_some_and(|n| n.name() == "Circle")),
        "C#: Circle should be in IShape's impact set via disambiguated IMPLEMENTS"
    );

    // Same-file call: Circle.Area calls Helper (both declared in Circle.cs,
    // matching the same-file-call limitation the Rust/Python/Go/Java
    // fixtures already document for their own adapters' scope-tree reach).
    let csharp_helper = csharp_engine
        .symbols_named("Helper")
        .into_iter()
        .next()
        .expect("C# Helper symbol");
    let csharp_callers = csharp_engine.callers_of(&csharp_helper.id);
    assert!(
        csharp_callers.iter().any(|s| s.name == "Area"),
        "C#: Area should call Helper"
    );

    for kind in [EdgeKind::Calls, EdgeKind::References, EdgeKind::Implements] {
        assert!(
            csharp_result.edges.iter().any(|e| e.kind == kind),
            "C#: expected at least one {kind:?} edge"
        );
    }
}

/// Phase 17.6 task 7: extend the fixture with a Kotlin one. Kotlin's
/// grammar makes no syntactic distinction between a class's "extends" and an
/// interface's "implements" relation -- both are just a `: SuperType` entry
/// in the same `delegation_specifiers` list -- so this fixture exercises a
/// class satisfying an interface via a bare (non-call) `: Shape` entry,
/// which `adapters::kotlin::resolve_structural` resolves into the same
/// `IMPLEMENTS` edge kind Java's explicit `implements` keyword produces, and
/// which is resolved repository-wide (`Shape` lives in a different file from
/// `Circle`) exactly like every other adapter in this suite. Also exercises
/// the cross-file `IMPORTS` path via Kotlin's explicit `import`, resolving
/// to an external-package placeholder the same way Java's own `import`
/// does (Codeate doesn't resolve Kotlin import paths against repository
/// files any more than it does Java's).
#[test]
fn kotlin_adapter_produces_equivalent_implements_query_behavior_to_java_rust_python_and_go() {
    let kotlin_repo = TempRepo::new(&[
        ("Shape.kt", "interface Shape {\n    fun area(): Double\n}\n"),
        (
            "Circle.kt",
            "import shapes.Shape\n\nclass Circle : Shape {\n    override fun area(): Double {\n        return helper()\n    }\n\n    fun helper(): Double {\n        return 0.0\n    }\n}\n",
        ),
    ]);

    let kotlin_result = analyze(&kotlin_repo);
    let (kotlin_graph, kotlin_report) = Graph::build(
        kotlin_result.syntax_entities.clone(),
        kotlin_result.symbols.clone(),
        kotlin_result.edges.clone(),
    );
    assert!(kotlin_report.is_clean());

    let kotlin_engine = QueryEngine::new(&kotlin_graph);

    // Structural relationship: Circle satisfies Shape across files, exactly
    // as Java's cross-file explicit IMPLEMENTS, Rust's cross-file IMPLEMENTS,
    // Python's cross-file EXTENDS, and Go's cross-file structural IMPLEMENTS
    // all do.
    let kotlin_shape = kotlin_engine
        .symbols_named("Shape")
        .into_iter()
        .next()
        .expect("Kotlin Shape symbol");
    let kotlin_impact = kotlin_engine.impact_of(&kotlin_shape.id);
    assert!(
        kotlin_impact
            .iter()
            .any(|id| kotlin_graph.node(id).is_some_and(|n| n.name() == "Circle")),
        "Kotlin: Circle should be in Shape's impact set via explicit IMPLEMENTS"
    );

    // Same-file call: Circle.area calls helper (both declared in Circle.kt,
    // matching the same-file-call limitation every other fixture in this
    // suite already documents for its own adapter's scope-tree reach).
    let kotlin_helper = kotlin_engine
        .symbols_named("helper")
        .into_iter()
        .next()
        .expect("Kotlin helper symbol");
    let kotlin_callers = kotlin_engine.callers_of(&kotlin_helper.id);
    assert!(
        kotlin_callers.iter().any(|s| s.name == "area"),
        "Kotlin: area should call helper"
    );

    for kind in [
        EdgeKind::Calls,
        EdgeKind::References,
        EdgeKind::Implements,
        EdgeKind::Imports,
    ] {
        assert!(
            kotlin_result.edges.iter().any(|e| e.kind == kind),
            "Kotlin: expected at least one {kind:?} edge"
        );
    }
}
