# Changelog

All notable changes to Codeate are documented here, per closed phase.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

Nothing closed yet.

## [Phase 7] — Reference & Call Resolution

### Added

- Explicit unresolved-reference placeholder (`resolve/unresolved.rs`): `unresolved_symbol(reason, name)`
  gives every unresolvable reference/call a canonical, deterministic `SymbolEntity` (the reason folded
  into the symbol's own name, e.g. `"external-path:std::process::exit"`), so an edge to it is always
  real, never a silently dropped reference -- built TDD, 4 unit tests.
- `REFERENCES` edges (`resolve_references` in `adapters/rust.rs`): walks a function/method body's
  common expression forms for single-segment identifier usages, resolving each via the Phase 5
  `ScopeTree`; an undefined name gets an explicit `unknown-local` placeholder -- built TDD, 2 unit
  tests.
- `CALLS` edges (`resolve_calls`): resolves free-function calls via scope lookup and method calls by
  best-effort name match against any `Method` symbol in the file (full receiver-type dispatch needs
  Phase 8's `OVERRIDES`/`IMPLEMENTS`, out of scope here); multi-segment/external callee paths get an
  explicit `external-path` placeholder -- built TDD, 3 unit tests.
- `INSTANTIATES` edges (`resolve_instantiates`): struct literals always resolve to the struct's `Type`
  symbol (real one in scope, else a `Named`-type stand-in for an external struct); tuple-struct-style
  calls are only treated as a constructor when the callee resolves in scope to a `Type` symbol -- an
  unrecognized call is left to `resolve_calls`, not guessed at -- built TDD, 2 unit tests.
- Combined per-body resolution pass (`resolve_body`): ties reference/call/instantiation resolution
  together into one walk over a function/method body -- built TDD, 1 integration-style test exercising
  a reference, a resolved call, an unresolved external call, and an instantiation together.

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (129 tests total: 115 `codeate-core` unit, 3 symbols end-to-end, 1
  golden-file, 4 model invariant, 1 detect, 5 ingest, 3 CLI) via `scripts/check.ps1`.

## [Phase 6] — Type Resolution

### Added

- `TypeRef` model and canonical type symbols (`types.rs`): a closed representation --
  `Primitive`, `Named { name, type_args }`, `Reference { mutable, inner }`, `Generic`,
  `Unresolved(raw)` -- plus `type_symbol_for()` giving every `TypeRef` a deterministic
  `SymbolKind::Type` symbol, keyed so structurally different types (including
  same-named primitive vs. named, or differing generic args) never collide -- built TDD,
  6 unit tests.
- Rust type-expression extraction (`extract_type` in `adapters/rust.rs`): maps
  `syn::Type::Path` to `Primitive`/`Named`/`Generic` (18 built-in primitives, plus
  generic parameters recognized by name), `syn::Type::Reference` to `Reference`,
  everything else (tuples, trait objects, ...) to `Unresolved` with raw source text via
  `quote` -- built TDD, 6 unit tests.
- Struct field extraction (`extract_struct_fields`): Rust fields have no `SyntaxKind`
  counterpart in the closed Phase 3 model (only the symbol side has `Field`), so fields
  are extracted directly from `syn::ItemStruct` as `SymbolKind::Field` symbols,
  `DECLARES`-edged from the struct's `Type` symbol and `DEPENDS_ON`-edged to their
  resolved type -- built TDD, 2 unit tests.
- Parameter/return type binding (`extract_signature_types`): `ACCEPTS` edges from a
  function/method symbol to each parameter's type symbol, and a `RETURNS` edge to its
  return type symbol, defaulting to the unit type `()` when unspecified -- built TDD, 3
  unit tests.
- Generic parameters and explicit unresolved types (`extract_generic_params`): extracts
  `<T: Bound>` as `SymbolKind::Generic` symbols (`DECLARES`-edged from their owner) and
  threads the declared names through so matching usages resolve to `TypeRef::Generic`;
  confirmed a genuinely unresolvable type (`dyn Trait`) still produces a present,
  explicit `Unresolved` type symbol rather than being dropped -- built TDD, 2 unit
  tests.
- New dependencies: `quote`, `proc-macro2` (both already transitive via `syn`).

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (120 tests total: 103 `codeate-core` unit, 3 symbols
  end-to-end, 1 golden-file, 4 model invariant, 1 detect, 5 ingest, 3 CLI) via
  `scripts/check.ps1`.

## [Phase 5] — Symbol Resolution

### Added

- Scope model (`symbols/scope.rs`): `ScopeTree` built from a `SyntaxEntity`/`Edge`
  `CONTAINS` tree -- `Module`/`Function`/`Method` each open a scope; other declarations
  bind names into their immediately enclosing scope. `lookup()` walks up through parent
  scopes so an inner binding shadows an outer same-named one -- built TDD, 3 unit tests.
- Per-file symbol table (`symbols/table.rs`): `build_symbol_table()` maps each
  `SyntaxKind` to its `SymbolKind` (`Module`/`Package`→`Namespace`,
  `Class`/`Struct`/`Enum`/`Interface`→`Type`, `Function`→`Function`, `Method`→`Method`,
  `Variable`/`Parameter`→generic `Symbol`, since Phase 3's closed enum has no dedicated
  variable kind; `Statement`/`Expression` resolve to no symbol). Each symbol's ID is
  derived from its originating syntax entity's own ID -- built TDD, 4 unit tests.
- `DEFINED_IN` edges (`symbols/defined_in.rs`): `defined_in_edges()` follows each
  symbol's originating `DECLARES` edge back to its declaring module -- built TDD, 3 unit
  tests.
- Repository-scope merge: `merge_symbol_tables()` combines per-file tables into one,
  symbols never collide across files since ids are derived per-file, per-scope -- built
  TDD, 2 unit tests.
- Deterministic shadowing/redeclaration policy, exercised end-to-end in a new
  `tests/symbols.rs` (adapter → scope tree → symbol table → `DEFINED_IN`, 3 integration
  tests): an inner scope's binding shadows an outer same-named one on lookup while the
  outer symbol stays independently resolvable; a name redeclared in the *same* scope
  resolves to the most recently declared binding.

### Fixed

- `let x = 1; let x = 2;` in the same function produced two `Variable` syntax entities
  with an identical `EntityId` (same kind + scope + name collided) -- caught while
  writing the redeclaration tests. Fixed by folding the statement index into the id's
  location string in the Rust adapter's body extraction, so same-scope redeclarations
  get distinct, deterministic ids.

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (101 tests total: 84 `codeate-core` unit, 3 symbols
  end-to-end, 1 golden-file, 4 model invariant, 1 detect, 5 ingest, 3 CLI) via
  `scripts/check.ps1`.

## [Phase 4] — First Language Adapter: Parsing & AST Extraction

### Added

- `LanguageAdapter` trait (`adapter.rs`): `parse`/`extract` plus default-empty stub methods
  for later resolver stages (`resolve_symbols`, `resolve_references`), so the trait shape
  won't need to change again as Phases 5-8 land — built TDD, 2 unit tests against a
  minimal no-op test adapter.
- `RustAdapter` parser (`adapters/rust.rs`): `parse()` via `syn::parse_file`, converting
  syntax errors into a `codeate_core::Error` rather than panicking — built TDD, 2 unit
  tests (valid source, clear error on invalid syntax).
- Declaration extraction: `extract()` walks a parsed `syn::File` and produces canonical
  entities for modules (file-level + nested `mod`), structs, enums, traits (mapped to
  `Interface`), top-level functions, and `impl` block methods with their parameters
  (`self` excluded), plus `CONTAINS`/`DECLARES` edges from each module to its
  declarations. Entity IDs are scoped by a `module::item` path string so same-named items
  in different modules never collide — built TDD, 7 unit tests.
- Body extraction: extended to walk function/method bodies — `let` bindings become
  `Variable` entities, semicolon-terminated statements become `Statement` entities, and a
  tail expression becomes an `Expression` entity, linked to their function/method via
  `CONTAINS` only (not `DECLARES`) — built TDD, 3 unit tests.
- Golden-file test (`tests/rust_adapter.rs` + `tests/fixtures/rust_adapter/sample.rs`): one
  fixture exercising every extraction rule together (2 modules, 2 structs, an enum, a
  trait, a function, a method, 3 parameters, 2 variables, a statement, and 2 tail
  expressions), asserting both per-kind counts and specific `CONTAINS`/`DECLARES` edges.
- New dependency: `syn` (with `full`, `parsing`, `printing`, `extra-traits` features).

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (94 tests total: 80 `codeate-core` unit, 1 golden-file
  integration, 4 model invariant, 1 detect, 5 ingest, 3 CLI) via `scripts/check.ps1`.

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
