use std::path::Path;

use crate::language::{LanguageDescriptor, LanguageId, LanguageRegistry};

/// A registry pre-populated with a handful of common languages, enough to
/// exercise extension- and content-based detection. Not an exhaustive
/// language list — later language adapters (Phase 4+) register their own
/// descriptors as they're implemented.
pub fn default_registry() -> LanguageRegistry {
    let mut registry = LanguageRegistry::new();

    registry.register(
        LanguageDescriptor::new(LanguageId::known("rust")).with_extensions(["rs"]),
    );
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
}
