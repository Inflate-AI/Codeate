use codeate_core::{
    build_symbol_table, defined_in_edges, LanguageAdapter, PythonAdapter, ScopeTree, SyntaxKind,
};

fn extract(source: &str) -> (Vec<codeate_core::SyntaxEntity>, Vec<codeate_core::Edge>) {
    let adapter = PythonAdapter;
    let ast = adapter.parse(source).expect("valid Python source");
    adapter.extract(&ast, "module.py")
}

/// End-to-end: Python adapter output -> scope tree -> symbol table,
/// mirroring `tests/symbols.rs`'s Rust coverage -- proving Phase 5's
/// resolution machinery (which only ever looks at canonical `SyntaxKind`/
/// `Edge`, never a language's own AST) needs zero changes for a second
/// adapter.
#[test]
fn a_local_variable_and_a_module_level_class_are_both_independently_resolvable() {
    let source = "class Value:\n    pass\n\n\ndef run():\n    value = 1\n";
    let (entities, edges) = extract(source);

    let module = entities
        .iter()
        .find(|e| e.kind == SyntaxKind::Module)
        .unwrap();
    let run = entities
        .iter()
        .find(|e| e.kind == SyntaxKind::Function)
        .unwrap();

    let scope_tree = ScopeTree::build(module, &entities, &edges);
    let run_scope = scope_tree.scope_of(&run.id).expect("run opens a scope");

    let symbol_table = build_symbol_table(&entities);

    let value_var_id = scope_tree
        .lookup(run_scope, "value")
        .expect("value binding");
    let value_symbol = symbol_table
        .find_by_syntax_id(value_var_id)
        .expect("value symbol");
    assert_eq!(value_symbol.name, "value");

    let class_id = scope_tree
        .lookup(scope_tree.root(), "Value")
        .expect("Value binding");
    let class_symbol = symbol_table
        .find_by_syntax_id(class_id)
        .expect("Value symbol");
    assert_eq!(class_symbol.name, "Value");
    assert_ne!(value_symbol.id, class_symbol.id);
}

#[test]
fn redeclaring_a_name_in_the_same_scope_resolves_to_the_most_recent_declaration() {
    let source = "def run():\n    x = 1\n    x = 2\n";
    let (entities, edges) = extract(source);

    let module = entities
        .iter()
        .find(|e| e.kind == SyntaxKind::Module)
        .unwrap();
    let run = entities
        .iter()
        .find(|e| e.kind == SyntaxKind::Function)
        .unwrap();

    let scope_tree = ScopeTree::build(module, &entities, &edges);
    let run_scope = scope_tree.scope_of(&run.id).unwrap();

    let xs: Vec<_> = entities
        .iter()
        .filter(|e| e.kind == SyntaxKind::Variable && e.name == "x")
        .collect();
    assert_eq!(
        xs.len(),
        2,
        "both assignments to x exist as distinct entities"
    );

    let symbol_table = build_symbol_table(&entities);
    let first = symbol_table
        .find_by_syntax_id(&xs[0].id)
        .expect("first x symbol");
    let second = symbol_table
        .find_by_syntax_id(&xs[1].id)
        .expect("second x symbol");
    assert_ne!(first.id, second.id);

    let looked_up = scope_tree.lookup(run_scope, "x").expect("x binding");
    assert_eq!(looked_up, &xs[1].id);
}

/// `defined_in_edges` follows the nearest `DECLARES` edge regardless of
/// what declared it -- for Python that's routinely a `Class`, not the file
/// `Module` (Python has no nested-module-within-a-file construct the way
/// Rust's `mod` does, so a class is this adapter's equivalent proof that
/// `DEFINED_IN` isn't hardcoded to "the file's own module").
#[test]
fn a_methods_defined_in_edge_points_at_its_declaring_class_not_the_file_module() {
    let source = "class Shape:\n    def area(self):\n        return 0\n";
    let (entities, edges) = extract(source);

    let symbol_table = build_symbol_table(&entities);
    let defined_in = defined_in_edges(&symbol_table, &edges);

    let area = entities
        .iter()
        .find(|e| e.kind == SyntaxKind::Method)
        .unwrap();
    let shape = entities
        .iter()
        .find(|e| e.kind == SyntaxKind::Class && e.name == "Shape")
        .unwrap();
    let module = entities
        .iter()
        .find(|e| e.kind == SyntaxKind::Module)
        .unwrap();
    let area_symbol = symbol_table.find_by_syntax_id(&area.id).unwrap();

    assert!(defined_in
        .iter()
        .any(|edge| edge.from == area_symbol.id && edge.to == shape.id));
    assert!(!defined_in
        .iter()
        .any(|edge| edge.from == area_symbol.id && edge.to == module.id));
}
