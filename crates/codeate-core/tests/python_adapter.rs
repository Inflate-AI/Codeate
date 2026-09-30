use std::path::PathBuf;

use codeate_core::{Edge, EdgeKind, LanguageAdapter, PythonAdapter, SyntaxEntity, SyntaxKind};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/python_adapter")
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

/// Golden-file test mirroring `tests/rust_adapter.rs`: `sample.py` exercises
/// every declaration and body extraction rule the Python adapter implements
/// together (a base class, a subclass overriding a method, a top-level
/// function, parameters with `self` excluded, a variable assignment, and
/// plain statements). A regression in any single extraction rule changes
/// these counts or drops an expected entity/edge.
#[test]
fn extracts_the_full_expected_syntax_tree_from_the_sample_fixture() {
    let source = fixture("sample.py");
    let adapter = PythonAdapter;
    let ast = adapter.parse(&source).expect("sample.py is valid Python");
    let (entities, edges) = adapter.extract(&ast, "sample.py");

    assert_eq!(count(&entities, SyntaxKind::Module), 1, "the file itself");
    assert_eq!(count(&entities, SyntaxKind::Class), 2, "Shape + Circle");
    assert_eq!(
        count(&entities, SyntaxKind::Method),
        2,
        "Shape.area + Circle.area"
    );
    assert_eq!(count(&entities, SyntaxKind::Function), 1, "add");
    assert_eq!(count(&entities, SyntaxKind::Parameter), 3, "a, b, scale");
    assert_eq!(count(&entities, SyntaxKind::Variable), 1, "total");
    assert_eq!(
        count(&entities, SyntaxKind::Statement),
        4,
        "return 0; print(total); return total; return a + b"
    );

    let file_module = find(&entities, SyntaxKind::Module, "sample.py");
    let shape = find(&entities, SyntaxKind::Class, "Shape");
    let circle = find(&entities, SyntaxKind::Class, "Circle");
    let add = find(&entities, SyntaxKind::Function, "add");
    let param_a = find(&entities, SyntaxKind::Parameter, "a");
    let param_scale = find(&entities, SyntaxKind::Parameter, "scale");
    let total = find(&entities, SyntaxKind::Variable, "total");

    let methods: Vec<&SyntaxEntity> = entities
        .iter()
        .filter(|e| e.kind == SyntaxKind::Method && e.name == "area")
        .collect();
    assert_eq!(methods.len(), 2);
    assert_ne!(
        methods[0].id, methods[1].id,
        "Shape.area and Circle.area must get distinct ids"
    );

    // File module declares every top-level item.
    for child in [&shape.id, &circle.id, &add.id] {
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

    // Each class declares its own method, scoped to itself.
    for (class, method) in [(&shape.id, methods[0]), (&circle.id, methods[1])] {
        assert!(edges.contains(&Edge::new(
            class.clone(),
            method.id.clone(),
            EdgeKind::Declares
        )));
    }

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
        methods[1].id.clone(),
        param_scale.id.clone(),
        EdgeKind::Contains
    )));
    assert!(edges.contains(&Edge::new(
        methods[1].id.clone(),
        total.id.clone(),
        EdgeKind::Contains
    )));

    // No stray "self" parameter entity from either method's receiver.
    assert!(!entities
        .iter()
        .any(|e| e.kind == SyntaxKind::Parameter && e.name == "self"));
}
