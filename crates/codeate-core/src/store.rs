use std::collections::HashMap;
use std::path::Path;

use crate::error::{Error, Result};
use crate::graph::{diff, Graph, GraphDiff, IntegrityReport};
use crate::model::{Edge, EntityId, SymbolEntity, SyntaxEntity};
use crate::pipeline::{analyze_incremental, AnalysisResult};
use crate::repository::{File, Repository};

/// The current on-disk schema version. Bump this whenever `StoredGraph`'s
/// shape changes in a way that breaks reading an older file, and extend
/// `load`'s version check accordingly rather than silently misreading old
/// data.
///
/// v2 (Phase 13) added `files` and `owner_file` so incremental analysis can
/// detect changes and find what a changed file's data to invalidate without
/// re-parsing everything.
pub const CURRENT_VERSION: u32 = 2;

/// The full Code Graph in its on-disk form: every syntax entity, symbol, and
/// edge from a `pipeline::AnalysisResult`, plus an explicit schema version.
/// JSON via `serde_json` -- human-inspectable, and doubles as the "export to
/// JSON" operation with no separate format to maintain.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredGraph {
    pub version: u32,
    pub syntax_entities: Vec<SyntaxEntity>,
    pub symbols: Vec<SymbolEntity>,
    pub edges: Vec<Edge>,
    /// The repository's files (path, content hash) as of this analysis --
    /// Phase 13's change detection compares a fresh ingest against this.
    #[serde(default)]
    pub files: Vec<File>,
    /// Which file each syntax entity / symbol came from.
    #[serde(default)]
    pub owner_file: HashMap<EntityId, String>,
}

impl StoredGraph {
    pub fn from_analysis(result: &AnalysisResult, files: &[File]) -> Self {
        Self {
            version: CURRENT_VERSION,
            syntax_entities: result.syntax_entities.clone(),
            symbols: result.symbols.clone(),
            edges: result.edges.clone(),
            files: files.to_vec(),
            owner_file: result.owner_file.clone(),
        }
    }

    /// Assemble this stored state back into an in-memory `Graph`, re-running
    /// Phase 9's integrity checks (never assumed still valid just because it
    /// was persisted).
    pub fn into_graph(self) -> (Graph, IntegrityReport) {
        Graph::build(self.syntax_entities, self.symbols, self.edges)
    }

    /// This state's contents as an `AnalysisResult`, for feeding back into
    /// `analyze_incremental`.
    fn as_analysis_result(&self) -> AnalysisResult {
        AnalysisResult {
            syntax_entities: self.syntax_entities.clone(),
            symbols: self.symbols.clone(),
            edges: self.edges.clone(),
            owner_file: self.owner_file.clone(),
        }
    }

    /// Patch this stored state against `repository`'s current contents:
    /// re-analyze only what changed (Phase 13's `analyze_incremental`),
    /// drop everything owned by removed files, merge in the rest, and
    /// re-run Phase 9's integrity checks on the result before it's
    /// considered valid. Equivalent to a full re-`analyze` + `from_analysis`
    /// for the same `repository`, just without redoing unaffected work.
    pub fn update_incremental(&self, repository: &Repository) -> Result<(Self, IntegrityReport)> {
        let previous_result = self.as_analysis_result();
        let updated_result = analyze_incremental(repository, &self.files, &previous_result)?;
        let updated = Self::from_analysis(&updated_result, &repository.files);

        let (_, report) = Graph::build(
            updated.syntax_entities.clone(),
            updated.symbols.clone(),
            updated.edges.clone(),
        );

        Ok((updated, report))
    }
}

/// Serialize `stored` to `path` as pretty-printed JSON.
pub fn save(path: &Path, stored: &StoredGraph) -> Result<()> {
    let json = serde_json::to_string_pretty(stored)
        .map_err(|err| Error::Message(format!("failed to serialize graph: {err}")))?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Load a previously-`save`d graph from `path`. Rejects a file whose
/// `version` is newer than this build's `CURRENT_VERSION` (a forward
/// compatibility hazard) with a clear error rather than misreading it;
/// migrating an older, still-supported version forward is a later task's
/// concern once a version bump actually happens.
pub fn load(path: &Path) -> Result<StoredGraph> {
    let text = std::fs::read_to_string(path)?;
    let stored: StoredGraph = serde_json::from_str(&text).map_err(|err| {
        Error::Message(format!(
            "failed to parse stored graph at {}: {err}",
            path.display()
        ))
    })?;
    if stored.version > CURRENT_VERSION {
        return Err(Error::Message(format!(
            "stored graph at {} is version {}, but this build only supports up to version {}",
            path.display(),
            stored.version,
            CURRENT_VERSION
        )));
    }
    Ok(stored)
}

/// Compare two previously-saved states, loading each and diffing the graphs
/// they build into.
pub fn compare(a_path: &Path, b_path: &Path) -> Result<GraphDiff> {
    let (a_graph, _) = load(a_path)?.into_graph();
    let (b_graph, _) = load(b_path)?.into_graph();
    Ok(diff(&a_graph, &b_graph))
}

/// Export a graph to GraphML (http://graphml.graphdrawing.org/), for
/// external tools (Gephi, yEd, ...) rather than round-tripping back into
/// Codeate itself -- that's what `StoredGraph`'s own JSON form is for.
pub fn export_graphml(graph: &Graph) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\">\n");
    out.push_str("  <key id=\"kind\" for=\"node\" attr.name=\"kind\" attr.type=\"string\"/>\n");
    out.push_str("  <key id=\"name\" for=\"node\" attr.name=\"name\" attr.type=\"string\"/>\n");
    out.push_str("  <key id=\"edgekind\" for=\"edge\" attr.name=\"kind\" attr.type=\"string\"/>\n");
    out.push_str("  <graph id=\"codeate\" edgedefault=\"directed\">\n");

    let mut node_ids: Vec<&crate::model::EntityId> = graph.node_ids().collect();
    node_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    for id in node_ids {
        let node = graph.node(id).expect("id came from graph.node_ids()");
        let (kind_ns, kind) = match node {
            crate::graph::Node::Syntax(entity) => ("syntax", entity.kind.as_str()),
            crate::graph::Node::Symbol(entity) => ("symbol", entity.kind.as_str()),
        };
        out.push_str(&format!(
            "    <node id=\"{}\">\n      <data key=\"kind\">{}:{}</data>\n      <data key=\"name\">{}</data>\n    </node>\n",
            xml_escape(id.as_str()),
            kind_ns,
            xml_escape(kind),
            xml_escape(node.name()),
        ));
    }

    let mut edges: Vec<&Edge> = graph.all_edges().collect();
    edges.sort_by(|a, b| {
        (a.from.as_str(), a.kind.as_str(), a.to.as_str()).cmp(&(
            b.from.as_str(),
            b.kind.as_str(),
            b.to.as_str(),
        ))
    });
    for (index, edge) in edges.iter().enumerate() {
        out.push_str(&format!(
            "    <edge id=\"e{index}\" source=\"{}\" target=\"{}\">\n      <data key=\"edgekind\">{}</data>\n    </edge>\n",
            xml_escape(edge.from.as_str()),
            xml_escape(edge.to.as_str()),
            xml_escape(edge.kind.as_str()),
        ));
    }

    out.push_str("  </graph>\n");
    out.push_str("</graphml>\n");
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EdgeKind, EntityId, SyntaxKind};

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "codeate-store-test-{}-{}-{name}",
            std::process::id(),
            unique_suffix()
        ))
    }

    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn sample() -> StoredGraph {
        let module = SyntaxEntity::new(SyntaxKind::Module, "src/lib.rs", "src/lib.rs");
        let point = SyntaxEntity::new(SyntaxKind::Struct, "src/lib.rs", "Point");
        let edge = Edge::new(module.id.clone(), point.id.clone(), EdgeKind::Contains);
        StoredGraph {
            version: CURRENT_VERSION,
            syntax_entities: vec![module, point],
            symbols: vec![],
            edges: vec![edge],
            files: vec![],
            owner_file: HashMap::new(),
        }
    }

    struct TempRepo {
        root: std::path::PathBuf,
    }

    impl TempRepo {
        fn new(files: &[(&str, &str)]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "codeate-store-repo-test-{}-{}",
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

        fn hashed(&self) -> Repository {
            let repository = crate::ingest::ingest(&self.root).expect("ingest succeeds");
            let registry = crate::detect::default_registry();
            crate::detect::detect_languages(&repository, &registry).expect("detect succeeds")
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn update_incremental_patches_and_reintegrity_checks_a_stored_graph() {
        let repo = TempRepo::new(&[("src/lib.rs", "struct Point { x: i32 }")]);
        let repository = repo.hashed();

        let analysis = crate::pipeline::analyze_repository(&repository).expect("analyze");
        let stored = StoredGraph::from_analysis(&analysis, &repository.files);

        std::fs::write(
            repo.root.join("src/lib.rs"),
            "struct Point { x: i32 }\nstruct Line { a: i32 }",
        )
        .expect("rewrite lib.rs");
        let updated_repository = repo.hashed();

        let (patched, report) = stored
            .update_incremental(&updated_repository)
            .expect("update_incremental succeeds");

        assert!(report.is_clean());
        assert!(patched
            .syntax_entities
            .iter()
            .any(|e| e.kind == SyntaxKind::Struct && e.name == "Line"));

        let full_rebuild = crate::pipeline::analyze_repository(&updated_repository)
            .expect("full rebuild succeeds");
        let full_stored = StoredGraph::from_analysis(&full_rebuild, &updated_repository.files);
        let (patched_graph, _) = patched.into_graph();
        let (full_graph, _) = full_stored.into_graph();
        assert_eq!(patched_graph.canonical_form(), full_graph.canonical_form());
    }

    #[test]
    fn save_then_load_round_trips_exactly() {
        let path = temp_path("roundtrip.json");
        let stored = sample();

        save(&path, &stored).expect("save succeeds");
        let loaded = load(&path).expect("load succeeds");

        assert_eq!(loaded, stored);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_round_tripped_graph_builds_the_same_graph_as_the_original() {
        let stored = sample();
        let (original_graph, _) = stored.clone().into_graph();

        let path = temp_path("roundtrip-graph.json");
        save(&path, &stored).expect("save succeeds");
        let (loaded_graph, report) = load(&path).expect("load succeeds").into_graph();

        assert_eq!(
            original_graph.canonical_form(),
            loaded_graph.canonical_form()
        );
        assert!(report.is_clean());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn files_and_owner_file_round_trip_through_save_and_load() {
        let path = temp_path("files-roundtrip.json");
        let mut stored = sample();
        stored.files = vec![File::new("src/lib.rs", "abc123", 42)];
        stored.owner_file.insert(
            stored.syntax_entities[0].id.clone(),
            "src/lib.rs".to_string(),
        );

        save(&path, &stored).expect("save succeeds");
        let loaded = load(&path).expect("load succeeds");

        assert_eq!(loaded, stored);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn loading_a_v1_file_with_no_files_or_owner_file_defaults_them_to_empty() {
        let path = temp_path("v1-legacy.json");
        let legacy_json = r#"{
            "version": 1,
            "syntax_entities": [],
            "symbols": [],
            "edges": []
        }"#;
        std::fs::write(&path, legacy_json).expect("write legacy json");

        let loaded = load(&path).expect("load succeeds despite missing fields");

        assert!(loaded.files.is_empty());
        assert!(loaded.owner_file.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn loading_a_newer_unsupported_version_is_a_clear_error() {
        let path = temp_path("future-version.json");
        let mut stored = sample();
        stored.version = CURRENT_VERSION + 1;
        save(&path, &stored).expect("save succeeds");

        let err = load(&path).expect_err("newer version should be rejected");
        assert!(matches!(err, Error::Message(_)));
        assert!(err.to_string().contains("version"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn loading_a_missing_file_is_a_clear_error() {
        let path = temp_path("does-not-exist.json");
        let err = load(&path).expect_err("missing file should error");
        assert!(matches!(err, Error::Io(_)));
    }

    #[test]
    fn compare_diffs_two_saved_states() {
        let a_path = temp_path("a.json");
        let b_path = temp_path("b.json");

        let a = sample();
        save(&a_path, &a).expect("save a");

        let extra = SyntaxEntity::new(SyntaxKind::Struct, "src/lib.rs", "Line");
        let mut b = a.clone();
        b.syntax_entities.push(extra.clone());
        save(&b_path, &b).expect("save b");

        let diff = compare(&a_path, &b_path).expect("compare succeeds");
        assert_eq!(diff.added_nodes, vec![extra.id]);
        assert!(diff.removed_nodes.is_empty());

        let _ = std::fs::remove_file(&a_path);
        let _ = std::fs::remove_file(&b_path);
    }

    #[test]
    fn graphml_export_contains_every_node_and_edge() {
        let stored = sample();
        let (graph, _) = stored.into_graph();

        let xml = export_graphml(&graph);

        assert!(xml.starts_with("<?xml"));
        assert_eq!(xml.matches("<node ").count(), 2);
        assert_eq!(xml.matches("<edge ").count(), 1);
        assert!(xml.contains("Point"));
        assert!(xml.contains("CONTAINS"));
    }

    #[test]
    fn graphml_export_escapes_special_characters_in_names() {
        let weird = SyntaxEntity::new(SyntaxKind::Struct, "src/lib.rs", "A<B>&\"C\"");
        let (graph, _) = Graph::build(vec![weird], vec![], vec![]);

        let xml = export_graphml(&graph);

        assert!(!xml.contains("A<B>&\"C\""));
        assert!(xml.contains("A&lt;B&gt;&amp;&quot;C&quot;"));
    }

    #[test]
    fn entity_id_round_trips_through_json() {
        let id = EntityId::new("Struct", "src/lib.rs", "Point", None);
        let json = serde_json::to_string(&id).unwrap();
        let back: EntityId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
