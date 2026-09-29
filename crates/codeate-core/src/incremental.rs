use std::collections::{HashMap, HashSet};

use crate::graph::Graph;
use crate::model::{EdgeKind, EntityId};
use crate::repository::File;

/// The result of comparing a repository's current file set against a
/// previously-analyzed one: which files are new, which changed content
/// (same path, different hash), and which disappeared. Compared by path +
/// content hash from Phase 1 -- no re-parsing needed to detect a change.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    pub added: Vec<File>,
    pub modified: Vec<File>,
    pub removed: Vec<String>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }

    /// Every path this change touches, added or modified -- the starting
    /// point for affected-structure computation. Removed paths are not
    /// included: there is nothing left to re-analyze there, only stale data
    /// to drop.
    pub fn changed_paths(&self) -> HashSet<String> {
        self.added
            .iter()
            .chain(self.modified.iter())
            .map(|f| f.path.clone())
            .collect()
    }
}

/// Compare `previous` (the file set from a prior analysis) against `current`
/// (a fresh ingest), by path and content hash.
pub fn detect_changes(previous: &[File], current: &[File]) -> ChangeSet {
    let previous_by_path: HashMap<&str, &File> =
        previous.iter().map(|f| (f.path.as_str(), f)).collect();
    let current_by_path: HashMap<&str, &File> =
        current.iter().map(|f| (f.path.as_str(), f)).collect();

    let mut added = Vec::new();
    let mut modified = Vec::new();
    for file in current {
        match previous_by_path.get(file.path.as_str()) {
            None => added.push(file.clone()),
            Some(prior) if prior.content_hash != file.content_hash => modified.push(file.clone()),
            Some(_) => {}
        }
    }

    let mut removed: Vec<String> = previous
        .iter()
        .filter(|f| !current_by_path.contains_key(f.path.as_str()))
        .map(|f| f.path.clone())
        .collect();

    added.sort_by(|a, b| a.path.cmp(&b.path));
    modified.sort_by(|a, b| a.path.cmp(&b.path));
    removed.sort();

    ChangeSet {
        added,
        modified,
        removed,
    }
}

/// Every `EntityId` (syntax entity or symbol) owned by one of `paths`,
/// according to `owner_file`.
pub fn entities_owned_by(
    owner_file: &HashMap<EntityId, String>,
    paths: &HashSet<String>,
) -> HashSet<EntityId> {
    owner_file
        .iter()
        .filter(|(_, file)| paths.contains(file.as_str()))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Edge kinds that carry a change's "blast radius" across file boundaries:
/// if a file changes (or disappears), anything that references, imports,
/// calls, or otherwise structurally depends on one of its entities may
/// resolve differently now and must be re-analyzed too. Every semantic edge
/// kind is considered here (not just `IMPORTS`/`DEPENDS_ON`) since a
/// cross-file `IMPLEMENTS`/`EXTENDS`/`CALLS`/... edge can exist without a
/// `DEPENDS_ON` edge yet having been derived from it.
const CROSS_FILE_EDGE_KINDS: [EdgeKind; 11] = [
    EdgeKind::Imports,
    EdgeKind::Exports,
    EdgeKind::References,
    EdgeKind::Calls,
    EdgeKind::Extends,
    EdgeKind::Implements,
    EdgeKind::Overrides,
    EdgeKind::Instantiates,
    EdgeKind::Returns,
    EdgeKind::Accepts,
    EdgeKind::DependsOn,
];

/// The full set of files that must be re-analyzed given `changes`: every
/// added or modified file, plus every file that (per `graph` and
/// `owner_file`) imports from or depends on an entity owned by a changed or
/// removed file -- since that file's resolution may now be different even
/// though its own content didn't change. Removed files themselves are never
/// included (there is no file left to parse).
pub fn affected_files(
    changes: &ChangeSet,
    graph: &Graph,
    owner_file: &HashMap<EntityId, String>,
) -> HashSet<String> {
    let touched: HashSet<String> = changes
        .added
        .iter()
        .map(|f| f.path.clone())
        .chain(changes.modified.iter().map(|f| f.path.clone()))
        .chain(changes.removed.iter().cloned())
        .collect();

    let touched_entities = entities_owned_by(owner_file, &touched);

    let mut affected = changes.changed_paths();
    for entity_id in &touched_entities {
        for kind in CROSS_FILE_EDGE_KINDS {
            for edge in graph.incoming_of_kind(entity_id, kind) {
                if let Some(dependent_file) = owner_file.get(&edge.from) {
                    if !changes.removed.contains(dependent_file) {
                        affected.insert(dependent_file.clone());
                    }
                }
            }
        }
    }

    affected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, hash: &str) -> File {
        File::new(path, hash, 0)
    }

    #[test]
    fn a_file_present_only_in_current_is_added() {
        let previous = vec![file("a.rs", "h1")];
        let current = vec![file("a.rs", "h1"), file("b.rs", "h2")];

        let changes = detect_changes(&previous, &current);

        assert_eq!(changes.added, vec![file("b.rs", "h2")]);
        assert!(changes.modified.is_empty());
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn a_file_with_a_changed_hash_is_modified() {
        let previous = vec![file("a.rs", "h1")];
        let current = vec![file("a.rs", "h1-changed")];

        let changes = detect_changes(&previous, &current);

        assert_eq!(changes.modified, vec![file("a.rs", "h1-changed")]);
        assert!(changes.added.is_empty());
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn a_file_present_only_in_previous_is_removed() {
        let previous = vec![file("a.rs", "h1"), file("b.rs", "h2")];
        let current = vec![file("a.rs", "h1")];

        let changes = detect_changes(&previous, &current);

        assert_eq!(changes.removed, vec!["b.rs".to_string()]);
        assert!(changes.added.is_empty());
        assert!(changes.modified.is_empty());
    }

    #[test]
    fn identical_file_sets_produce_no_changes() {
        let files = vec![file("a.rs", "h1"), file("b.rs", "h2")];

        let changes = detect_changes(&files, &files);

        assert!(changes.is_empty());
    }

    #[test]
    fn changed_paths_covers_added_and_modified_but_not_removed() {
        let previous = vec![file("a.rs", "h1"), file("c.rs", "h3")];
        let current = vec![file("a.rs", "h1-changed"), file("b.rs", "h2")];

        let changes = detect_changes(&previous, &current);
        let paths = changes.changed_paths();

        assert_eq!(paths.len(), 2);
        assert!(paths.contains("a.rs"));
        assert!(paths.contains("b.rs"));
    }

    #[test]
    fn results_are_sorted_by_path_regardless_of_input_order() {
        let previous = vec![file("z.rs", "h1"), file("a.rs", "h2")];
        let current = vec![file("m.rs", "h3"), file("b.rs", "h4")];

        let changes = detect_changes(&previous, &current);

        assert_eq!(
            changes
                .added
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["b.rs", "m.rs"]
        );
        assert_eq!(
            changes.removed,
            vec!["a.rs".to_string(), "z.rs".to_string()]
        );
    }

    #[test]
    fn affected_files_includes_changed_files_and_their_dependents() {
        use crate::graph::Graph;
        use crate::model::{Edge, SymbolEntity, SymbolKind};

        let shape = SymbolEntity::new(SymbolKind::Type, "shape.rs", "Shape");
        let circle = SymbolEntity::new(SymbolKind::Type, "circle.rs", "Circle");
        let unrelated = SymbolEntity::new(SymbolKind::Type, "other.rs", "Other");
        let edge = Edge::new(circle.id.clone(), shape.id.clone(), EdgeKind::Imports);

        let (graph, _) = Graph::build(
            vec![],
            vec![shape.clone(), circle.clone(), unrelated.clone()],
            vec![edge],
        );

        let mut owner_file = HashMap::new();
        owner_file.insert(shape.id.clone(), "shape.rs".to_string());
        owner_file.insert(circle.id.clone(), "circle.rs".to_string());
        owner_file.insert(unrelated.id.clone(), "other.rs".to_string());

        let changes = ChangeSet {
            added: vec![],
            modified: vec![file("shape.rs", "h2")],
            removed: vec![],
        };

        let affected = affected_files(&changes, &graph, &owner_file);

        assert!(affected.contains("shape.rs"));
        assert!(affected.contains("circle.rs"));
        assert!(!affected.contains("other.rs"));
    }

    #[test]
    fn affected_files_excludes_removed_files_themselves() {
        use crate::graph::Graph;

        let (graph, _) = Graph::build(vec![], vec![], vec![]);
        let owner_file = HashMap::new();

        let changes = ChangeSet {
            added: vec![],
            modified: vec![],
            removed: vec!["gone.rs".to_string()],
        };

        let affected = affected_files(&changes, &graph, &owner_file);

        assert!(!affected.contains("gone.rs"));
        assert!(affected.is_empty());
    }

    #[test]
    fn entities_owned_by_filters_the_ownership_map_to_the_given_paths() {
        let id_a = EntityId::new("Struct", "a.rs", "A", None);
        let id_b = EntityId::new("Struct", "b.rs", "B", None);
        let mut owner_file = HashMap::new();
        owner_file.insert(id_a.clone(), "a.rs".to_string());
        owner_file.insert(id_b.clone(), "b.rs".to_string());

        let paths: HashSet<String> = ["a.rs".to_string()].into_iter().collect();
        let owned = entities_owned_by(&owner_file, &paths);

        assert_eq!(owned, [id_a].into_iter().collect());
    }
}
