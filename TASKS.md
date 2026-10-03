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

Promoted to the current phase 2026-10-03 when Phase 17 closed. **Not yet approved for
implementation** — this breakdown is carried over from Phase 17's original deferral
note; it needs a fresh approval pass (per `CLAUDE.md` workflow step 2) before any TDD
work starts, since 18.1 in particular depends on which languages Phase 17 actually
shipped (12, not the originally-planned full Tier 1 + Tier 2 set).

### Proposed sub-areas (each independently testable, same closing discipline as a
language sub-phase: planned → TDD → Happenings → closed with its own commits)

- [ ] 18.1 **Performance & Comparative Benchmarking.** Benchmark each of the 12
  languages Phase 17 shipped, compared against Code Review Graph and Graphify on
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
