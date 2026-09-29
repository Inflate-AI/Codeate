use codeate_core::{Edge, EdgeKind, EntityId, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};

#[test]
fn entity_ids_are_stable_across_independent_construction() {
    // Two independently-built entities describing the same logical thing
    // (same kind, location, name) must resolve to the same ID -- this is
    // what lets Phase 9's graph builder and Phase 10's state store treat
    // them as the same node across separate analysis runs.
    let a = SyntaxEntity::new(SyntaxKind::Class, "src/lib.rs", "Dog");
    let b = SyntaxEntity::new(SyntaxKind::Class, "src/lib.rs", "Dog");
    assert_eq!(a.id, b.id);
}

#[test]
fn syntax_and_symbol_entities_of_the_same_name_get_distinct_ids() {
    // A SyntaxEntity::Function and a SymbolEntity::Function with the same
    // location and name are different things in the model (AST shape vs.
    // resolved symbol) and must not collide on identity.
    let syntax_fn = SyntaxEntity::new(SyntaxKind::Function, "src/lib.rs", "run");
    let symbol_fn = SymbolEntity::new(SymbolKind::Function, "src/lib.rs", "run");

    assert_ne!(syntax_fn.id, symbol_fn.id);
}

#[test]
fn every_edge_kind_is_one_of_the_fourteen_closed_variants() {
    let from = EntityId::new("Class", "src/lib.rs", "Dog", None);
    let to = EntityId::new("Interface", "src/lib.rs", "Animal", None);

    let all_kinds = [
        EdgeKind::Contains,
        EdgeKind::Imports,
        EdgeKind::Exports,
        EdgeKind::Declares,
        EdgeKind::References,
        EdgeKind::Calls,
        EdgeKind::Extends,
        EdgeKind::Implements,
        EdgeKind::Overrides,
        EdgeKind::Instantiates,
        EdgeKind::Returns,
        EdgeKind::Accepts,
        EdgeKind::DependsOn,
        EdgeKind::DefinedIn,
    ];
    assert_eq!(all_kinds.len(), 14);

    for kind in all_kinds {
        let edge = Edge::new(from.clone(), to.clone(), kind);
        // Every edge's kind round-trips through the same stable string form
        // an on-disk graph (Phase 10) would persist.
        assert_eq!(EdgeKind::parse(edge.kind.as_str()), Some(kind));
    }
}

#[test]
fn a_syntax_entity_can_be_linked_to_its_symbol_via_a_defined_in_edge() {
    // Even though their IDs are distinct, the model can still relate a
    // syntax node to the symbol it declares via an explicit edge -- the
    // "unless explicitly linked" escape hatch mentioned in the task.
    let syntax_fn = SyntaxEntity::new(SyntaxKind::Function, "src/lib.rs", "run");
    let symbol_fn = SymbolEntity::new(SymbolKind::Function, "src/lib.rs", "run");

    let edge = Edge::new(symbol_fn.id.clone(), syntax_fn.id.clone(), EdgeKind::DefinedIn);

    assert_eq!(edge.from, symbol_fn.id);
    assert_eq!(edge.to, syntax_fn.id);
    assert_eq!(edge.kind, EdgeKind::DefinedIn);
}
