# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Phase 17 — Additional Language Adapters: closed (see `CHANGELOG.md`)

Closed 2026-10-03 with scope frozen early by explicit user decision: 12 languages
shipped (Go, TypeScript, JavaScript, Java, C#, Kotlin, C, C++, Ruby, Dart, plus
Rust/Python from Phases 4/15) across batches 1-3. The remaining Tier 1 (PHP, Swift,
Scala), all of Tier 2, and Tier 3 are deferred to a future version, not a backlog for
this phase — see `CHANGELOG.md`'s `[Phase 17]` entry and `ROADMAP.md` for the full
deferred list. Batch-by-batch detail (parser crates, deviations, disk-space lessons)
lives in that same `CHANGELOG.md` entry and its sub-phase entries; not reproduced here
per this file's own "past phases can be cleared/archived" rule.

---

## Current phase: Phase 18 — 1.0.0 Beta Hardening & Release

Promoted to the current phase 2026-10-03 when Phase 17 closed. 18.1 now has a
concrete, user-approved implementation plan (full detail below;
see `C:\Users\saran\.claude\plans\will-claude-code-can-melodic-minsky.md` for the
plan-mode record). 18.2-18.7 remain at the earlier draft level and still need their
own planning/approval pass before implementation, same as 18.1 just got.

### Proposed sub-areas (each independently testable, same closing discipline as a
language sub-phase: planned → TDD → Happenings → closed with its own commits)

- [ ] 18.1 **Performance & Comparative Benchmarking.** Benchmark one big real-world
  repo per each of the 12 languages Phase 17 shipped, compared against Code Review
  Graph and Graphify/Graphifyy on identical fixtures (all deterministic, offline,
  no-LLM). See full breakdown below — **approved for implementation 2026-10-03**.
- [ ] 18.2 **Correctness/robustness hardening.** Malformed/edge-case input handling
  across all adapters, property-based/fuzz testing where it fits the determinism
  guarantee.
- [ ] 18.3 **Documentation.** Architecture doc, adapter-authoring guide, query API
  reference, CLI usage docs.
- [ ] 18.4 **Packaging/distribution.** crates.io publish readiness, binary release
  process.
- [ ] 18.5 **Dependency/security audit.** `cargo audit` across the workspace.
- [ ] 18.6 **CI.** Automated `cargo test` + `cargo clippy -- -D warnings` on push/PR.
- [ ] 18.7 **Versioning policy.** SemVer commitments for canonical model, storage
  format, `codeate-api` surface; explicit 1.0.0-beta scope statement.

### 18.1 — Performance & Comparative Benchmarking (approved implementation plan)

**Scope (user-confirmed, including one correction after the first draft):** tool-vs-
tool comparison — Codeate vs. Code Review Graph vs. Graphify/Graphifyy — using **one
big real-world repo per language, 12 fixtures total** (the small-repo tier originally
drafted was dropped: it doesn't meaningfully stress any of these tools). Not an
agent-task-success benchmark.

**Repo candidates (unverified — license/file-count/commit-pin to be confirmed at
implementation time, swap silently if any fails verification):**

| Language | Big repo | Note |
|---|---|---|
| Rust | `rust-lang/cargo` | |
| Python | `django/django` | |
| Go | `etcd-io/etcd` | avoids `kubernetes/kubernetes` |
| TypeScript | `nestjs/nest` | avoids `microsoft/vscode` |
| JavaScript | `webpack/webpack` | |
| Java | `apache/dubbo` | verify file count fits ~1-5k band |
| C# | `dotnet/efcore` | |
| Kotlin | `ktorio/ktor` | |
| C | `redis/redis` | |
| C++ | `facebook/folly` | |
| Ruby | `rails/rails`, subdir `activerecord/`+`activesupport/` only | monorepo scoping |
| Dart | `flutter/flutter`, subdir `packages/flutter/lib/src/` only | monorepo scoping |

**Disk-safety design (D:\ hit 100% full repeatedly during Phase 17 — must not
repeat):** tarball-only fetches (no `git clone`, no `.git` history), one language
fetched/benchmarked/deleted at a time (not all 12 kept at once), a 10GB free-space
floor checked before each fetch.

1. **Manifest + fetch script.** `benches/fixtures-manifest.json` (committed:
   `{language, repo, commit, subdir, license, approx_files, approx_size_mb}` per
   entry) + `scripts/fetch-benchmark-fixtures.ps1` (`-Language <x>` filter,
   `Invoke-WebRequest` + `tar.exe` tarball extraction, subdir-hoisting, free-space
   check, idempotent `.fetched-commit` marker, cleanup flag). `.gitignore`:
   `/benches/fixtures/`, `/benches/results/`.
2. **Isolated comparison-tool environment.** `scripts/setup-benchmark-venv.ps1`:
   `pip install code-review-graph`, `uv tool install graphify`/`graphifyy` (fallback
   `pipx`) into a throwaway venv under the scratchpad directory — **installing now,
   go-ahead already given 2026-10-03**. Records versions + `--help` output (also
   pins down each competitor tool's exact invocation syntax, unverified until this
   step runs).
3. **Metric harness — new workspace member `crates/codeate-bench`.** TDD (failing
   test first): manifest parsing, metric aggregation (mean/median/stdev), determinism
   diff (two `StoredGraph` blobs via `codeate_core`'s existing load path), Markdown
   table formatter. Binary subcommands: `fixtures verify`, `metrics format`,
   `determinism diff`.
4. **Orchestration script.** `scripts/run-benchmarks.ps1` (scaffolding):
   `Measure-ProcessPeakMemory` (Windows live-polling, peak RSS isn't readable
   post-exit), per-language×tool loop (Codeate cold/incremental/determinism via the
   `codeate` CLI already built; competitor tools per their confirmed `--help`
   syntax). Writes `benches/results/<timestamp>/<language>-<tool>.json`.
5. **Coverage/depth comparison — not a race.** Hand-curated
   `benches/coverage-data.json` (entity/edge types each tool extracts, inferred vs.
   explicit), rendered through the same formatter as the performance tables.
6. **Token-efficiency (stretch) — decision deferred.** Revisit once 18.1 steps 1-5
   produce real numbers; implement if a fair comparison is constructible, else
   document why not, in `docs/benchmarks.md` itself.
7. **Results doc.** `docs/benchmarks.md`: purpose/disclaimer, methodology (fixture
   sourcing, hardware/OS, cold-cache definition, N=5 median, tool versions, venv
   repro command), performance tables, coverage table, token-efficiency note,
   explicit out-of-scope callout (no README claims, no CI gate yet).
8. **Regression gate (stretch) — deferred.** One-line note only; not implemented
   this sub-phase.

**Verification:** `cargo test --workspace` covers `codeate-bench`'s own logic and
stays green with the existing 549+ tests; `cargo clippy --workspace --all-targets --
-D warnings` clean including the new crate; an end-to-end dry run on one language
(Rust/`cargo`) before scaling to all 12, confirming the fixture is deleted afterward
and disk free space returns to baseline; a deliberately non-deterministic test case
to confirm `determinism diff` actually catches divergence, not just the happy path.

### 18.2 — Correctness/robustness hardening

1. Malformed-input fixture set per adapter (truncated, syntax errors, bad encoding,
   empty, deeply nested, huge single-line) — no panics, explicit errors.
2. Property-based tests (`proptest`) per language — determinism + crash-freedom.
3. Fix whatever's found before moving on.

### 18.3 — Documentation

1. `docs/architecture.md`, 2. `docs/adapter-authoring-guide.md`,
3. `docs/query-api-reference.md`, 4. CLI usage docs.

### 18.4 — Packaging/distribution

1. Crate metadata for all published crates. 2. `cargo publish --dry-run`.
3. Binary release process (Windows/Linux/macOS).

### 18.5 — Dependency/security audit

1. `cargo audit` against the workspace. 2. Triage/document findings.

### 18.6 — CI

1. `.github/workflows/ci.yml` mirroring `scripts/check.ps1`.
2. Confirm clean on a fresh checkout (no `D:\` toolchain assumptions).

### 18.7 — Versioning policy

1. `docs/versioning-policy.md`: SemVer for canonical model, storage format, API.
2. Explicit 1.0.0-beta scope statement.
