//! Codeate's public API/SDK layer (Phase 16): a stable, ergonomic entry
//! point for external consumers (IDE tools, code agents, other developer
//! systems) to analyze a repository and run Phase 11/14's deterministic
//! queries against it, without needing to know `codeate-core`'s internal
//! ingest -> detect -> analyze -> persist pipeline.
//!
//! # Versioning policy
//!
//! This crate's own version (`codeate-api`'s `Cargo.toml`, independent of
//! the workspace-shared version `codeate-core`/`codeate-cli` use) is the
//! SemVer contract: the [`Codebase`] type and every method/type reachable
//! from it are covered. A breaking change to any of them is a major-version
//! bump of `codeate-api`.
//!
//! `codeate-core`'s own internals -- its module layout, anything not
//! re-exported here, `Graph`'s or `QueryEngine`'s exact shape -- are **not**
//! covered and may change freely between `codeate-api` releases; this crate
//! exists specifically so a consumer never needs to depend on `codeate-core`
//! directly to get a stable contract.
//!
//! # Contract for an LLM/agent consumer
//!
//! Every query here returns deterministic facts read from an already-built
//! graph -- the same repository state always produces the same answer, and
//! nothing here performs inference, ranking, or probabilistic reasoning.
//! An agent should treat a query result as ground truth about the
//! repository's *structure* (what calls what, what depends on what, what
//! implements what), not as a suggestion or a summary. Call [`Codebase::refresh`]
//! after the repository's files change on disk; results before a refresh
//! reflect the repository as of the last `open`/`refresh` call, not
//! necessarily its current on-disk state.
//!
//! # Example
//!
//! ```no_run
//! let codebase = codeate_api::Codebase::open(".")?;
//! for symbol in codebase.symbol("UserRepository") {
//!     println!("{}  {}", symbol.id.as_str(), symbol.name);
//!     for caller in codebase.callers(&symbol.id) {
//!         println!("  called by {}", caller.name);
//!     }
//! }
//! # Ok::<(), codeate_api::Error>(())
//! ```

use std::path::{Path, PathBuf};

use codeate_core::{Graph, QueryEngine, Repository, StoredGraph};

pub use codeate_core::{
    default_registry, detect_languages, ingest, EdgeKind, EntityId, Error, IntegrityReport,
    LanguageId, ModuleTree, Result, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind,
};

/// A repository, analyzed and ready to query. The stable entry point this
/// crate exists to provide -- open once, query as many times as needed,
/// `refresh` after the repository's files change.
pub struct Codebase {
    root: PathBuf,
    store_path: PathBuf,
    repository: Repository,
    stored: StoredGraph,
    graph: Graph,
}

impl Codebase {
    /// Open (and analyze) the repository at `root`. If a graph already
    /// exists at `root/.codeate/graph.json` (from a prior `open`/`refresh`,
    /// or from `codeate analyze`), it's patched incrementally; otherwise a
    /// full analysis runs. Either way, the result is persisted back to that
    /// path before returning.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let store_path = default_store_path(&root);
        let repository = ingest_and_detect(&root)?;

        let stored = if store_path.exists() {
            let previous = codeate_core::store::load(&store_path)?;
            update_incremental_checked(&previous, &repository)?
        } else {
            let analysis = codeate_core::analyze_repository_state(&repository)?;
            StoredGraph::from_analysis(&analysis, &repository.files)
        };

        persist(&store_path, &stored)?;
        let (graph, _report) = stored.clone().into_graph();

        Ok(Self {
            root,
            store_path,
            repository,
            stored,
            graph,
        })
    }

    /// Re-ingest the repository's current on-disk contents and re-analyze
    /// (incrementally, against this `Codebase`'s own last-known state) and
    /// persist the result. Every query method reflects the refreshed state
    /// afterward.
    pub fn refresh(&mut self) -> Result<()> {
        let repository = ingest_and_detect(&self.root)?;
        let stored = update_incremental_checked(&self.stored, &repository)?;
        persist(&self.store_path, &stored)?;
        let (graph, _report) = stored.clone().into_graph();

        self.repository = repository;
        self.stored = stored;
        self.graph = graph;
        Ok(())
    }

    /// Persist the current graph to its store path. `open`/`refresh` already
    /// do this; exposed for a consumer that wants explicit control over
    /// when disk writes happen.
    pub fn save(&self) -> Result<()> {
        persist(&self.store_path, &self.stored)
    }

    /// The repository root this `Codebase` was opened against.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where this `Codebase`'s graph is persisted.
    pub fn store_path(&self) -> &Path {
        &self.store_path
    }

    /// Every file this `Codebase`'s last analysis covered.
    pub fn files(&self) -> &[codeate_core::File] {
        &self.repository.files
    }

    fn engine(&self) -> QueryEngine<'_> {
        QueryEngine::new(&self.graph)
    }

    // -- Symbol lookup, definition lookup --------------------------------

    /// Every symbol with exactly this name.
    pub fn symbol(&self, name: &str) -> Vec<SymbolEntity> {
        self.engine()
            .symbols_named(name)
            .into_iter()
            .cloned()
            .collect()
    }

    /// The symbol with this exact id, if any.
    pub fn symbol_by_id(&self, id: &EntityId) -> Option<SymbolEntity> {
        self.engine().symbol_by_id(id).cloned()
    }

    /// The syntax entity that declares `symbol_id`.
    pub fn definition(&self, symbol_id: &EntityId) -> Option<SyntaxEntity> {
        self.engine().definition_of(symbol_id).cloned()
    }

    // -- Call graph queries ------------------------------------------------

    /// Symbols that call `symbol_id`.
    pub fn callers(&self, symbol_id: &EntityId) -> Vec<SymbolEntity> {
        self.engine()
            .callers_of(symbol_id)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Symbols `symbol_id` calls.
    pub fn callees(&self, symbol_id: &EntityId) -> Vec<SymbolEntity> {
        self.engine()
            .callees_of(symbol_id)
            .into_iter()
            .cloned()
            .collect()
    }

    // -- Dependency analysis ------------------------------------------------

    /// Everything `id` transitively depends on.
    pub fn dependencies(&self, id: &EntityId) -> Vec<EntityId> {
        self.engine().dependencies_of(id)
    }

    /// Everything that transitively depends on `id`.
    pub fn dependents(&self, id: &EntityId) -> Vec<EntityId> {
        self.engine().dependents_of(id)
    }

    // -- Implementation lookup ----------------------------------------------

    /// Symbols that implement the interface/trait `trait_id`.
    pub fn implementers(&self, trait_id: &EntityId) -> Vec<SymbolEntity> {
        self.engine()
            .implementers_of(trait_id)
            .into_iter()
            .cloned()
            .collect()
    }

    // -- Reference lookup -----------------------------------------------

    /// Symbols that reference `symbol_id`.
    pub fn references(&self, symbol_id: &EntityId) -> Vec<SymbolEntity> {
        self.engine()
            .references_to(symbol_id)
            .into_iter()
            .cloned()
            .collect()
    }

    // -- Impact analysis ------------------------------------------------

    /// Everything transitively affected by a change to `id`.
    pub fn impact(&self, id: &EntityId) -> Vec<EntityId> {
        self.engine().impact_of(id)
    }

    // -- Repository exploration ------------------------------------------

    /// Functions/methods named `main`.
    pub fn entry_points(&self) -> Vec<SyntaxEntity> {
        self.engine().entry_points().into_iter().cloned().collect()
    }

    /// The nested module hierarchy.
    pub fn module_tree(&self) -> Vec<ModuleTree> {
        self.engine().module_tree()
    }

    // -- Phase 14: repository-level state ---------------------------------

    /// Every `Cargo.toml` package in the repository.
    pub fn packages(&self) -> Vec<SyntaxEntity> {
        self.engine().packages().into_iter().cloned().collect()
    }

    /// Every declared dependency across every manifest.
    pub fn package_dependencies(&self) -> Vec<SyntaxEntity> {
        self.engine()
            .package_dependencies()
            .into_iter()
            .cloned()
            .collect()
    }

    /// Every build target (`lib`/`bin`/`test`/`bench`/`example`).
    pub fn build_targets(&self) -> Vec<SyntaxEntity> {
        self.engine().build_targets().into_iter().cloned().collect()
    }

    /// Every `[profile.*]` configuration.
    pub fn configurations(&self) -> Vec<SyntaxEntity> {
        self.engine()
            .configurations()
            .into_iter()
            .cloned()
            .collect()
    }

    /// Every `#[test]`-attributed function.
    pub fn tests(&self) -> Vec<SyntaxEntity> {
        self.engine().tests().into_iter().cloned().collect()
    }

    /// Every detected API route (best-effort).
    pub fn api_endpoints(&self) -> Vec<SyntaxEntity> {
        self.engine().api_endpoints().into_iter().cloned().collect()
    }

    /// Every file flagged as machine-generated (best-effort).
    pub fn generated_files(&self) -> Vec<SyntaxEntity> {
        self.engine()
            .generated_files()
            .into_iter()
            .cloned()
            .collect()
    }

    /// Every first-class entry point tied to a build target.
    pub fn manifest_entry_points(&self) -> Vec<SyntaxEntity> {
        self.engine()
            .manifest_entry_points()
            .into_iter()
            .cloned()
            .collect()
    }
}

fn default_store_path(root: &Path) -> PathBuf {
    root.join(".codeate").join("graph.json")
}

fn ingest_and_detect(root: &Path) -> Result<Repository> {
    let repository = ingest(root)?;
    let registry = default_registry();
    detect_languages(&repository, &registry)
}

fn update_incremental_checked(
    previous: &StoredGraph,
    repository: &Repository,
) -> Result<StoredGraph> {
    let (updated, report) = previous.update_incremental(repository)?;
    if !report.is_clean() {
        return Err(Error::Message(format!(
            "incremental analysis produced an inconsistent graph: {report:?}"
        )));
    }
    Ok(updated)
}

fn persist(path: &Path, stored: &StoredGraph) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    codeate_core::store::save(path, stored)
}
