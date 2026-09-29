use std::path::Path;

use crate::error::Result;
use crate::language::{LanguageDescriptor, LanguageId, LanguageRegistry};
use crate::repository::Repository;

/// A registry pre-populated with a handful of common languages, enough to
/// exercise extension- and content-based detection. Not an exhaustive
/// language list — later language adapters (Phase 4+) register their own
/// descriptors as they're implemented.
pub fn default_registry() -> LanguageRegistry {
    let mut registry = LanguageRegistry::new();

    registry.register(LanguageDescriptor::new(LanguageId::known("rust")).with_extensions(["rs"]));
    registry.register(
        LanguageDescriptor::new(LanguageId::known("python"))
            .with_extensions(["py"])
            .with_shebang_interpreters(["python", "python3"]),
    );
    registry.register(
        LanguageDescriptor::new(LanguageId::known("javascript"))
            .with_extensions(["js", "mjs", "cjs"])
            .with_shebang_interpreters(["node"]),
    );
    registry.register(
        LanguageDescriptor::new(LanguageId::known("typescript")).with_extensions(["ts", "tsx"]),
    );
    registry.register(
        LanguageDescriptor::new(LanguageId::known("markdown")).with_extensions(["md", "markdown"]),
    );
    registry.register(LanguageDescriptor::new(LanguageId::known("toml")).with_extensions(["toml"]));
    registry.register(LanguageDescriptor::new(LanguageId::known("json")).with_extensions(["json"]));

    registry
}

/// Detect a language purely from `path`'s extension. Returns `None` when the
/// path has no extension, or the extension isn't recognized by `registry`.
pub fn detect_by_extension(path: &Path, registry: &LanguageRegistry) -> Option<LanguageId> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| registry.by_extension(ext))
        .cloned()
}

/// Detect a language from a shebang line at the start of `bytes` (e.g.
/// `#!/usr/bin/env python3` or `#!/usr/bin/python3`), for extensionless
/// scripts. Returns `None` when there is no shebang, or its interpreter
/// isn't recognized by `registry`.
pub fn detect_by_content(bytes: &[u8], registry: &LanguageRegistry) -> Option<LanguageId> {
    let first_line = bytes.split(|&b| b == b'\n').next()?;
    let first_line = std::str::from_utf8(first_line).ok()?.trim();
    let rest = first_line.strip_prefix("#!")?;

    let mut parts = rest.split_whitespace();
    let mut token = parts.next()?;
    if token.rsplit('/').next() == Some("env") {
        token = parts.next()?;
    }
    let interpreter = token.rsplit('/').next().unwrap_or(token);

    registry.by_shebang_interpreter(interpreter).cloned()
}

/// Detect the language of a file: extension first, shebang content as a
/// fallback, and `LanguageId::Unknown` when neither resolves — detection
/// never drops a file silently.
pub fn detect(path: &Path, bytes: &[u8], registry: &LanguageRegistry) -> LanguageId {
    detect_by_extension(path, registry)
        .or_else(|| detect_by_content(bytes, registry))
        .unwrap_or(LanguageId::Unknown)
}

/// Annotate every file in `repository` with its detected language, returning
/// a new `Repository`. Re-reads each file's bytes from disk (under
/// `repository.root`) so shebang-based fallback detection has content to
/// inspect.
pub fn detect_languages(
    repository: &Repository,
    registry: &LanguageRegistry,
) -> Result<Repository> {
    let mut files = Vec::with_capacity(repository.files.len());

    for file in &repository.files {
        let absolute_path = repository.root.join(&file.path);
        let bytes = std::fs::read(&absolute_path)?;
        let language = detect(Path::new(&file.path), &bytes, registry);
        files.push(file.clone().with_language(language));
    }

    Ok(Repository::new(repository.root.clone(), files))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_by_extension() {
        let registry = default_registry();
        assert_eq!(
            detect_by_extension(Path::new("src/main.rs"), &registry),
            Some(LanguageId::known("rust"))
        );
    }

    #[test]
    fn detects_python_by_extension() {
        let registry = default_registry();
        assert_eq!(
            detect_by_extension(Path::new("scripts/build.py"), &registry),
            Some(LanguageId::known("python"))
        );
    }

    #[test]
    fn returns_none_for_an_unrecognized_extension() {
        let registry = default_registry();
        assert_eq!(
            detect_by_extension(Path::new("archive.zip"), &registry),
            None
        );
    }

    #[test]
    fn returns_none_for_a_path_with_no_extension() {
        let registry = default_registry();
        assert_eq!(detect_by_extension(Path::new("Makefile"), &registry), None);
    }

    #[test]
    fn detects_python_from_an_env_shebang() {
        let registry = default_registry();
        let content = b"#!/usr/bin/env python3\nprint('hi')\n";
        assert_eq!(
            detect_by_content(content, &registry),
            Some(LanguageId::known("python"))
        );
    }

    #[test]
    fn detects_python_from_a_direct_interpreter_shebang() {
        let registry = default_registry();
        let content = b"#!/usr/bin/python3\nprint('hi')\n";
        assert_eq!(
            detect_by_content(content, &registry),
            Some(LanguageId::known("python"))
        );
    }

    #[test]
    fn returns_none_for_content_without_a_shebang() {
        let registry = default_registry();
        let content = b"just some text\n";
        assert_eq!(detect_by_content(content, &registry), None);
    }

    #[test]
    fn returns_none_for_an_unrecognized_shebang_interpreter() {
        let registry = default_registry();
        let content = b"#!/bin/bash\necho hi\n";
        assert_eq!(detect_by_content(content, &registry), None);
    }

    #[test]
    fn detect_prefers_extension_over_content() {
        let registry = default_registry();
        // A .py file with no shebang: extension alone should be enough.
        let result = detect(Path::new("script.py"), b"print('hi')\n", &registry);
        assert_eq!(result, LanguageId::known("python"));
    }

    #[test]
    fn detect_falls_back_to_shebang_when_extensionless() {
        let registry = default_registry();
        let result = detect(
            Path::new("run"),
            b"#!/usr/bin/env node\nconsole.log('hi')\n",
            &registry,
        );
        assert_eq!(result, LanguageId::known("javascript"));
    }

    #[test]
    fn detect_returns_unknown_when_nothing_matches() {
        let registry = default_registry();
        let result = detect(Path::new("archive.zip"), b"binary junk", &registry);
        assert_eq!(result, LanguageId::Unknown);
    }

    #[test]
    fn detect_returns_unknown_for_extensionless_unrecognized_content() {
        let registry = default_registry();
        let result = detect(Path::new("LICENSE"), b"MIT License\n", &registry);
        assert_eq!(result, LanguageId::Unknown);
    }
}
