pub mod adapter;
pub mod adapters;
pub mod config;
pub mod detect;
pub mod error;
pub mod graph;
pub mod hash;
pub mod ingest;
pub mod language;
pub mod model;
pub mod pipeline;
pub mod repository;
pub mod resolve;
pub mod store;
pub mod symbols;
pub mod types;

pub use adapter::LanguageAdapter;
pub use adapters::RustAdapter;
pub use config::Config;
pub use detect::{
    default_registry, detect, detect_by_content, detect_by_extension, detect_languages,
};
pub use error::{Error, Result};
pub use graph::{diff, Graph, GraphDiff, IntegrityReport, Node};
pub use hash::hash_bytes;
pub use ingest::ingest;
pub use language::{LanguageDescriptor, LanguageId, LanguageRegistry};
pub use model::{Edge, EdgeKind, EntityId, SymbolEntity, SymbolKind, SyntaxEntity, SyntaxKind};
pub use pipeline::{analyze_repository, AnalysisResult};
pub use repository::{File, Repository};
pub use resolve::{resolve_depends_on, unresolved_symbol};
pub use store::{export_graphml, StoredGraph, CURRENT_VERSION};
pub use symbols::{
    build_symbol_table, defined_in_edges, merge_symbol_tables, ScopeTree, SymbolTable,
    SymbolTableEntry,
};
pub use types::{type_symbol_for, TypeRef};
