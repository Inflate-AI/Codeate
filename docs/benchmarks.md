# Benchmarks

## Purpose

A tool-vs-tool comparison of Codeate against two other deterministic, offline,
no-LLM code-analysis tools — **Code Review Graph** and **Graphify** (PyPI package
`graphifyy`, CLI binary `graphify`) — on real-world repositories, one per supported
language.

**This is explicitly not an agent-task-success benchmark.** It does not measure how
well an LLM/coding agent performs with vs. without any of these tools; it measures
each tool's own cold/incremental analysis performance, determinism, and
entity/edge-type coverage, run directly via each tool's own CLI.

## Methodology

- **Fixtures.** One real-world repository per language, pinned by commit, fetched
  fresh per run as a single-commit tarball snapshot (no `git clone`, no upstream
  history) — see `benches/fixtures-manifest.json` and
  `scripts/fetch-benchmark-fixtures.ps1`. A fixture's own deliberately-invalid test
  data (e.g. a compiler-error-message test fixture) is excluded via a generated
  `.codeateignore`, documented per-entry in the manifest's `ignore_globs`.
- **Hardware/OS.** Windows 10.0.26200, Intel Core i3-10110U @ 2.10GHz, ~15.8GB RAM.
- **Tool versions.** Codeate 0.1.0 (this repo, release build), Code Review Graph
  2.3.9 (pip, isolated venv), Graphify 0.9.75 (`graphifyy` via `uv tool install`).
  See `scripts/setup-benchmark-venv.ps1`.
- **Cold run.** A fresh analysis with no prior state: `codeate analyze --full`,
  `code-review-graph build`, `graphify update --no-cluster` (its first run against a
  fixture with no existing `graphify-out/`). Timed via a wall-clock `Stopwatch`
  wrapping the whole process; **N=3 runs per tool per language, median reported**
  (not mean — a couple of runs always include one-time OS/filesystem cache warm-up
  noise that a median absorbs better than a mean).
- **Incremental run.** One deterministic no-op edit (a trailing blank line appended
  to the fixture's first source file, then committed to the fixture's local
  single-commit git history so Code Review Graph's git-diff-based `update --base`
  has something to diff against) followed by each tool's own incremental command:
  `codeate analyze` (no `--full`), `code-review-graph update`, `graphify update
  --no-cluster` again. **Caveat:** because the shared no-op edit is made once before
  both competitor-tool sections run, Graphify's own cold-run loop already absorbed
  that edit by the time its "incremental" step runs — so Graphify's incremental
  number here measures a true no-op re-check (nothing new changed), while Codeate's
  and Code Review Graph's incremental numbers measure one real pending change. This
  asymmetry is called out per-language rather than silently normalized away.
- **Peak memory.** Sampled via a 50ms poll loop over the process's own
  `PeakWorkingSet64` **plus every live descendant process's**, summed per poll and
  maxed across the run — both Code Review Graph and Graphify fork worker
  subprocesses for parallel AST extraction, so measuring only the top-level process
  under-reports by a large margin (an earlier version of this harness did exactly
  that; see git history). Windows doesn't expose peak RSS after a process exits, so
  this must be sampled live.
- **Determinism.** Two independent cold runs, compared structurally, **not as a raw
  byte/file hash** — none of the three tools guarantees stable serialization
  ordering (JSON key/array order, SQLite row order), so a byte-hash comparison would
  flag false divergences on tools that are logically deterministic but don't
  promise byte-stable output. Codeate's check diffs the actual `StoredGraph`
  entity/symbol/edge sets (`codeate-bench determinism diff`); the competitor tools'
  checks compare node/edge **counts** via their own `status --json` (Code Review
  Graph) or `graph.json`'s `nodes`/`links` array lengths (Graphify) — a fair,
  like-for-like bar (same semantic content, not same bytes), but a coarser one than
  Codeate's own id-level diff.
- **Coverage/depth — not a race.** A separate table per language: the entity/edge
  **kinds** each tool's own output actually contains, read directly off a cold run's
  result (Codeate's closed `SyntaxKind`/`EdgeKind` enums; Code Review Graph's
  `nodes.kind`/`edges.kind` SQLite columns; Graphify's `nodes[].type`/
  `links[].relation` JSON fields), not a ranking.
- **Token-efficiency.** Deferred — not attempted in this pass. Will be revisited
  once every language's core metrics are in, and only implemented if a fair
  comparison turns out to be constructible (Graphify ships its own `benchmark`
  subcommand making a token-reduction claim against naive full-corpus analysis,
  which is a different comparison than what this table would need).
- **Explicitly out of scope.** No README claims from these numbers (deferred), no CI
  regression gate on them yet (deferred) — both per Phase 17/18.1's own planning
  decisions.

## Results

### Rust — `rust-lang/cargo`

Fixture: commit `bb2126cffae48394a37db8728dc50a17bbfe54d4`, Apache-2.0, 650 files
Codeate registers a language for (cargo's own `tests/testsuite/` and
`crates/rustfix/tests/` excluded — deliberately-invalid compiler-error-message test
fixtures, not representative source; see the manifest's `ignore_globs`).

| Language | Tool | Cold (s) | Incremental (s) | Peak Memory (MB) | Deterministic |
|---|---|---|---|---|---|
| rust | codeate | 11.04 | 4.81 | 175.6 | yes |
| rust | code-review-graph | 36.59 | 1.67 | 358.9 | yes |
| rust | graphify | 43.31 | 39.57 (see incremental-run caveat above — this is a no-op re-check, not a one-file-change measurement) | 446.9 | yes |

Codeate is the fastest cold analysis and the lightest on peak memory of the three.
Code Review Graph's incremental update (1.67s) is the fastest of the three for an
actual one-file change, via its git-diff-based change detection. Graphify's
"incremental" number isn't directly comparable per the caveat above — a true
one-file-change run would need the edit made strictly between its own cold loop and
its incremental step, which the current shared-edit ordering doesn't do; worth
fixing before drawing a real conclusion about Graphify's incremental performance
specifically.

#### Coverage/depth — Rust

| Tool | Entity/node kinds observed | Edge/relation kinds observed | Notes |
|---|---|---|---|
| codeate | Closed `SyntaxKind` enum (Module, Class, Struct, Enum, Function, Method, Variable, Parameter, Statement, Expression, Package, Dependency, BuildTarget, Configuration, Test, EntryPoint, ApiEndpoint, GeneratedCode — 18 variants) + `SymbolKind` | Closed `EdgeKind` enum, 14 variants: CONTAINS, IMPORTS, EXPORTS, DECLARES, REFERENCES, CALLS, EXTENDS, IMPLEMENTS, OVERRIDES, INSTANTIATES, RETURNS, ACCEPTS, DEPENDS_ON, DEFINED_IN | Full symbol/type/reference resolution; every edge kind is one of a closed, documented set — no catch-all "other." |
| code-review-graph | `Function` (9355), `File` (1381), `Class` (871), `Test` (558) — 4 kinds observed on this fixture | `CALLS` (108943), `CONTAINS` (10905), `IMPORTS_FROM` (7340), `TESTED_BY` (5361), `IMPLEMENTS` (520), `REFERENCES` (197) — 6 kinds observed | Multi-language-aware **within one repo**: this same cargo fixture registered files in bash/javascript/python/rust (1381 files vs. Codeate's 650 Rust-only files) — a real coverage advantage for polyglot repos Codeate doesn't currently match in a single `analyze` run across differently-adapted languages' combined graph. |
| graphify | Mostly untyped: 17989/19237 node records have no `type` field at all (just id/label/source location); the few that do are `package` (556) or `external` (692) | 11 relation kinds on edges: `calls` (8481), `references` (12636), `contains` (10989), `imports_from` (6682), `method` (3064), `case_of` (696), `depends_on` (692), `implements` (231), `defines` (11), `inherits` (1), `imports` (5) | Structure lives mostly in edge relations, not node types — closer to a generic knowledge-graph model than Codeate's/Code Review Graph's typed-node schema. |

## Findings surfaced by this benchmark (not just numbers)

Running Codeate against a large, real multi-crate workspace (rather than Codeate's
own single-crate repo, which is all its existing dogfooding/test fixtures cover)
surfaced a real correctness bug, fixed in the same session this benchmark was run:
`run_pipeline`'s merge step concatenated carried-in symbols with a freshly-rebuilt
symbol table with no deduplication, inflating the raw `AnalysisResult`/`StoredGraph`
symbol count by ~44% after a single-file incremental edit (and, separately, a plain
full/cold analyze already had ~2x internal duplication from the same root cause).
Graph-based query correctness was unaffected (`Graph::build`'s `HashMap<EntityId,
Node>` silently deduped by id), which is exactly why the existing
`incremental_reanalysis_of_a_modified_file_matches_a_full_rebuild`-style tests never
caught it — they only ever compared the deduped `Graph` form. See
`crates/codeate-core/src/pipeline.rs`'s `run_pipeline` and the
`incremental_reanalysis_of_a_modified_file_does_not_duplicate_carried_in_symbols`
regression test. This is exactly the kind of gap real-world-scale benchmarking is
meant to surface that small, single-crate fixtures don't.
