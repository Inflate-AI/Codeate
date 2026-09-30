# Roadmap

Phases of implementation for Codeate, derived from the architecture and design in
`README.md`. Phases are ordered — each one builds on artifacts the previous phase
produced. Phases are only added/reordered with user approval. See `CLAUDE.md` for the
workflow each phase follows (plan → approve → clean git check → TDD → Happenings review
→ structured commits).

Status legend: `planned` → `in progress` → `done`

---

## Phase 0 — Project Foundations
**Status:** done

Rust workspace scaffold and baseline tooling so every later phase has somewhere to land.

- Cargo workspace layout (e.g. `core`, `cli` crates from day one; more crates split off
  as later phases need them — no premature splitting).
- `rustfmt`, `clippy` wired in; CI-equivalent local check script.
- Base error-handling and logging conventions for the workspace.
- Empty but runnable `codeate` CLI binary (`--version`, `--help`).

---

## Phase 1 — Repository Ingestion
**Status:** done

*Architecture stage: Repository Ingestion.*

Deterministically enumerate the files that make up a repository.

- File tree walker with `.gitignore`-aware and `.codeateignore`-aware traversal.
- Stable `File` and `Repository` entities (paths, content hashes, sizes).
- Repository-level config file (analysis scope, excludes, target languages).
- Determinism guarantee: same repo + same config → same file set, same order.

---

## Phase 2 — Language Detection
**Status:** done

*Architecture stage: Language Detection.*

Classify each ingested file by language, independent of any parser.

- Extension- and content-based language detection.
- Pluggable `LanguageId` registry that later language adapters register into.
- Per-file detection result attached to the `File` entity from Phase 1.
- Unknown/unsupported languages are represented explicitly, not dropped silently.

---

## Phase 3 — Canonical Code Model
**Status:** done

*Architecture stage: Canonical Model (defined early so every later stage targets a
stable schema instead of language-specific ASTs).*

Define the language-agnostic data model all adapters produce and all consumers read.

- Syntax entities: `Module`, `Package`, `Class`, `Interface`, `Struct`, `Enum`,
  `Function`, `Method`, `Variable`, `Parameter`, `Statement`, `Expression`.
- Symbol entities: `Symbol`, `Type`, `Field`, `Method`, `Function`, `Constant`,
  `Namespace`, `Generic`.
- Relationship edge types as a closed enum: `CONTAINS`, `IMPORTS`, `EXPORTS`,
  `DECLARES`, `REFERENCES`, `CALLS`, `EXTENDS`, `IMPLEMENTS`, `OVERRIDES`,
  `INSTANTIATES`, `RETURNS`, `ACCEPTS`, `DEPENDS_ON`, `DEFINED_IN`.
- Stable, language-independent IDs for every entity (for graph edges and persistence).
- No parser targets this model yet — this phase is the schema and its invariants only.

---

## Phase 4 — First Language Adapter: Parsing & AST Extraction
**Status:** done

*Architecture stage: Language Adapters → AST / Syntax Data.*

Prove the adapter architecture end-to-end with one real language, mapping its AST into
the Phase 3 canonical syntax entities.

- Define the `LanguageAdapter` trait (`Parser`, `AST Extractor`, plus stubs for the
  resolvers added in later phases).
- Implement the first adapter (Rust, via a Rust parsing crate) covering: files,
  modules, structs, enums, traits (→ interfaces), functions, methods, params,
  variables, top-level statements/expressions.
- Adapter output is canonical syntax entities + `CONTAINS`/`DECLARES` edges — no symbol
  resolution yet.
- Golden-file tests: fixture source → expected canonical syntax tree.

---

## Phase 5 — Symbol Resolution
**Status:** done

*Architecture stage: Symbol & Type Resolution (symbols).*

Resolve declared names to `Symbol` entities with stable identity and scope.

- Scope model (module/namespace/block scoping) built from Phase 4 output.
- Symbol table construction per file, then merged at repository scope.
- `DEFINED_IN` edges from symbols to their declaring file/module.
- Shadowing, redeclaration, and duplicate-symbol handling defined deterministically.

---

## Phase 6 — Type Resolution
**Status:** done

*Architecture stage: Symbol & Type Resolution (types).*

Resolve static type information for symbols, fields, parameters, and return values.

- `Type` entity resolution: primitives, user-defined types, generics.
- Field/parameter/return type binding onto Phase 5 symbols.
- Generic parameter and constraint representation.
- Unresolvable/external types represented explicitly (not silently dropped).

---

## Phase 7 — Reference & Call Resolution
**Status:** done

*Architecture stage: Semantic Analysis (references, calls).*

Resolve usages back to the symbols they refer to.

- `REFERENCES` edges: every identifier use resolved to its declaring symbol.
- `CALLS` edges: call-site resolution to callee symbol(s), including simple
  polymorphic/dynamic-dispatch cases via `OVERRIDES`/`IMPLEMENTS` awareness.
- `INSTANTIATES` edges for constructor/object-creation sites.
- Unresolved references represented explicitly with a reason (e.g. external crate).

---

## Phase 8 — Structural Relationships
**Status:** done

*Architecture stage: Semantic Analysis (dependencies, structure).*

Fill in the remaining relationship edges that describe structural relationships
between types and modules.

- `EXTENDS`, `IMPLEMENTS`, `OVERRIDES` for inheritance/trait-implementation hierarchies.
- `RETURNS`, `ACCEPTS` for function/method signatures.
- `IMPORTS`, `EXPORTS`, `DEPENDS_ON` at module/file granularity.
- Cross-file relationship resolution (not just within a single file's scope).

---

## Phase 9 — Graph Builder
**Status:** done

*Architecture stage: Graph Builder.*

Assemble the canonical entities and edges from Phases 3–8 into the unified **Code
Graph**.

- Graph construction from the accumulated entity/edge stream.
- Structural integrity checks (dangling edges, orphaned nodes, cycles where invalid).
- In-memory graph API used by later phases (state store, query engine).
- Deterministic graph diffing (same inputs → identical graph, byte-for-byte where
  serialized).

---

## Phase 10 — Persistent State Store
**Status:** done

*Architecture stage: State Store.*

Persist the Code Graph so analysis doesn't restart from zero on every invocation.

- On-disk storage format for the graph (symbols, types, files, relationships,
  dependencies, metadata).
- Load/save round-trip with integrity verification.
- Compare/export operations (diff two stored states; export to common formats e.g.
  JSON/GraphML for external tools).
- Storage versioning so schema changes (from later phases) don't corrupt old state.

---

## Phase 11 — Query Engine
**Status:** done

*Architecture stage: Query Engine.*

Implement the deterministic query surface described in the README.

- Symbol lookup, definition lookup.
- Call graph queries (callers of / callees of).
- Dependency analysis and reverse dependency analysis.
- Implementation lookup (interface → implementers).
- Reference lookup (symbol → all reference sites).
- Impact analysis (symbol/file change → affected code).
- Repository exploration (entry points, module tree).
- Query engine operates purely against Phase 10's persisted state — no re-parsing.

---

## Phase 12 — Developer CLI
**Status:** done

*Architecture stage: Query / API / CLI (CLI surface).*

Expose ingestion, analysis, and querying through the `codeate` binary from Phase 0.

- `codeate analyze <path>` — run full pipeline (Phases 1–10) and persist state.
- `codeate query <kind> <args>` — run Phase 11 queries, human-readable and JSON output.
- Clear exit codes and error messages suitable for scripting/CI use.
- End-to-end tests: real fixture repo → `analyze` → `query` → expected output.

---

## Phase 13 — Incremental Analysis
**Status:** done

*Architecture stage: Incremental Analysis.*

Avoid full re-analysis on every change.

- Change detection against persisted state (via Phase 1 content hashes).
- Affected-structure computation (which files/symbols/edges are invalidated by a
  change, using the graph's own edges to find blast radius).
- Re-analyze only affected structures (Phases 4–9) and patch the persisted graph
  (Phase 10) rather than rebuilding it.
- Correctness check: incremental result == full-rebuild result, for a fixture set of
  changes.

---

## Phase 14 — Repository-Level State
**Status:** done

*Architecture stage: Repository State.*

Higher-level repository facts beyond code structure.

- `Package`, `Dependency` entities (from manifest files, starting with `Cargo.toml`).
- `Build Target`, `Configuration` entities.
- `Test` entity (test discovery, linkage to the function/method it tests).
- `Entry Point` detection (binaries, `main`, exported library roots).
- `API Endpoint` and `Generated Code` representation (best-effort, adapter-dependent).
- These integrate into the Phase 9 graph and are queryable via Phase 11.

---

## Phase 15 — Second Language Adapter
**Status:** done

*Architecture stage: Language Adapters (validating language-agnosticism).*

Prove the canonical model and pipeline (Phases 4–8) generalize beyond the bootstrap
language by implementing a second, architecturally different adapter: **Python**, via
`rustpython-parser`.

Discovered while planning: `pipeline::analyze_repository` does not actually dispatch
through `LanguageAdapter`'s trait methods for resolution today — it calls Rust-specific
free functions directly, and the trait has no hook at all for structural resolution
(implements/overrides/imports/exports). A second adapter can't prove
language-agnosticism without first making that dispatch real, so this phase opens with
that refactor (verified behavior-identical to today) before any Python-specific code is
written.

- Generalize `LanguageAdapter` + pipeline dispatch: extend the trait with the resolution
  hooks Phase 5–8 need, move Rust's existing logic behind them, make the pipeline
  dispatch per-file by `LanguageId` — no behavior change, all existing tests pass
  unchanged.
- Full Python adapter: parser, AST extractor, symbol/type/reference/structural
  resolvers. Phase 5's `build_symbol_table`/`ScopeTree` are already language-agnostic
  and expected to work for Python unchanged.
- Any canonical-model gaps found are fixed in Phase 3's model, not worked around in the
  adapter.
- Cross-adapter test: two fixture repos (Rust + Python) analyzed with identical query
  behavior for equivalent constructs.

---

## Phase 16 — Public API / SDK Layer
**Status:** planned

*Architecture stage: Query / API / CLI (programmatic surface).*

Expose Phase 11's query engine as a stable API for external consumers (IDE tools, code
agents, other developer systems), per README's "Codeate as Infrastructure".

- Stable, versioned API surface (library crate first; wire protocol e.g. JSON-RPC or
  HTTP if/when a real external consumer needs it).
- API-level tests mirroring the CLI's end-to-end tests.
- Documented contract for how an LLM/agent is expected to consume Codeate (query in,
  deterministic facts out — no inference performed by Codeate itself).

---

## Phase 17 — Additional Language Adapters
**Status:** planned

*Architecture stage: Language Adapters (breadth).*

Expand language coverage, one adapter at a time, reusing the Phase 4/15 pattern.
Candidate order (subject to revision based on demand): Go, TypeScript/JavaScript
(if not already Phase 15), Java, C#, Kotlin, C, C++.

- Each language is its own sub-phase with the same shape as Phase 15: planned,
  approved, and closed independently.
- This phase entry is a placeholder tracking the queue; ROADMAP is updated to expand
  each language into its own dated sub-phase when it's picked up.

---

## Phase 18 — Hardening & Release
**Status:** planned

Production-readiness pass once the core pipeline and a handful of language adapters
are stable.

- Performance benchmarking on large real-world repositories.
- Documentation: architecture docs, adapter-authoring guide, query API reference.
- Packaging/distribution (crates.io, binary releases).
- Semantic versioning policy for the canonical model, storage format, and API.

---

## Notes

- Phases 0–14 form the critical path for a single-language, end-to-end deterministic
  pipeline (ingest → parse → resolve → graph → persist → query → CLI → incremental).
- Phase 15 is the first real test of the "language-agnostic" design principle; phases
  before it must not bake in Rust-specific assumptions.
- Phase 17 is intentionally open-ended — new language sub-phases are appended as
  they're approved, rather than trying to fully enumerate all target languages now.
