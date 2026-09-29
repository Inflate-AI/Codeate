# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 2 — Language Detection

Classify each ingested file by language, independent of any parser (see `ROADMAP.md`).

No new external dependencies proposed — pure logic over the `File`/`Repository` entities
from Phase 1.

1. **`LanguageId` and a pluggable language registry.**
   A `LanguageId` type (e.g. a newtype over a static name: `"rust"`, `"python"`, ...) plus
   a `LanguageRegistry` that language descriptors register into — each descriptor carries
   its `LanguageId`, recognized file extensions, and (optionally) content sniffers. This is
   the extension point later language adapters (Phase 4+) register against.
   Done when: unit tests cover registering a descriptor and looking it up by id/extension.

2. **Extension-based detection.**
   `detect_by_extension(path, &registry) -> LanguageId` using the registry's extension map.
   Ship a default registry pre-populated with a handful of common languages/extensions
   (rust, python, javascript, typescript, markdown, toml, json — enough to exercise the
   registry, not an exhaustive list).
   Done when: unit tests confirm known extensions resolve to the right `LanguageId`.

3. **Content-based detection fallback.**
   `detect_by_content(bytes) -> Option<LanguageId>` for extensionless files, at minimum
   recognizing shebang lines (`#!/usr/bin/env python3` → python, etc.) via the registry.
   Used when extension-based detection doesn't resolve.
   Done when: unit tests cover shebang recognition and no-match content.

4. **Explicit unknown-language representation.**
   `LanguageId::Unknown` (or equivalent) — detection always returns a language, never
   silently drops a file. Combine extension + content detection into one
   `detect(path, bytes, &registry) -> LanguageId` entry point with a clear precedence
   (extension first, content fallback, else `Unknown`).
   Done when: unit tests confirm an unrecognized extension/content combination yields
   `Unknown` rather than `None` or a panic.

5. **Attach detection results to ingested files.**
   Extend the Phase 1 `File` entity (or a thin wrapper) with a `language: LanguageId`
   field, and a `detect_languages(&Repository, &registry) -> Repository` (or similar) that
   annotates every file from ingestion.
   Done when: an integration test ingests a fixture repo (mixed known/unknown/extensionless
   files) and asserts each file's detected language, including at least one `Unknown` case.
