use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

/// Repository-level ingestion config, loaded from a `codeate.toml` at the
/// repository root. Absent entirely, and every field falls back to its
/// default.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Paths (relative to the repo root) that ingestion is scoped to.
    pub scope: Vec<String>,
    /// Extra exclude globs, applied in addition to `.gitignore`/`.codeateignore`.
    pub excludes: Vec<String>,
    /// Languages ingestion should target (informational only until Phase 2).
    pub target_languages: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scope: vec![".".to_string()],
            excludes: Vec::new(),
            target_languages: Vec::new(),
        }
    }
}

impl Config {
    /// Load config from `path`. A missing file yields `Config::default()`;
    /// a present-but-malformed file is an error.
    pub fn load(path: impl AsRef<Path>) -> Result<Config> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Config::default());
        }

        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|err| {
            Error::Message(format!("invalid codeate.toml at {}: {err}", path.display()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/config")
            .join(name)
    }

    #[test]
    fn loading_a_full_config_parses_every_field() {
        let config = Config::load(fixture("full.toml")).expect("load full.toml");

        assert_eq!(config.scope, vec!["src".to_string(), "crates".to_string()]);
        assert_eq!(config.excludes, vec!["**/generated/**".to_string()]);
        assert_eq!(
            config.target_languages,
            vec!["rust".to_string(), "python".to_string()]
        );
    }

    #[test]
    fn loading_an_absent_config_returns_defaults() {
        let config = Config::load(fixture("does_not_exist.toml")).expect("default config");

        assert_eq!(config, Config::default());
        assert_eq!(config.scope, vec![".".to_string()]);
        assert!(config.excludes.is_empty());
        assert!(config.target_languages.is_empty());
    }

    #[test]
    fn loading_a_malformed_config_is_a_clear_error() {
        let result = Config::load(fixture("malformed.toml"));

        let err = result.expect_err("malformed config should error");
        assert!(matches!(err, Error::Message(_)));
        assert!(err.to_string().contains("codeate.toml"));
    }
}
