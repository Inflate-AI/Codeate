//! Parses `benches/fixtures-manifest.json`: one real-world repo per
//! language, pinned by commit, used as the fixture for Phase 18.1's
//! performance/coverage comparison against Code Review Graph and
//! Graphify/Graphifyy.

use std::collections::HashSet;

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FixtureEntry {
    pub language: String,
    pub repo: String,
    pub commit: String,
    #[serde(default)]
    pub subdir: Option<String>,
    pub license: String,
    pub approx_files: u32,
    pub approx_size_mb: u32,
    /// Paths to exclude from analysis via a generated `.codeateignore`, for
    /// a fixture's own deliberately-invalid test data (e.g. a compiler-test
    /// fixture containing intentionally broken syntax) that isn't
    /// representative source code -- documented per-entry rather than
    /// silently dropped.
    #[serde(default)]
    pub ignore_globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    pub fixtures: Vec<FixtureEntry>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("invalid manifest JSON: {0}")]
    InvalidJson(String),
    #[error("duplicate language in manifest: {0}")]
    DuplicateLanguage(String),
    #[error("entry for {language} has a malformed commit SHA: {commit:?} (expected 40 lowercase hex characters)")]
    MalformedCommit { language: String, commit: String },
}

/// Parse a manifest from its raw JSON text, validating that every entry has
/// a well-formed (40 lowercase hex character) commit SHA and that no two
/// entries claim the same language -- both are invariants the fetch script
/// and the per-language benchmark loop depend on.
pub fn parse_manifest(json: &str) -> Result<Manifest, ManifestError> {
    let manifest: Manifest =
        serde_json::from_str(json).map_err(|err| ManifestError::InvalidJson(err.to_string()))?;

    let mut seen_languages = HashSet::new();
    for entry in &manifest.fixtures {
        if !seen_languages.insert(entry.language.clone()) {
            return Err(ManifestError::DuplicateLanguage(entry.language.clone()));
        }
        if !is_valid_commit_sha(&entry.commit) {
            return Err(ManifestError::MalformedCommit {
                language: entry.language.clone(),
                commit: entry.commit.clone(),
            });
        }
    }

    Ok(manifest)
}

fn is_valid_commit_sha(commit: &str) -> bool {
    commit.len() == 40 && commit.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_entry_json(language: &str, commit: &str) -> String {
        format!(
            r#"{{"language": "{language}", "repo": "owner/repo", "commit": "{commit}", "license": "MIT", "approx_files": 1200, "approx_size_mb": 50}}"#
        )
    }

    #[test]
    fn parses_a_single_valid_entry() {
        let commit = "a".repeat(40);
        let json = format!(r#"{{"fixtures": [{}]}}"#, valid_entry_json("rust", &commit));
        let manifest = parse_manifest(&json).expect("valid manifest parses");
        assert_eq!(manifest.fixtures.len(), 1);
        assert_eq!(manifest.fixtures[0].language, "rust");
        assert_eq!(manifest.fixtures[0].commit, commit);
        assert_eq!(manifest.fixtures[0].subdir, None);
    }

    #[test]
    fn defaults_ignore_globs_to_empty_when_absent() {
        let commit = "e".repeat(40);
        let json = format!(r#"{{"fixtures": [{}]}}"#, valid_entry_json("go", &commit));
        let manifest = parse_manifest(&json).expect("valid manifest parses");
        assert_eq!(manifest.fixtures[0].ignore_globs, Vec::<String>::new());
    }

    #[test]
    fn parses_an_entry_with_ignore_globs() {
        let commit = "f".repeat(40);
        let json = format!(
            r#"{{"fixtures": [{{"language": "rust", "repo": "owner/repo", "commit": "{commit}", "license": "MIT", "approx_files": 10, "approx_size_mb": 1, "ignore_globs": ["tests/broken/"]}}]}}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest with ignore_globs parses");
        assert_eq!(manifest.fixtures[0].ignore_globs, vec!["tests/broken/".to_string()]);
    }

    #[test]
    fn parses_an_entry_with_a_subdir() {
        let commit = "b".repeat(40);
        let json = format!(
            r#"{{"fixtures": [{{"language": "ruby", "repo": "rails/rails", "commit": "{commit}", "subdir": "activerecord", "license": "MIT", "approx_files": 2000, "approx_size_mb": 80}}]}}"#
        );
        let manifest = parse_manifest(&json).expect("valid manifest with subdir parses");
        assert_eq!(manifest.fixtures[0].subdir, Some("activerecord".to_string()));
    }

    #[test]
    fn rejects_malformed_json() {
        let err = parse_manifest("not json").expect_err("malformed JSON should error");
        assert!(matches!(err, ManifestError::InvalidJson(_)));
    }

    #[test]
    fn rejects_a_missing_required_field() {
        let json = r#"{"fixtures": [{"language": "rust", "repo": "a/b"}]}"#;
        let err = parse_manifest(json).expect_err("missing fields should error");
        assert!(matches!(err, ManifestError::InvalidJson(_)));
    }

    #[test]
    fn rejects_a_duplicate_language() {
        let commit_a = "c".repeat(40);
        let commit_b = "d".repeat(40);
        let json = format!(
            r#"{{"fixtures": [{}, {}]}}"#,
            valid_entry_json("go", &commit_a),
            valid_entry_json("go", &commit_b)
        );
        let err = parse_manifest(&json).expect_err("duplicate language should error");
        assert_eq!(err, ManifestError::DuplicateLanguage("go".to_string()));
    }

    #[test]
    fn rejects_a_malformed_commit_sha() {
        let json = format!(r#"{{"fixtures": [{}]}}"#, valid_entry_json("python", "not-a-sha"));
        let err = parse_manifest(&json).expect_err("malformed commit SHA should error");
        assert_eq!(
            err,
            ManifestError::MalformedCommit {
                language: "python".to_string(),
                commit: "not-a-sha".to_string(),
            }
        );
    }

    #[test]
    fn rejects_an_uppercase_commit_sha() {
        let commit = "A".repeat(40);
        let json = format!(r#"{{"fixtures": [{}]}}"#, valid_entry_json("java", &commit));
        let err = parse_manifest(&json).expect_err("uppercase commit SHA should error");
        assert!(matches!(err, ManifestError::MalformedCommit { .. }));
    }
}
