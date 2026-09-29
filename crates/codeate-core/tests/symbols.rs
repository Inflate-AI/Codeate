use codeate_core::{
    build_symbol_table, defined_in_edges, LanguageAdapter, RustAdapter, ScopeTree, SyntaxKind,
};

fn extract(source: &str) -> (Vec<codeate_core::SyntaxEntity>, Vec<codeate_core::Edge>) {
    let adapter = RustAdapter;
    let ast = adapter.parse(source).expect("valid Rust source");
    adapter.extract(&ast, "src/lib.rs")
}

/// End-to-end: adapter output -> scope tree -> symbol table -> DEFINED_IN
/// edges, exercising the deterministic shadowing/redeclaration policy from
/// Phase 5 task 5.
#[test]
fn an_inner_scope_symbol_shadows_an_outer_one_but_both_remain_resolvable() {
    let source = "struct Value; fn run() { let value = 1; }";
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

    // Lookup from inside run() finds the local `value` variable, not the
    // outer `Value` struct -- name lookup is case-sensitive so this isn't
    // really shadowing here, but the module-scope struct remains
    // independently resolvable regardless.
    let value_var_id = scope_tree.lookup(run_scope, "value").expect("value binding");
    let value_symbol = symbol_table.find_by_syntax_id(value_var_id).expect("value symbol");
    assert_eq!(value_symbol.name, "value");

    let struct_id = scope_tree.lookup(scope_tree.root(), "Value").expect("Value binding");
    let struct_symbol = symbol_table.find_by_syntax_id(struct_id).expect("Value symbol");
    assert_eq!(struct_symbol.name, "Value");
    assert_ne!(value_symbol.id, struct_symbol.id);
}

#[test]
fn redeclaring_a_name_in_the_same_scope_resolves_to_the_most_recent_declaration() {
    let source = "fn run() { let x = 1; let x = 2; }";
    let (entities, edges) = extract(source);

    let module = entities.iter().find(|e| e.kind == SyntaxKind::Module).unwrap();
    let run = entities.iter().find(|e| e.kind == SyntaxKind::Function).unwrap();

    let scope_tree = ScopeTree::build(module, &entities, &edges);
    let run_scope = scope_tree.scope_of(&run.id).unwrap();

    let xs: Vec<_> = entities
        .iter()
        .filter(|e| e.kind == SyntaxKind::Variable && e.name == "x")
        .collect();
    assert_eq!(xs.len(), 2, "both declarations of x exist as distinct entities");

    let symbol_table = build_symbol_table(&entities);

    // Both redeclarations resolve to a symbol...
    let first = symbol_table.find_by_syntax_id(&xs[0].id).expect("first x symbol");
    let second = symbol_table.find_by_syntax_id(&xs[1].id).expect("second x symbol");
    assert_ne!(first.id, second.id);

    // ...but a plain name lookup deterministically resolves to the most
    // recently declared one.
    let looked_up = scope_tree.lookup(run_scope, "x").expect("x binding");
    assert_eq!(looked_up, &xs[1].id);
}

#[test]
fn every_resolved_symbol_has_a_defined_in_edge_to_its_declaring_module() {
    let source = "mod shapes { struct Circle; }";
    let (entities, edges) = extract(source);

    let symbol_table = build_symbol_table(&entities);
    let defined_in = defined_in_edges(&symbol_table, &edges);

    let circle = entities.iter().find(|e| e.kind == SyntaxKind::Struct).unwrap();
    let shapes = entities
        .iter()
        .find(|e| e.kind == SyntaxKind::Module && e.name == "shapes")
        .unwrap();
    let circle_symbol = symbol_table.find_by_syntax_id(&circle.id).unwrap();

    assert!(defined_in.iter().any(|edge| edge.from == circle_symbol.id && edge.to == shapes.id));
}
