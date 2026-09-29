use std::path::PathBuf;

use codeate_core::{Edge, EdgeKind, LanguageAdapter, RustAdapter, SyntaxEntity, SyntaxKind};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rust_adapter")
        .join(name);
    std::fs::read_to_string(path).expect("read fixture")
}

fn count(entities: &[SyntaxEntity], kind: SyntaxKind) -> usize {
    entities.iter().filter(|e| e.kind == kind).count()
}

fn find<'a>(entities: &'a [SyntaxEntity], kind: SyntaxKind, name: &str) -> &'a SyntaxEntity {
    entities
        .iter()
        .find(|e| e.kind == kind && e.name == name)
        .unwrap_or_else(|| panic!("no {kind:?} entity named {name}"))
}

/// Golden-file test: `sample.rs` exercises every declaration and body
/// extraction rule from Phase 4 tasks 3-4 together (module, struct, enum,
/// trait-as-interface, function, method, parameters, variables, a
/// statement, and a tail expression, including one level of module
/// nesting). A regression in any single extraction rule changes these
/// counts or drops an expected entity/edge.
#[test]
fn extracts_the_full_expected_syntax_tree_from_the_sample_fixture() {
    let source = fixture("sample.rs");
    let adapter = RustAdapter;
    let ast = adapter.parse(&source).expect("sample.rs is valid Rust");
    let (entities, edges) = adapter.extract(&ast, "sample.rs");

    assert_eq!(
        count(&entities, SyntaxKind::Module),
        2,
        "file module + geometry module"
    );
    assert_eq!(count(&entities, SyntaxKind::Struct), 2, "Point + Line");
    assert_eq!(count(&entities, SyntaxKind::Enum), 1, "Shape");
    assert_eq!(
        count(&entities, SyntaxKind::Interface),
        1,
        "Area (from trait)"
    );
    assert_eq!(count(&entities, SyntaxKind::Function), 1, "add");
    // Point's impl method (`magnitude`) plus Area's own declared method
    // (`area`) -- traits declare their fn items as Method entities too
    // (Phase 8's OVERRIDES resolution needs a trait-side symbol to point at).
    assert_eq!(count(&entities, SyntaxKind::Method), 2, "magnitude + area");
    assert_eq!(count(&entities, SyntaxKind::Parameter), 3, "a, b, scale");
    assert_eq!(count(&entities, SyntaxKind::Variable), 2, "sum, squared");
    assert_eq!(count(&entities, SyntaxKind::Statement), 1, "sum + 0;");
    assert_eq!(
        count(&entities, SyntaxKind::Expression),
        2,
        "tail of add, tail of magnitude"
    );

    let file_module = find(&entities, SyntaxKind::Module, "sample.rs");
    let geometry = find(&entities, SyntaxKind::Module, "geometry");
    let point = find(&entities, SyntaxKind::Struct, "Point");
    let line = find(&entities, SyntaxKind::Struct, "Line");
    let shape = find(&entities, SyntaxKind::Enum, "Shape");
    let area = find(&entities, SyntaxKind::Interface, "Area");
    let add = find(&entities, SyntaxKind::Function, "add");
    let magnitude = find(&entities, SyntaxKind::Method, "magnitude");
    let area_method = find(&entities, SyntaxKind::Method, "area");
    let param_a = find(&entities, SyntaxKind::Parameter, "a");
    let param_scale = find(&entities, SyntaxKind::Parameter, "scale");
    let sum = find(&entities, SyntaxKind::Variable, "sum");

    // File module declares every top-level item, including geometry, but
    // not geometry's own nested declarations.
    for child in [
        &geometry.id,
        &point.id,
        &shape.id,
        &area.id,
        &add.id,
        &magnitude.id,
    ] {
        assert!(edges.contains(&Edge::new(
            file_module.id.clone(),
            child.clone(),
            EdgeKind::Contains
        )));
        assert!(edges.contains(&Edge::new(
            file_module.id.clone(),
            child.clone(),
            EdgeKind::Declares
        )));
    }
    assert!(!edges.contains(&Edge::new(
        file_module.id.clone(),
        line.id.clone(),
        EdgeKind::Contains
    )));

    // geometry contains/declares Line, scoped to itself.
    assert!(edges.contains(&Edge::new(
        geometry.id.clone(),
        line.id.clone(),
        EdgeKind::Contains
    )));
    assert!(edges.contains(&Edge::new(
        geometry.id.clone(),
        line.id.clone(),
        EdgeKind::Declares
    )));

    // Area declares its own `area` method, scoped to itself -- not directly
    // to the file module (mirrors how geometry, not the file module, owns Line).
    assert!(edges.contains(&Edge::new(
        area.id.clone(),
        area_method.id.clone(),
        EdgeKind::Contains
    )));
    assert!(edges.contains(&Edge::new(
        area.id.clone(),
        area_method.id.clone(),
        EdgeKind::Declares
    )));
    assert!(!edges.contains(&Edge::new(
        file_module.id.clone(),
        area_method.id.clone(),
        EdgeKind::Contains
    )));

    // Function/method contain their parameters and body entities, but do
    // not "declare" parameters the way the module declares top-level items.
    assert!(edges.contains(&Edge::new(
        add.id.clone(),
        param_a.id.clone(),
        EdgeKind::Contains
    )));
    assert!(!edges.contains(&Edge::new(
        add.id.clone(),
        param_a.id.clone(),
        EdgeKind::Declares
    )));
    assert!(edges.contains(&Edge::new(
        add.id.clone(),
        sum.id.clone(),
        EdgeKind::Contains
    )));
    assert!(edges.contains(&Edge::new(
        magnitude.id.clone(),
        param_scale.id.clone(),
        EdgeKind::Contains
    )));

    // No stray "self" parameter entity from the method's receiver.
    assert!(!entities
        .iter()
        .any(|e| e.kind == SyntaxKind::Parameter && e.name == "self"));
}
