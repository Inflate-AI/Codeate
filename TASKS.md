# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 14 — Repository-Level State

Approved breakdown:

1. **Manifest parsing** — `Cargo.toml` → `Package` entities (existing `SyntaxKind`)
   and a new `Dependency` entity kind + edges, workspace-aware.
2. **Build Target & Configuration entities** — `[[bin]]`/`[lib]`/`[[test]]`/
   `[[bench]]`/`[[example]]` → `BuildTarget`; `[profile.*]` sections → `Configuration`.
3. **Test entity + discovery** — detect `#[test]`-annotated functions, wrap as `Test`
   entities linked to what they exercise via existing `CALLS` edges (best-effort).
4. **Entry Point detection** — binary `main` functions and library crate-root `pub`
   exports, tied to their owning `BuildTarget`/`Package`.
5. **API Endpoint & Generated Code (best-effort)** — recognize common Rust
   web-framework route attributes as `ApiEndpoint`; flag generated files via
   `// @generated`-style markers.
6. **Graph & query integration** — wire new entities/edges into `Graph::build`, add
   query kinds (`dependencies`, `entry-points`, `tests`, ...) to the Phase 11 engine
   and Phase 12 CLI.
7. **End-to-end tests** — fixture repo with real `Cargo.toml`, deps, targets, tests,
   entry points → `analyze` → `query` → expected output.
