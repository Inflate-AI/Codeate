# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 17 — Additional Language Adapters

Unpaused 2026-09-30. Closing condition narrowed to Tier 1 + Tier 2 (Tier 3 deferred
past this phase; see `ROADMAP.md`). Implemented in parallel batches of 2-3 languages
via git worktrees + subagents, per
`C:\Users\saran\.claude\plans\for-adding-support-for-refactored-spindle.md`.

**Process guardrail (from 17.1's process failure):** a worktree agent implements,
tests, and reports back only. It never updates `CHANGELOG.md`/`TASKS.md`/`ROADMAP.md`
status, never declares its sub-phase closed, and never claims an approval it wasn't
actually given. The coordinator reviews each diff directly, confirms tests/clippy are
actually clean, merges to `main`, presents Happenings to the user, and only then does
closing bookkeeping — after genuine user confirmation, not before.

### Sub-phase queue

**Tier 1 — mainstream general-purpose** (9 remaining)
- [x] 17.1 Go
- [x] 17.2 TypeScript — batch 1, closed
- [x] 17.3 JavaScript — batch 1, closed
- [x] 17.4 Java — batch 1, closed
- [ ] 17.5 C# — batch 2
- [ ] 17.6 Kotlin — batch 2
- [ ] 17.7 C — batch 2
- [ ] 17.8 C++
- [ ] 17.9 Ruby
- [ ] 17.10 PHP
- [ ] 17.11 Swift
- [ ] 17.12 Scala
- [ ] 17.13 Dart

**Tier 2 — scripting / data / infra**
- [ ] 17.14 SQL
- [ ] 17.15 Shell (bash/sh)
- [ ] 17.16 PowerShell
- [ ] 17.17 Perl
- [ ] 17.18 Lua
- [ ] 17.19 R
- [ ] 17.20 Julia

### Batch 1 — TypeScript, JavaScript, Java (closed 2026-09-30, see `CHANGELOG.md`)

Implemented in parallel via 3 worktrees, reviewed and merged sequentially into `main`.
Verified independently (not just agent self-reports) after each merge — full
`cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`
clean, all 5 cross-adapter tests passing.

Lessons for future batches: the shared `CARGO_TARGET_DIR` strategy filled D:\
completely during verification builds (Windows MSVC linker PDB/link limits plus
multiple parsers' build artifacts add up fast) — clear `target/debug/incremental` and
stale `.pdb` files proactively between batches rather than waiting for a hard failure.
One merged worktree directory resisted `git worktree remove` (lingering file handle,
harmless) — pruned from git's tracking; the leftover directory can be removed later
once nothing holds it open.

### Batch 2 — C#, Kotlin, C (in progress)

Same 7-task shape and worktree-per-language process as batch 1. Lessons applied:
`target/debug/incremental` and stale `.pdb` files cleared proactively before starting
(D:\ was at 6.6GB free going in), and each worktree agent is told to clean its own
incremental cache before a final verification build if disk gets tight, rather than
letting it silently fail.

**17.5 C#** — parser: `tree-sitter-c-sharp` (comprehensive C# 1-13 support). Closest
structural template is Java (17.4): explicit `class`/`interface`, explicit base-list
syntax (`class Foo : IBar`) for EXTENDS/IMPLEMENTS — simpler than Go/TS's structural
inference. Watch for: generics with constraints (`where T : IComparable<T>` — same
canonical-model gap Java already found and left open, follow that precedent rather
than re-litigating it), properties (`get`/`set` — map as a Field or a Method pair,
pick one and document it), namespaces for Module mapping, attributes (like Java
annotations — parse past, don't model semantics), nullable reference types (`string?`
— likely fits `TypeRef` as-is, confirm), `async`/`await` (out of scope for control-flow
modeling, just don't crash on it).

**17.6 Kotlin** — parser: `tree-sitter-kotlin`. Closer to C#/Java for explicit
`class`/`interface`/`: SuperType` syntax, but has real divergences to document as
gaps rather than force: extension functions (a function "extending" a type it doesn't
own — no clean canonical-model fit, document as not-extracted), data classes (map as
a normal Class, don't try to model auto-generated equals/hashCode/copy), sealed
classes and `when` expressions (structural extraction only, no exhaustiveness
checking), companion objects, nullable types (`T?` — same `TypeRef` question as C#).

**17.7 C** — parser: `tree-sitter-c`. Structurally the most different adapter yet: no
classes/interfaces/inheritance at all, so EXTENDS/IMPLEMENTS are simply never
produced (not a gap, a correct absence — document as such rather than leaving it
looking unfinished). Structs/unions/enums, functions (no methods — no receiver
concept), global variables, `static`/`extern` linkage for a rough EXPORTS proxy
(no real module system — header `#include` is the closest thing to IMPORTS, but the
preprocessor is a separate pass tree-sitter doesn't run, so `#include` resolution is a
documented gap, not attempted; function pointers as a `TypeRef` case to check).

Tiers "17.8+" get their own planning pass before batch 3 starts — not detailed yet.

---

## Deferred: Phase 18 — 1.0.0 Beta Hardening & Release

Full breakdown preserved below; implementation is on hold until Phase 17's Tier 1 +
Tier 2 close (per user decision 2026-09-30 — solid cross-language benchmarking needs
real language breadth first). Resumes as the current phase once Phase 17 closes.

<details>
<summary>Phase 18 draft task breakdown (click to expand)</summary>

### Proposed sub-areas (each independently testable, same closing discipline as a
language sub-phase: planned → TDD → Happenings → closed with its own commits)

- [ ] 18.1 **Performance & Comparative Benchmarking.** Benchmark every Tier 1/2
  language once implemented, compared against Code Review Graph and Graphify on
  identical fixtures (both have deterministic, offline, no-LLM code paths). See full
  8-task breakdown below.
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

### 18.1 — Performance & Comparative Benchmarking (full detail)

1. **Fixture repos.** One real-world repo per supported language, pinned by commit,
   vendored under `benches/fixtures/`.
2. **Isolated comparison-tool environment.** Install `code-review-graph` (pip) and
   `graphify`/`graphifyy` (uv/pipx) into a throwaway venv under the scratchpad
   directory. **Needs explicit go-ahead before installing third-party PyPI packages**
   (previously granted for the isolated-venv approach; re-confirm if this has been a
   while).
3. **Metric harness.** Cold analyze/build wall time, incremental-update wall time,
   peak memory, determinism check, using each tool's own no-LLM command.
4. **Coverage/depth comparison — not a race.** Separate table: entity/edge types each
   tool extracts, inferred vs. explicit edges.
5. **Token-efficiency comparison (stretch).** Only if a fair comparison is
   constructible; skip rather than force a misleading number.
6. **Results doc.** `docs/benchmarks.md`: methodology, results table per language,
   coverage table.
7. **Repo/branding structuring — deferred.** Results stay in `docs/benchmarks.md`
   only; no README claims yet.
8. **Regression gate (stretch).** Whether CI gates on benchmark regressions — TBD.

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

</details>
