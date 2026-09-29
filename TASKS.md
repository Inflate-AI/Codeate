# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 1 — Repository Ingestion

Deterministically enumerate the files that make up a repository (see `ROADMAP.md`).

New dependencies proposed: `ignore` (gitignore-aware walking), `sha2` (content hashing),
`serde` + `toml` (config file parsing).

1. **`File` and `Repository` entities.**
   Add data types to `codeate-core`: `File { path: RelativePath, content_hash: String,
   size_bytes: u64 }` and `Repository { root: PathBuf, files: Vec<File> }`. Path type is
   normalized/relative to the repo root (no OS-specific separators leaking into identity).
   Done when: unit tests cover construction, equality, and that two `File`s with the same
   path/hash/size are equal regardless of platform path separator quirks.

2. **Deterministic content hashing.**
   A `hash_bytes(&[u8]) -> String` (or similar) function in `codeate-core` using `sha2`,
   returning a stable hex digest.
   Done when: unit tests confirm same input → same hash, different input → different hash,
   empty input has a defined hash, and the digest format is stable (hex, fixed length).

3. **`.gitignore`/`.codeateignore`-aware file tree walker.**
   A function that walks a root directory using the `ignore` crate (respects `.gitignore`
   and a repo-specific `.codeateignore`), reads each non-ignored file, computes its hash and
   size, and returns a `Repository`. Files are sorted by path so output order is stable.
   Done when: integration tests against fixture directories (in
   `crates/codeate-core/tests/fixtures/`) confirm ignored files are excluded and the
   returned file list is sorted and stable across runs.

4. **Repository-level config file.**
   Define a `codeate.toml` schema (`serde` + `toml`): analysis scope (include paths),
   explicit excludes, target languages (list, informational only at this phase — no
   language detection yet). Loading is optional (sane defaults when absent).
   Done when: unit tests cover parsing a full config, parsing an absent config (defaults),
   and a malformed config producing a clear `codeate_core::Error`.

5. **Determinism guarantee test.**
   Integration test: ingest the same fixture repository twice (two separate walker runs,
   same config) and assert the resulting `Repository` values are identical — same file set,
   same order, same hashes.
   Done when: this test passes and would fail if walker ordering or hashing were
   nondeterministic (e.g. relying on OS directory iteration order without sorting).
