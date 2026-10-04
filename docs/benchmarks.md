# Benchmarks

## Purpose

A tool-vs-tool comparison of Codeate against two other deterministic, offline,
no-LLM code-analysis tools — **Code Review Graph** and **Graphify** (PyPI package
`graphifyy`, CLI binary `graphify`) — on real-world repositories, one per supported
language. This measures each tool's own analysis performance, determinism, and
entity/edge coverage, run directly via each tool's own CLI. It is not an
agent-task-success benchmark: it does not measure how well an LLM/coding agent
performs with vs. without any of these tools.

## What is measured, and why

- **Cold run — wall-clock time for a from-scratch analysis.** The baseline cost of
  pointing each tool at a repository it has never seen: `codeate analyze --full`,
  `code-review-graph build`, `graphify update --no-cluster` against a fixture with
  no prior output. Timed end-to-end with a wall-clock stopwatch around the whole
  process. **N=3 runs per tool per language; the median is reported** (a median
  absorbs one-off OS/filesystem cache warm-up noise better than a mean would).
- **Incremental run — wall-clock time to re-analyze after one small change.** The
  cost that matters for a tool used repeatedly during development, where only a
  little changed since the last analysis. Methodology: one deterministic no-op edit
  (a trailing blank line appended to the fixture's first source file, committed to
  the fixture's local git history) followed by each tool's own incremental command:
  `codeate analyze` (no `--full`), `code-review-graph update` (git-diff-based,
  via `--base`), `graphify update --no-cluster` again.
- **Peak memory — the most memory any single run uses.** Sampled via a 50ms poll
  loop over the running process's own working set **plus every live descendant
  process's**, summed per poll and maxed across the run. Code Review Graph and
  Graphify both fork worker subprocesses for parallel AST extraction, so only
  counting the top-level process would significantly under-report their real
  memory cost. Windows doesn't expose peak RSS after a process exits, so this has
  to be sampled while the process tree is still alive.
- **Determinism — same input, same output, run twice.** Two independent cold runs,
  compared structurally rather than as a raw byte/file hash, since none of the
  three tools guarantees stable serialization ordering (JSON key/array order,
  SQLite row order) — a byte-hash comparison would flag false divergences on a tool
  that is logically deterministic but doesn't promise byte-stable output. Codeate's
  check diffs the actual entity/symbol/edge sets (`codeate-bench determinism
  diff`); the competitor tools' checks compare node/edge **counts** via their own
  `status --json` (Code Review Graph) or `graph.json` array lengths (Graphify) — a
  fair, same-semantic-content bar, though coarser than Codeate's own id-level diff.
- **Coverage/depth — what each tool's graph actually contains, not a ranking.** The
  entity/edge **kinds** present in each tool's own output from a cold run,
  with observed counts for each — Codeate's via `codeate-bench coverage summarize
  <graph.json>` (groups `syntax_entities`/`symbols`/`edges` by their `SyntaxKind`/
  `SymbolKind`/`EdgeKind`), Code Review Graph's via its `nodes.kind`/`edges.kind`
  SQLite columns, Graphify's via its `nodes[].type`/`links[].relation` JSON fields.
  A kind that occurred zero times in a given fixture is omitted, not listed as
  zero — the same convention all three tools' rows follow. This table exists to
  show what each tool models, not to declare a winner.
- **Fixtures.** One real-world repository per language, pinned by commit, fetched
  fresh as a single-commit tarball snapshot (no `git clone`, no upstream history —
  see `benches/fixtures-manifest.json` and `scripts/fetch-benchmark-fixtures.ps1`).
  A fixture's own deliberately-invalid test data (e.g. a compiler-error-message test
  fixture) is excluded via a generated `.codeateignore`, listed per-entry in the
  manifest's `ignore_globs`.
- **Hardware/OS.** Windows 10.0.26200, Intel Core i3-10110U @ 2.10GHz, ~15.8GB RAM.
- **Tool versions.** Codeate 0.1.0 (this repo, release build), Code Review Graph
  2.3.9 (pip, isolated venv), Graphify 0.9.75 (`graphifyy` via `uv tool install`) —
  see `scripts/setup-benchmark-venv.ps1`.
- **Token-efficiency and a CI regression gate are not part of this pass** — both
  are out of scope for now per Phase 18.1's own planning decisions.

## Results

### Rust — `rust-lang/cargo`

Fixture: commit `bb2126cffae48394a37db8728dc50a17bbfe54d4`, Apache-2.0, 650 files
Codeate registers a language for (cargo's own `tests/testsuite/` and
`crates/rustfix/tests/` excluded — deliberately-invalid compiler-error-message test
fixtures, not representative source; see the manifest's `ignore_globs`).

| Language | Tool | Cold (s) | Incremental (s) | Peak Memory (MB) | Deterministic |
|---|---|---|---|---|---|
| rust | codeate | 18.42 | 6.00 | 175.8 | yes |
| rust | code-review-graph | 55.24 | 1.68 | 364.8 | yes |
| rust | graphify | 67.75 | 60.19 | 450.0 | yes |

#### Coverage/depth — Rust

| Tool | Entity/node kinds observed | Edge/relation kinds observed |
|---|---|---|
| codeate | `Expression` (6274), `Parameter` (6031), `Variable` (5243), `Method` (3066), `Statement` (2300), `Function` (1885), `Struct` (564), `Module` (740), `Test` (343), `Enum` (209), `Dependency` (207), `Interface` (32), `BuildTarget` (31), `EntryPoint` (30), `Package` (29), `Class` (1) — plus `SymbolKind`: `Symbol` (14205), `Type` (929), `Method` (2685), `Namespace` (760), `Function` (1860), `Constant` (207) | `REFERENCES` (20228), `CONTAINS` (26252), `CALLS` (19980), `DEPENDS_ON` (6661), `DECLARES` (6136), `DEFINED_IN` (6136), `IMPORTS` (5188), `OVERRIDES` (695), `INSTANTIATES` (672), `EXPORTS` (1083), `IMPLEMENTS` (516), `EXTENDS` (7) |
| code-review-graph | `Function` (9355), `File` (1381), `Class` (871), `Test` (558) | `CALLS` (108943), `CONTAINS` (10905), `IMPORTS_FROM` (7340), `TESTED_BY` (5361), `IMPLEMENTS` (520), `REFERENCES` (197) |
| graphify | Mostly untyped: 17989/19237 node records have no `type` field; the few that do are `package` (556) or `external` (692) | `calls` (8481), `references` (12636), `contains` (10989), `imports_from` (6682), `method` (3064), `case_of` (696), `depends_on` (692), `implements` (231), `defines` (11), `inherits` (1), `imports` (5) |
