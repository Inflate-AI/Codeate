# Changelog

All notable changes to Codeate are documented here, per closed phase.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

Nothing closed yet.

## [Phase 2] — Language Detection

### Added

- `LanguageId` and a pluggable `LanguageRegistry` (`language.rs`): `LanguageId::Known(String)`
  / `Unknown`, `LanguageDescriptor { id, extensions, shebang_interpreters }`, registry
  `register`/`by_extension`/`by_shebang_interpreter` — the extension point later language
  adapters (Phase 4+) register against — built TDD, 5 unit tests.
- Extension-based detection (`detect.rs`): `detect_by_extension()`, plus `default_registry()`
  pre-populated with rust, python, javascript, typescript, markdown, toml, json — built TDD,
  4 unit tests.
- Shebang-based content-detection fallback: `detect_by_content()` recognizes
  `#!/usr/bin/env python3`-style and direct-interpreter shebangs for extensionless files —
  built TDD, 4 unit tests.
- Combined `detect()` entry point: extension first, shebang fallback, else
  `LanguageId::Unknown` — detection never silently drops a file — built TDD, 4 unit tests.
- Detection results attached to ingested files: `File` gained a `language: LanguageId` field
  (defaults `Unknown`, added non-breakingly via a `with_language()` builder) and
  `detect_languages(&Repository, &registry) -> Result<Repository>` annotates every file from
  a Phase 1 ingestion — built TDD, 3 unit tests on `File` plus an integration test against an
  extended `basic_repo` fixture (added an extensionless python-shebang script and an
  unknown-extension file) confirming rust/markdown/python/`Unknown` classification.

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (47 tests total: 38 `codeate-core` unit, 1 detect
  integration, 5 ingest integration, 3 CLI integration) via `scripts/check.ps1`.

## [Phase 1] — Repository Ingestion

### Added

- `File`/`Repository` entities in `codeate-core` (`repository.rs`): `File { path,
  content_hash, size_bytes }` with path identity normalized (backslash → forward-slash)
  so it doesn't depend on the host OS; `Repository { root, files }` — built TDD, 5 unit
  tests (equality, separator normalization, hash inequality, repository
  construction/equality).
- Deterministic content hashing (`hash.rs`): `hash_bytes()` via `sha2`/SHA-256 producing
  a stable hex digest — built TDD, 4 unit tests (stability, distinctness, known
  empty-input digest, fixed-length lowercase hex format).
- `.gitignore`/`.codeateignore`-aware file tree walker (`ingest.rs`): `ingest(root)` uses
  the `ignore` crate to walk a directory respecting both ignore files, reads and hashes
  every non-ignored file, and returns a `Repository` with files sorted by path for
  deterministic ordering — built TDD against a fixture repo (`tests/fixtures/basic_repo/`,
  including a gitignored `target/` dir and a codeateignored `secrets.local`), 5
  integration tests (exclusion, sort order, hash/size correctness, missing-root error).
- Repository-level `codeate.toml` config schema (`config.rs`): `Config { scope, excludes,
  target_languages }` via `serde`+`toml`, `Config::load(path)` defaulting when the file is
  absent and returning a clear `Error::Message` on malformed TOML — built TDD, 3 unit
  tests (full parse, default-on-absent, malformed-is-error) against fixture TOML files.
- Determinism guarantee integration test: two `ingest()` runs over the same fixture repo
  assert byte-identical `Repository` results (same file set, order, hashes).
- New dependencies: `ignore`, `sha2`, `serde` (derive), `toml`.

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (26 tests total: 18 `codeate-core` unit, 5 ingestion
  integration, 3 CLI integration) via `scripts/check.ps1`.

## [Phase 0] — Project Foundations

### Added

- Cargo workspace with two crates: `codeate-core` (lib) and `codeate-cli` (binary
  `codeate`, depends on `codeate-core`).
- Base error type in `codeate-core` (`error::Error` via `thiserror`, `Result<T>`
  alias) with `Io` and `Message` variants — built TDD, 5 unit tests written first
  (red) then made to pass (green): Display formatting, `From<io::Error>` conversion,
  `std::error::Error` impl, Debug output.
- Local check script (`scripts/check.ps1`) running `cargo fmt --check` →
  `cargo clippy -- -D warnings` → `cargo test` in sequence, as the local stand-in
  for CI.
- `codeate` CLI binary skeleton (via `clap`): `codeate --version` and
  `codeate --help` — built TDD, 3 integration tests in `codeate-cli/tests/cli.rs`
  written first against a no-op `main()` (2 red), then made to pass by wiring in
  `clap::Parser` (green). No subcommands yet.

### Fixed

- Repaired a corrupted local `rustup` stable toolchain (missing manifest) by
  uninstalling and reinstalling it; now on rustc/cargo/clippy/rustfmt 1.98.1.

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (8 tests total) via `scripts/check.ps1`.
