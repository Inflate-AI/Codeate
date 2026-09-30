pub mod adapter;
pub mod adapters;
pub mod config;
pub mod detect;
pub mod discovery;
pub mod error;
pub mod graph;
pub mod hash;
pub mod incremental;
pub mod ingest;
pub mod language;
pub mod manifest;
pub mod model;
pub mod pipeline;
pub mod query;
pub mod repository;
pub mod resolve;
pub mod store;
pub mod symbols;
pub mod types;

pub use adapter::LanguageAdapter;
pub use adapters::{PythonAdapter, RustAdapter};
pub use config::Config;
pub use detect::{
    default_registry, detect, detect_by_content, detect_by_extension, detect_languages,
};
pub use discovery::{discover, DiscoveryResult};
pub use error::{Error, Result};
pub use graph::{diff, Graph, GraphDiff, IntegrityReport, Node};
pub use hash::hash_bytes;
pub use incremental::{affected_files, detect_changes, entities_owned_by, ChangeSet};
pub use ingest::ingest;
pub use language::{LanguageDescriptor, LanguageId, LanguageRegistry};
pub use manifest::{parse_manifests, BuildTargetInfo, BuildTargetKind, ManifestResult};
pub use model::{Edge, EdgeKind, EntityId, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
pub use pipeline::{
    analyze_incremental, analyze_repository, analyze_repository_state, merge_repository_state,
    AnalysisResult,
};
pub use query::{ModuleTree, QueryEngine};
pub use repository::{File, Repository};
pub use resolve::{resolve_depends_on, unresolved_symbol};
pub use store::{export_graphml, StoredGraph, CURRENT_VERSION};
pub use symbols::{
    build_symbol_table, defined_in_edges, merge_symbol_tables, ScopeTree, SymbolTable,
    SymbolTableEntry,
};
pub use types::{type_symbol_for, TypeRef};
