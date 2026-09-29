use std::path::PathBuf;

use crate::language::LanguageId;

/// A single file within an ingested repository, identified by a path that is
/// normalized (forward-slash separated) and relative to the repository root,
/// so identity does not depend on the host OS's path separator.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct File {
    pub path: String,
    pub content_hash: String,
    pub size_bytes: u64,
    pub language: LanguageId,
}

impl File {
    /// A file with no language classification yet (`LanguageId::Unknown`
    /// until `detect_languages` runs). Use `with_language` to attach one.
    pub fn new(path: impl Into<String>, content_hash: impl Into<String>, size_bytes: u64) -> Self {
        Self {
            path: normalize_path(&path.into()),
            content_hash: content_hash.into(),
            size_bytes,
            language: LanguageId::Unknown,
        }
    }

    pub fn with_language(mut self, language: LanguageId) -> Self {
        self.language = language;
        self
    }
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// The set of files that make up a repository, rooted at `root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub root: PathBuf,
    pub files: Vec<File>,
}

impl Repository {
    pub fn new(root: impl Into<PathBuf>, files: Vec<File>) -> Self {
        Self {
            root: root.into(),
            files,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_with_same_path_hash_and_size_is_equal() {
        let a = File::new("src/main.rs", "abc123", 42);
        let b = File::new("src/main.rs", "abc123", 42);
        assert_eq!(a, b);
    }

    #[test]
    fn file_path_identity_ignores_platform_separator() {
        let backslash = File::new("src\\main.rs", "abc123", 42);
        let forward = File::new("src/main.rs", "abc123", 42);
        assert_eq!(backslash, forward);
        assert_eq!(backslash.path, "src/main.rs");
    }

    #[test]
    fn file_with_different_hash_is_not_equal() {
        let a = File::new("src/main.rs", "abc123", 42);
        let b = File::new("src/main.rs", "def456", 42);
        assert_ne!(a, b);
    }

    #[test]
    fn new_file_has_unknown_language_until_classified() {
        let file = File::new("src/main.rs", "abc123", 42);
        assert_eq!(file.language, LanguageId::Unknown);
    }

    #[test]
    fn with_language_attaches_a_classification() {
        let file = File::new("src/main.rs", "abc123", 42).with_language(LanguageId::known("rust"));
        assert_eq!(file.language, LanguageId::known("rust"));
    }

    #[test]
    fn files_with_different_languages_are_not_equal() {
        let a = File::new("src/main.rs", "abc123", 42).with_language(LanguageId::known("rust"));
        let b = File::new("src/main.rs", "abc123", 42);
        assert_ne!(a, b);
    }

    #[test]
    fn repository_holds_its_root_and_files() {
        let files = vec![File::new("a.rs", "h1", 1), File::new("b.rs", "h2", 2)];
        let repo = Repository::new("/repo", files.clone());
        assert_eq!(repo.root, PathBuf::from("/repo"));
        assert_eq!(repo.files, files);
    }

    #[test]
    fn repositories_with_same_root_and_files_are_equal() {
        let files = vec![File::new("a.rs", "h1", 1)];
        let a = Repository::new("/repo", files.clone());
        let b = Repository::new("/repo", files);
        assert_eq!(a, b);
    }
}
