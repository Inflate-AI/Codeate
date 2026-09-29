# Changelog

All notable changes to Codeate are documented here, per closed phase.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

Nothing closed yet.

## [Phase 3] — Canonical Code Model

### Added

- `EntityId` (`model/id.rs`): a deterministic, language-independent identifier derived
  from an entity's kind + declaring location + name (+ optional disambiguator) via the
  Phase 1 `hash_bytes` primitive — the same logical entity always resolves to the same
  ID, on any run — built TDD, 6 unit tests.
- Closed `SyntaxKind`/`SyntaxEntity` (`model/syntax.rs`): the 12 README syntax kinds
  (`Module`, `Package`, `Class`, `Interface`, `Struct`, `Enum`, `Function`, `Method`,
  `Variable`, `Parameter`, `Statement`, `Expression`) as a genuinely closed enum, with no
  catch-all variant — enforced via an exhaustive-match test that fails to compile if a
  variant goes unhandled — built TDD, 4 unit tests.
- Closed `SymbolKind`/`SymbolEntity` (`model/symbol.rs`): the 8 README symbol kinds
  (`Symbol`, `Type`, `Field`, `Method`, `Function`, `Constant`, `Namespace`, `Generic`),
  same closed-enum discipline — built TDD, 4 unit tests.
- Closed `EdgeKind`/`Edge` (`model/edge.rs`): all 14 README relationship types
  (`CONTAINS`, `IMPORTS`, `EXPORTS`, `DECLARES`, `REFERENCES`, `CALLS`, `EXTENDS`,
  `IMPLEMENTS`, `OVERRIDES`, `INSTANTIATES`, `RETURNS`, `ACCEPTS`, `DEPENDS_ON`,
  `DEFINED_IN`), with a stable SCREAMING_SNAKE_CASE string form (`as_str()`/`parse()`)
  for later persistence (Phase 10) — built TDD, 5 unit tests including full round-trip
  coverage of all 14 kinds.
- Cross-cutting model invariant tests (`tests/model.rs`, 4 integration tests): ID
  stability across independent construction; every edge kind round-trips through its
  string form; a syntax node can be explicitly linked to its symbol via a `DefinedIn`
  edge despite having a distinct ID.

### Fixed

- `SyntaxKind::Function` and `SymbolKind::Function` (and other same-named kind pairs)
  both stringified to `"Function"`, which would have collided their `EntityId`s for a
  same-named function — caught while writing the cross-cutting invariant tests. Fixed by
  namespacing ID generation with a `"syntax::"`/`"symbol::"` domain prefix so syntax and
  symbol entities never collide on identity.

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (68 tests total: 57 `codeate-core` unit, 4 model
  invariant integration, 1 detect integration, 5 ingest integration, 3 CLI integration)
  via `scripts/check.ps1`.

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
