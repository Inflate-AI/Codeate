use std::collections::{HashMap, HashSet};

use crate::model::{Edge, EdgeKind, EntityId};

/// Collapse cross-file edges into file-level `DEPENDS_ON` edges.
///
/// `owner_file` maps every entity's id to the file it was declared in;
/// `module_symbol_for_file` maps each file to the `EntityId` of that file's
/// module/namespace symbol (the endpoint `DEPENDS_ON` edges connect).
/// Considers `IMPORTS`, `CALLS`, `REFERENCES`, and `INSTANTIATES` edges
/// (Phase 7/8's resolved edges); an edge whose endpoints resolve to the same
/// file, or whose endpoint isn't in `owner_file` (e.g. an unresolved
/// placeholder, which has no owning file), contributes nothing. The result
/// has at most one edge per distinct (source file, target file) pair.
pub fn resolve_depends_on(
    edges: &[Edge],
    owner_file: &HashMap<EntityId, String>,
    module_symbol_for_file: &HashMap<String, EntityId>,
) -> Vec<Edge> {
    const CONSIDERED: [EdgeKind; 4] = [
        EdgeKind::Imports,
        EdgeKind::Calls,
        EdgeKind::References,
        EdgeKind::Instantiates,
    ];

    let mut seen_pairs = HashSet::new();
    let mut depends_on = Vec::new();

    for edge in edges.iter().filter(|e| CONSIDERED.contains(&e.kind)) {
        let (Some(from_file), Some(to_file)) =
            (owner_file.get(&edge.from), owner_file.get(&edge.to))
        else {
            continue;
        };
        if from_file == to_file {
            continue;
        }
        if !seen_pairs.insert((from_file.clone(), to_file.clone())) {
            continue;
        }

        let (Some(from_module), Some(to_module)) = (
            module_symbol_for_file.get(from_file),
            module_symbol_for_file.get(to_file),
        ) else {
            continue;
        };
        depends_on.push(Edge::new(
            from_module.clone(),
            to_module.clone(),
            EdgeKind::DependsOn,
        ));
    }

    depends_on
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EntityId;

    fn id(tag: &str) -> EntityId {
        EntityId::new("Test", "test", tag, None)
    }

    #[test]
    fn a_cross_file_call_produces_one_depends_on_edge_between_the_files() {
        let caller = id("caller");
        let callee = id("callee");
        let module_a = id("module-a");
        let module_b = id("module-b");

        let owner_file: HashMap<EntityId, String> = [
            (caller.clone(), "src/a.rs".to_string()),
            (callee.clone(), "src/b.rs".to_string()),
        ]
        .into();
        let module_symbol_for_file: HashMap<String, EntityId> = [
            ("src/a.rs".to_string(), module_a.clone()),
            ("src/b.rs".to_string(), module_b.clone()),
        ]
        .into();

        let edges = vec![Edge::new(caller, callee, EdgeKind::Calls)];
        let depends_on = resolve_depends_on(&edges, &owner_file, &module_symbol_for_file);

        assert_eq!(
            depends_on,
            vec![Edge::new(module_a, module_b, EdgeKind::DependsOn)]
        );
    }

    #[test]
    fn a_same_file_call_produces_no_depends_on_edge() {
        let caller = id("caller");
        let callee = id("callee");
        let owner_file: HashMap<EntityId, String> = [
            (caller.clone(), "src/a.rs".to_string()),
            (callee.clone(), "src/a.rs".to_string()),
        ]
        .into();
        let module_symbol_for_file: HashMap<String, EntityId> =
            [("src/a.rs".to_string(), id("module-a"))].into();

        let edges = vec![Edge::new(caller, callee, EdgeKind::Calls)];
        assert!(resolve_depends_on(&edges, &owner_file, &module_symbol_for_file).is_empty());
    }

    #[test]
    fn multiple_cross_file_edges_between_the_same_pair_collapse_to_one_edge() {
        let caller = id("caller");
        let callee_1 = id("callee-1");
        let callee_2 = id("callee-2");
        let module_a = id("module-a");
        let module_b = id("module-b");

        let owner_file: HashMap<EntityId, String> = [
            (caller.clone(), "src/a.rs".to_string()),
            (callee_1.clone(), "src/b.rs".to_string()),
            (callee_2.clone(), "src/b.rs".to_string()),
        ]
        .into();
        let module_symbol_for_file: HashMap<String, EntityId> = [
            ("src/a.rs".to_string(), module_a.clone()),
            ("src/b.rs".to_string(), module_b.clone()),
        ]
        .into();

        let edges = vec![
            Edge::new(caller.clone(), callee_1, EdgeKind::Calls),
            Edge::new(caller, callee_2, EdgeKind::References),
        ];
        let depends_on = resolve_depends_on(&edges, &owner_file, &module_symbol_for_file);

        assert_eq!(
            depends_on,
            vec![Edge::new(module_a, module_b, EdgeKind::DependsOn)]
        );
    }

    #[test]
    fn an_edge_with_an_unresolved_endpoint_missing_from_owner_file_contributes_nothing() {
        let caller = id("caller");
        let unresolved = id("unresolved-placeholder");
        let owner_file: HashMap<EntityId, String> =
            [(caller.clone(), "src/a.rs".to_string())].into();
        let module_symbol_for_file: HashMap<String, EntityId> =
            [("src/a.rs".to_string(), id("module-a"))].into();

        let edges = vec![Edge::new(caller, unresolved, EdgeKind::Calls)];
        assert!(resolve_depends_on(&edges, &owner_file, &module_symbol_for_file).is_empty());
    }

    #[test]
    fn an_uninteresting_edge_kind_is_ignored_even_across_files() {
        let from = id("from");
        let to = id("to");
        let owner_file: HashMap<EntityId, String> = [
            (from.clone(), "src/a.rs".to_string()),
            (to.clone(), "src/b.rs".to_string()),
        ]
        .into();
        let module_symbol_for_file: HashMap<String, EntityId> = [
            ("src/a.rs".to_string(), id("module-a")),
            ("src/b.rs".to_string(), id("module-b")),
        ]
        .into();

        let edges = vec![Edge::new(from, to, EdgeKind::Contains)];
        assert!(resolve_depends_on(&edges, &owner_file, &module_symbol_for_file).is_empty());
    }
}
