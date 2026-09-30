# Changelog

All notable changes to Codeate are documented here, per closed phase.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

Nothing closed yet.

## [Phase 15] — Second Language Adapter

### Added

- Generalized `LanguageAdapter`: a new `resolve_file` hook covers everything past
  parsing/extraction (reference/call/instantiate resolution, structural relationships),
  with a default empty implementation so a parser-only adapter (Phase 4 shape) still
  satisfies the trait. `pipeline.rs` previously called Rust-specific free functions
  directly instead of going through the trait at all -- discovered while planning this
  phase, since a second adapter couldn't prove anything about language-agnosticism
  without this being real first.
- `DynLanguageAdapter` (`adapter.rs`, new): an object-safe view of `LanguageAdapter`,
  derived automatically via a blanket impl (`std::any::Any` type erasure for the
  per-adapter `Ast` associated type) so `pipeline::run_pipeline` can dispatch across
  multiple concrete adapters by `LanguageId` without knowing each one's `Ast` type at
  the call site. `adapters::default_adapters()` is the registry it dispatches through.
- Rust's own `resolve_body`/`resolve_implements`/`resolve_supertraits`/
  `resolve_overrides`/`resolve_imports`/`resolve_exports` calls moved out of
  `pipeline.rs` and behind `RustAdapter::resolve_file` -- re-deriving `ResolutionSites`
  internally via a second `extract_with_sites` call rather than threading them across
  the trait boundary, since they borrow into the AST. Verified **behavior-identical**:
  all 206 pre-existing tests passed unchanged after the refactor, with no fixture or
  assertion touched.
- Python adapter (`adapters/python.rs`, new, via `rustpython-parser`):
  - Parser + AST extractor: modules/classes/functions/methods/params/variables/
    statements mapped to canonical syntax entities + `CONTAINS`/`DECLARES`, matching
    the Rust adapter's item-vs-body extraction split -- built TDD, golden-file test
    (`tests/python_adapter.rs` + `tests/fixtures/python_adapter/sample.py`) mirroring
    `tests/rust_adapter.rs`.
  - Type extraction: PEP 484 annotations mapped to the existing (already
    language-agnostic) `TypeRef`/`type_symbol_for` -- primitives, `Foo[Bar, Baz]`
    generics, dotted attribute types, an explicit `Unresolved` for anything else (PEP
    604 unions included). Not wired into `ACCEPTS`/`RETURNS` binding, at parity with
    the Rust adapter's own undocumented-until-now `extract_signature_types` gap.
  - Reference/call/instantiate resolution: `REFERENCES` for `Load`-context names,
    `CALLS` for bare-name and best-effort method-name-match calls, `INSTANTIATES` for a
    call resolving in scope to a class -- via a single `walk_expr_sites`/
    `walk_stmt_sites` pair covering common expression/statement forms (mirrors the
    Rust adapter's own `walk_expr`/`walk_stmt`, including the same "a call site fires
    both a reference and a call" behavior).
  - Structural relationships: `EXTENDS` from a class to each base class (Python has no
    separate interface/trait construct, so a base is always another `Class`, unlike
    Rust's `IMPLEMENTS`-to-an-`Interface`); `IMPORTS` for `import`/`from ... import`,
    resolved by searching the repository's own declared entities by name (Python has
    no `crate::`-style marker distinguishing a local import from an external one, so
    every import tries local resolution first, falling back to an explicit
    `external-module` placeholder); `EXPORTS` via `__all__` when present, else the
    "no leading underscore" convention (Python's real public-API convention, since
    there's no `pub` keyword to inspect).
  - All of the above wired into `PythonAdapter::resolve_file`, dispatched through the
    same `run_pipeline` as Rust, verified via `tests/python_pipeline.rs` (a Python file
    resolved through `analyze_repository` end-to-end, and a repository containing both
    a Rust and a Python file resolved in the same call).

### Fixed

- `symbols/scope.rs`'s scope-tree builder bound `Class`/`Struct`/`Enum`/`Interface`
  entities into their enclosing scope but never recursed into their own `CONTAINS`
  children. Harmless for Rust (`impl` methods are always declared under the enclosing
  module, never under the `struct` itself, so this case never arose), but Python
  nests a class's methods directly under the `class` in the AST -- so every method
  silently never got a scope registered at all, meaning nothing inside a method body
  (not even `self`) ever resolved. Found while verifying Task 7's pipeline wiring
  end-to-end (a `Calls` edge that should have existed didn't). Fixed by recursing into
  the same scope rather than opening a new one (a class isn't a lexical scope boundary
  -- matches real Python semantics, where a method body's name lookup skips right past
  its own class). Added a dedicated regression test (a method nested under a class
  resolving a module-level name); all 242 Rust-side tests stayed green throughout.

### Verified

- Cross-adapter test (`tests/cross_adapter.rs`): equivalent Rust/Python fixture repos
  (a base type + a derived type that cross-file-imports it, plus a same-file helper
  call) analyzed through the identical `QueryEngine` surface. `impact_of` picks up
  both Rust's `IMPLEMENTS` and Python's `EXTENDS` identically; `dependencies_of`/
  `callers_of` behave identically for both, despite the underlying edge kinds
  differing where the languages themselves differ (no interface/trait concept in
  Python). Building this test also caught that a bare/imported cross-file function
  call doesn't resolve in *either* adapter (`resolve_calls`'s scope tree is
  single-file only -- a pre-existing, now-documented limitation, not a Python-adapter
  bug), so the fixture was corrected to match what's actually proven to work rather
  than papering over the gap.
- Dogfooded against Codeate's own repository (still pure Rust): stable, identical
  entity/symbol/edge counts across full and incremental `analyze` runs after the
  pipeline-generalization refactor.
- `scripts/check.ps1` (`cargo fmt --check`, `cargo clippy --workspace --all-targets`,
  `cargo test --workspace`) passes: 291 tests total.
- New dependency: `rustpython-parser`.

## [Phase 14] — Repository-Level State

### Added

- Canonical model: 7 new closed `SyntaxKind` variants -- `Dependency`, `BuildTarget`,
  `Configuration`, `Test`, `EntryPoint`, `ApiEndpoint`, `GeneratedCode` -- alongside the
  existing `Package`. Every exhaustive match on `SyntaxKind` (`symbols/table.rs`'s
  syntax→symbol mapping, `symbols/scope.rs`'s scope-binding walk) updated to stay a
  genuinely closed set; both map the new kinds to no-op/generic behavior since they're
  never produced by a `LanguageAdapter`'s AST walk.
- Manifest parsing (`manifest.rs`, new): every ingested `Cargo.toml` with a `[package]`
  table becomes a `Package` entity; `[dependencies]`/`[dev-dependencies]`/
  `[build-dependencies]` become `Dependency` entities (`CONTAINS` from the package,
  `DEPENDS_ON` at the symbol level); `[lib]`/`[[bin]]`/`[[test]]`/`[[bench]]`/
  `[[example]]` become `BuildTarget` entities, explicit or conventionally detected
  (`src/lib.rs`/`src/main.rs`); `[profile.*]` sections become `Configuration` entities.
  Nested-crate manifests resolve target paths relative to their own directory. A
  manifest with no `[package]` table (a virtual workspace root) is skipped -- built TDD,
  9 unit tests.
- Source discovery (`discovery.rs`, new): `#[test]`-attributed functions become `Test`
  entities `REFERENCES`-linked to their already-resolved `Function` symbol; common
  route attributes (`#[get]`/`#[post]`/`#[put]`/`#[delete]`/`#[patch]`/`#[route]`)
  become `ApiEndpoint` entities (best-effort, per the README's "adapter-dependent"
  scoping); an `@generated` marker in a file's first 5 lines flags it as a
  `GeneratedCode` entity (best-effort); a bin target's `main` function or a lib
  target's crate-root module becomes a first-class `EntryPoint` entity tied to its
  `BuildTarget` via `CONTAINS` -- built TDD, 7 unit tests.
- `pipeline::merge_repository_state`/`analyze_repository_state`: folds manifest +
  discovery output into a Phase 4-8 `AnalysisResult`, verified to produce a clean
  `IntegrityReport` (no dangling edges) on a merged fixture.
- 8 new `QueryEngine` methods and CLI query subcommands: `packages`,
  `package-dependencies`, `build-targets`, `configurations`, `tests`, `api-endpoints`,
  `generated-files`, `manifest-entry-points`. The existing `entry-points`
  (function-named-`main` heuristic) is untouched, for backward compatibility.
- `codeate analyze`'s full-analysis path now calls `analyze_repository_state` instead
  of `analyze_repository`, so every `analyze` run (not just a dedicated command)
  produces Phase 14 state.
- End-to-end coverage (`codeate-cli/tests/cli.rs`): a fixture repository with a real
  `Cargo.toml` (a dependency, an explicit `[[bin]]`, a `[profile.release]`) plus a
  `#[test]`-attributed function and a `main`, taken through `analyze` and every new
  query kind in both output modes.

### Fixed

- `StoredGraph::update_incremental` (Phase 13) strips Phase 14 data back out of a
  stored graph before feeding it to `analyze_incremental` as "previous" (Phase 14 state
  is always recomputed fresh, not carried through incremental analysis -- it isn't
  file-content keyed the way Phase 4-8's carry-over logic assumes). That stripping
  filtered entities and symbols but not edges. `analyze_incremental`'s no-change
  short-circuit returns its input verbatim, bypassing the endpoint-based edge filtering
  the normal path gets -- so a stale Phase 14 edge rode straight through, and a fresh
  set got appended on top of it on every subsequent `analyze`, growing the edge count
  on every run even with zero source changes. Found by dogfooding `codeate analyze .`
  against Codeate's own repository three times in a row (9195 -> 9473 -> ... edges);
  the existing test suite didn't cover a genuinely no-op re-analyze with Phase 14 data
  present. Fixed by filtering edges the same way entities/symbols already were; added
  `update_incremental_recomputes_phase_14_state_fresh_across_repeated_runs` (source
  change -> manifest change -> no-op re-analyze) to guard the specific sequence that
  exposed it.

### Verified

- Dogfooded against Codeate's own workspace: `codeate analyze .` correctly found both
  crates' `Package`s, every real dependency (`clap`, `syn`, `sha2`, ...), both
  `BuildTarget`s (`lib:codeate-core`, `bin:codeate`), hundreds of real `#[test]`
  functions, and both entry points -- and repeated `analyze` runs leave the graph
  stable (same entity/symbol/edge counts every time).
- `scripts/check.ps1` (`cargo fmt --check`, `cargo clippy --workspace --all-targets`,
  `cargo test --workspace`) passes: 248 tests total (28 `codeate-cli` integration, 206
  `codeate-core` unit, 3 symbols end-to-end, 1 golden-file, 4 model invariant, 1 detect,
  5 ingest).

## [Phase 13] — Incremental Analysis

### Added

- Schema v2 (`store::CURRENT_VERSION`): `StoredGraph` now persists the repository's file
  manifest (path + content hash, from Phase 1) and an entity→owning-file map alongside
  the graph itself. Both `File` and `LanguageId` gained `serde` derives to support this.
  A v1 file with neither field still loads (`#[serde(default)]`) rather than erroring.
- Entity ownership tracking through `pipeline::analyze_repository`: `AnalysisResult`
  gained an `owner_file: HashMap<EntityId, String>` field, populated for every syntax
  entity and every resolved symbol (not just declared ones), so later phases always know
  which file produced a given piece of the graph.
- Change detection (`incremental.rs`, new module): `detect_changes(previous_files,
  current_files) -> ChangeSet { added, modified, removed }`, compared by path + content
  hash — no re-parsing needed to know something changed.
- Affected-structure computation: `affected_files()` starts from a `ChangeSet` and walks
  every semantic edge kind (`IMPORTS`/`EXPORTS`/`REFERENCES`/`CALLS`/`EXTENDS`/
  `IMPLEMENTS`/`OVERRIDES`/`INSTANTIATES`/`RETURNS`/`ACCEPTS`/`DEPENDS_ON`) backwards from
  a changed file's entities to find every other file that must be re-resolved too (a
  file that imports from, calls into, or implements something in a changed file).
  Removed files are never included in the result — nothing left to re-parse there.
- Incremental re-analysis (`pipeline::analyze_incremental`, refactored out of
  `analyze_repository`'s body into a shared `run_pipeline` taking an optional file
  filter and a `CarryIn` of reused data): only the affected file set is re-parsed and
  re-resolved; every other file's syntax entities, resolved symbols, and non-structural
  edges are carried over from the previous run. The merged symbol table always includes
  the carried-over files' entities too, so cross-file resolution against unaffected
  files' symbols stays correct. `DEFINED_IN` and `DEPENDS_ON` are always recomputed
  fresh (cheap, and must reflect the full merged state). A no-op change set short-circuits
  to the previous result unchanged.
- Patch + re-verify (`StoredGraph::update_incremental`): runs `analyze_incremental`
  against a stored graph and rebuilds a `Graph` from the result to re-run Phase 9's
  integrity checks before the patched state is considered valid — a patched graph is
  never assumed correct just because it was assembled incrementally.
- CLI: `codeate analyze <path>` now reuses an existing graph at the target path
  incrementally by default (via `StoredGraph::update_incremental`); `--full` forces a
  full rebuild regardless. The summary line now says which mode ran (`analyzed fully` /
  `analyzed incrementally`).

### Verified

- Correctness: every incremental scenario (file added, file modified, file removed, and
  a chained 4-step sequence of edits mixing all of these against files with cross-file
  dependents) is asserted to produce a `Graph::canonical_form()` byte-identical to a
  from-scratch full rebuild of the same end state — incremental analysis is an
  optimization, not a different analysis.
- `scripts/check.ps1` (`cargo fmt --check`, `cargo clippy --workspace --all-targets`,
  `cargo test --workspace`) passes: 228 tests total (29 `codeate-cli` integration, 187
  `codeate-core` unit, 3 symbols end-to-end, 1 golden-file, 4 model invariant, 1 detect,
  5 ingest).

## [Phase 12] — Developer CLI

### Added

- `codeate analyze <path> [--out <path>]`: runs the full ingest -> detect-languages ->
  analyze_repository pipeline and persists the resulting `StoredGraph` to
  `<path>/.codeate/graph.json` by default, creating the directory as needed. Prints a
  one-line summary (file / syntax entity / symbol / edge counts) and the output path.
- `codeate query <kind> [--id <id>] [--store <path>] [--json]`: the full Phase 11 query
  surface, dispatched from a `StoredGraph` loaded from `--store` (default
  `./.codeate/graph.json`):
  - `symbol <name>` / `definition --id` -- symbol and definition lookup.
  - `callers --id` / `callees --id` -- call graph queries.
  - `dependencies --id` / `dependents --id` -- transitive `DEPENDS_ON`/`IMPORTS` closure.
  - `implementers --id` -- implementation lookup.
  - `references --id` -- reference lookup.
  - `impact --id` -- transitive impact analysis.
  - `entry-points` / `module-tree` -- repository exploration.
  - `--id`-targeted commands take an opaque `EntityId` (never a bare name, since
    same-named symbols in different files are legitimately distinct) obtained from
    `query symbol`; an unknown id is a hard error, distinct from a legitimately empty
    result (e.g. a symbol with no callers).
- Exit code convention: `0` for success, including empty-but-valid query results; `1`
  for runtime errors (bad path, missing/corrupt store, unknown id); `2` for usage
  errors (clap's default for bad args or a missing subcommand).
- 22 new `codeate-cli` integration tests (25 total, up from 3), including one
  end-to-end test running a single `analyze` through every `query` subcommand in both
  human-readable and `--json` output modes.

### Verified

- `scripts/check.ps1` (`cargo fmt --check`, `cargo clippy --workspace --all-targets`,
  `cargo test --workspace`) passes: 208 tests total (25 `codeate-cli` integration, 169
  `codeate-core` unit, 3 symbols end-to-end, 1 golden-file, 4 model invariant, 1 detect,
  5 ingest).

## [Phase 11] — Query Engine

### Added

- `pipeline::analyze_repository` now also wires `DEFINED_IN` edges
  (`symbols::defined_in_edges`) into its output -- a gap found while building the query
  engine's definition lookup, which needs them.
- `QueryEngine` (`query.rs`), operating purely over an already-built `Graph` -- no
  re-parsing, re-resolving, or inference:
  - `symbols_named`/`symbol_by_id`/`definition_of` -- symbol and definition lookup.
  - `callers_of`/`callees_of`, built on `CALLS` -- call graph queries.
  - `dependencies_of`/`dependents_of`, a transitive closure over `DEPENDS_ON`/`IMPORTS`.
  - `implementers_of`, built on `IMPLEMENTS` -- implementation lookup.
  - `references_to`, built on `REFERENCES` -- returns the enclosing function/method
    symbol Phase 7 attributes a reference to, not a fine-grained expression site.
  - `impact_of`, a transitive reverse walk over every edge kind representing "the
    source needs the target" (`CALLS`/`REFERENCES`/`IMPLEMENTS`/`EXTENDS`/`OVERRIDES`/
    `DEPENDS_ON`/`IMPORTS`) -- impact analysis.
  - `entry_points` (functions/methods named `main`) and `module_tree` (the `CONTAINS`
    hierarchy of `Module` entities, as a nested `ModuleTree`) -- repository exploration.

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (186 tests total: 169 `codeate-core` unit, 3 symbols
  end-to-end, 1 golden-file, 4 model invariant, 1 detect, 5 ingest, 3 CLI) via
  `scripts/check.ps1`.

## [Phase 10] — Persistent State Store

### Added

- Serde derives on the canonical model (`EntityId`, `SyntaxKind`/`SyntaxEntity`,
  `SymbolKind`/`SymbolEntity`, `EdgeKind`/`Edge`): purely additive, no behavior change to
  any existing method.
- `StoredGraph` (`store.rs`): the full graph (syntax entities, symbols, edges) plus an
  explicit schema `version`, serialized as pretty-printed JSON via `serde_json` -- this
  doubles as the "export to JSON" operation, with no separate format to maintain.
- `save()`/`load()`: round-trip a `StoredGraph` to/from a file exactly. `load()` rejects
  a file whose `version` is newer than this build's `CURRENT_VERSION` with a clear error
  rather than silently misreading it; migrating an older-but-still-supported version
  forward is deferred to whenever a version bump actually happens.
- `compare()`: loads two saved states and diffs the graphs they build into, via Phase
  9's `graph::diff`.
- `export_graphml()`: GraphML output for external tools (Gephi, yEd, ...), with proper
  XML escaping for names containing special characters -- a presentation format, not
  meant to round-trip back into Codeate (that's `StoredGraph`'s job).
- New dependency: `serde_json`.

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (177 tests total: 160 `codeate-core` unit, 3 symbols
  end-to-end, 1 golden-file, 4 model invariant, 1 detect, 5 ingest, 3 CLI) via
  `scripts/check.ps1`.

## [Phase 9] — Graph Builder

### Added

- Repository resolution pipeline (`pipeline::analyze_repository`, new): the missing
  piece discovered while closing Phase 8 -- given a `Repository`, parses every Rust
  file, extracts with `ResolutionSites` (a new side-channel `extract_items` populates
  during its existing walk, carrying the raw `syn` references -- function bodies, impl
  blocks, trait-impl methods, traits -- that Phase 6-8's resolvers need but don't fit
  the plain `(Vec<SyntaxEntity>, Vec<Edge>)` `extract()` returns), merges symbol tables
  repository-wide, then runs reference/call/instantiate resolution per body and
  structural resolution (`EXTENDS`/`IMPLEMENTS`/`OVERRIDES`/`IMPORTS`/`EXPORTS`/
  `DEPENDS_ON`) against the full repository's entities and merged symbol table -- built
  TDD, 3 tests including a real two-file cross-file case through actual disk files.
- `Graph` (`graph.rs`): unifies syntax and symbol entities behind one `Node` type,
  indexed by `EntityId` for O(1) lookup, with outgoing/incoming adjacency lists
  (optionally filtered by `EdgeKind`). `Graph::build()` is the single construction entry
  point, taking an `AnalysisResult`'s fields -- built TDD, 2 unit tests.
- Structural integrity checks: `Graph::build` returns an `IntegrityReport` alongside the
  graph -- dangling edges (endpoint not in the node set), orphaned nodes (no edges at
  all; not necessarily invalid, since an unresolved placeholder is expected to be one),
  and invalid cycles (a self-referential `EXTENDS`/`IMPLEMENTS`/`OVERRIDES` edge, as
  opposed to a self-referencing `CALLS` edge from plain recursion, which is fine).
  Nothing is dropped or panicked on -- a dangling edge is still indexed and also
  reported -- built TDD, 4 unit tests.
- Deterministic diffing: `Graph::canonical_form()` sorts nodes and edges into a fixed
  textual form so `HashMap`-driven construction order never affects the result;
  `Graph::fingerprint()` hashes that form via the Phase 1 `hash_bytes` primitive.
  `graph::diff(a, b)` reports added/removed nodes and edges between two graphs by
  value -- built TDD, 5 unit tests (including same-input-twice producing an identical
  fingerprint and an empty diff).

### Fixed

- Every resolver function added since Phase 5 (type/field/signature/generic
  extraction, reference/call/instantiate resolution, Phase 8's structural resolvers)
  is now genuinely reachable from `analyze_repository`, removing the `#[allow(dead_code)]`
  annotations added while closing Phase 8 -- except `extract_struct_fields`/
  `extract_signature_types`/`extract_generic_params` (Phase 6's struct-field and
  signature-type extraction), which remain a narrower, explicitly-noted open gap: they
  still aren't called from `extract_items`, so struct fields' and function signatures'
  own type symbols don't yet appear in the pipeline's output. Left for a future task
  rather than force-wired under time pressure.

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass (169 tests total: 152 `codeate-core` unit, 3
  symbols end-to-end, 1 golden-file, 4 model invariant, 1 detect, 5 ingest, 3 CLI) via
  `scripts/check.ps1`.

## [Phase 8] — Structural Relationships

### Added

- `EXTENDS`/`IMPLEMENTS` resolution (`resolve_implements`, `resolve_supertraits` in
  `adapters/rust.rs`): `impl Trait for Type` blocks resolve to an `IMPLEMENTS` edge from
  the implementing `Struct`/`Enum` symbol to the trait's `Interface` symbol; `trait Sub:
  Super` supertrait bounds resolve to `EXTENDS` between `Interface` symbols. An inherent
  impl (no `for Trait`) produces nothing. Either side falls back to an explicit
  `unresolved_symbol` placeholder (`external-type`/`external-trait`) when it isn't
  declared in this repository -- built TDD, 4 unit tests.
- Trait method declarations: the Rust adapter now declares a trait's own `fn` items as
  `Method` entities under the trait (previously only impl methods got a `Method`
  entity), giving `OVERRIDES` resolution a real trait-side symbol to point at. Updated
  the Phase 4 golden fixture's expected `Method` count (1 -> 2: `magnitude` + `area`)
  accordingly.
- `OVERRIDES` resolution (`resolve_overrides`): an impl method resolves to an `OVERRIDES`
  edge at the trait's matching method by walking that specific trait entity's own
  `DECLARES` edges (not a repo-wide name scan, so same-named methods on unrelated traits
  never cross-match). No trait counterpart produces no edge; an external trait produces
  an explicit `external-trait-method` placeholder -- built TDD, 3 unit tests.
- `IMPORTS`/`EXPORTS` resolution (`resolve_imports`, `resolve_exports`): `use` trees
  (paths, groups, renames, globs) resolve `crate`/`self`/`super`-rooted targets against
  the repository's declared items into `IMPORTS` edges from the file's module symbol;
  anything else (external crates, `std` included) gets an explicit `external-crate`
  placeholder, and an unresolvable local path gets its own `unresolved-local-import`
  placeholder. `pub` top-level items get `EXPORTS` edges from the file's module symbol
  -- built TDD, 5 unit tests.
- `DEPENDS_ON` resolution (new language-agnostic `resolve/structural.rs`):
  `resolve_depends_on()` collapses `IMPORTS`/`CALLS`/`REFERENCES`/`INSTANTIATES` edges
  into deduplicated file-to-file `DEPENDS_ON` edges via an explicit owner-file map;
  same-file edges and edges to unmapped (unresolved) targets contribute nothing -- built
  TDD, 5 unit tests.
- Cross-file resolution: all of the above take a repository-wide entity list and merged
  symbol table rather than a single file's scope, so they resolve across files without
  further wiring -- confirmed with 3 dedicated cross-file tests (`IMPLEMENTS` against a
  trait declared in another file, `IMPORTS` against an item declared in another file, and
  an end-to-end `DEPENDS_ON` test built on real two-file adapter output).
- Explicit-unresolved audit: every new edge kind (`EXTENDS`, `IMPLEMENTS`, `OVERRIDES`,
  `IMPORTS`, `EXPORTS`) has a dedicated test for its "target not in this repository"
  case; `EXPORTS` has no unresolved case by construction (an exported item is always
  declared in the exporting file) and `RETURNS`/`ACCEPTS` were already covered in Phase
  6.

### Fixed

- `scripts/check.ps1` never actually enforced its own steps: `$ErrorActionPreference =
  "Stop"` only catches PowerShell/cmdlet errors, not a native command (`cargo`)
  returning a nonzero exit code, so a failing `cargo clippy -- -D warnings` (or even
  `cargo test`) fell straight through to a printed "All checks passed." Discovered while
  re-verifying Phase 8 after an unrelated local toolchain reinstall. Fixed with an
  explicit `$LASTEXITCODE` check after every step.
- With that fixed, `clippy -D warnings` failed for real: every resolver function added
  since Phase 5 (type/field/signature/generic extraction, reference/call/instantiate
  resolution, and this phase's own structural resolvers) is a private helper never
  called outside its own unit tests -- none of it is wired into `LanguageAdapter`'s
  `resolve_symbols`/`resolve_references`, or into any repository-level pass, because
  that pass doesn't exist yet. Annotated each with `#[allow(dead_code)]` and a comment
  naming Phase 9's repository pipeline as the intended integration point, rather than
  force-wiring per-file-only trait methods to functions that need repository-wide
  context (this phase's own cross-file resolvers) or silently deleting tested,
  correct code. Phase 9 removes these allows as it wires the real pipeline together.

### Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` all pass, for real this time (155 tests total: 138
  `codeate-core` unit, 3 symbols end-to-end, 1 golden-file, 4 model invariant, 1 detect,
  5 ingest, 3 CLI) via the fixed `scripts/check.ps1`.

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
