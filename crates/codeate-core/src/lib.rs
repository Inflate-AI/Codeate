pub mod config;
pub mod detect;
pub mod error;
pub mod hash;
pub mod ingest;
pub mod language;
pub mod repository;

pub use config::Config;
pub use detect::{default_registry, detect, detect_by_content, detect_by_extension};
pub use error::{Error, Result};
pub use hash::hash_bytes;
pub use ingest::ingest;
pub use language::{LanguageDescriptor, LanguageId, LanguageRegistry};
pub use repository::{File, Repository};
