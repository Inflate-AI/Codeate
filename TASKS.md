# Tasks

Task breakdown for the **current phase only**. Cleared/archived when the phase closes
(its summary moves to `CHANGELOG.md`).

## Current phase: Phase 18 — 1.0.0 Beta Hardening & Release

See `ROADMAP.md` Phase 18. Moved up ahead of Phase 17's remaining language adapters by
explicit user request on 2026-09-30 — Phase 17 is paused (not closed) after 17.1 (Go)
and resumes once this phase closes. Draft task breakdown below, **pending approval**
before implementation starts (per `CLAUDE.md` workflow step 2).

### Proposed sub-areas (each independently testable, same closing discipline as a
language sub-phase: planned → TDD → Happenings → closed with its own commits)

- [ ] 18.1 **Performance benchmarking.** Pick/vendor 1-2 real-world repos per
  supported language (Rust, Python, Go) at meaningful scale. Benchmark harness
  (`criterion` or hand-rolled timing in the CLI) covering ingest, full analyze,
  incremental analyze, and representative queries. Record baseline numbers
  committed to the repo so future phases can catch regressions.
- [ ] 18.2 **Correctness/robustness hardening.** Malformed/edge-case input handling
  across all 3 adapters (truncated files, syntax errors, encoding issues, huge
  single files, deeply nested structures). Property-based or fuzz testing
  (`proptest`/`cargo-fuzz`) where it fits the determinism guarantee — same fuzzed
  input must always produce the same output, crash-free.
- [ ] 18.3 **Documentation.** Architecture doc (how ingestion → detection → adapters
  → resolution → graph → store → query fit together), an adapter-authoring guide
  (concrete enough that Phase 17's resumed language work can follow it without
  reverse-engineering existing adapters), query API reference (Phase 11/16
  surface), CLI usage docs.
- [ ] 18.4 **Packaging/distribution.** crates.io publish readiness (crate
  metadata: description, license, repository links, keywords for
  `codeate-core`/`codeate-cli`/`codeate-api`), binary release process (target
  platforms, build/release workflow).
- [ ] 18.5 **Dependency/security audit.** `cargo audit` (or equivalent) across the
  workspace; resolve or explicitly accept-and-document any findings.
- [ ] 18.6 **CI.** Automate `cargo test` + `cargo clippy -- -D warnings` on every
  push/PR, covering all 3 language adapters; extends Phase 0's local check script
  rather than replacing it.
- [ ] 18.7 **Versioning policy.** Formal SemVer policy for the canonical model,
  storage format, and `codeate-api`'s public surface (extends Phase 16's policy);
  explicit statement of what "1.0.0 beta" commits to vs. what can still change
  before a stable 1.0.0.

### 18.1 — Performance & Comparative Benchmarking

Expanded scope per user request: benchmark every language Codeate currently supports
(Rust, Python, Go today; re-run per new language as Phase 17 resumes), and compare
against **Code Review Graph** and **Graphify** on identical fixtures. Both tools have
a deterministic, offline, no-LLM/no-API-key code path (`code-review-graph build`;
`graphify extract --code-only`), so a fair head-to-head is possible. Results feed
`docs/benchmarks.md`, which is the source of truth for any README/branding comparison
claims — not marketing copy written first and backfilled with numbers.

1. **Fixture repos.** One real-world repo per supported language (Rust, Python, Go),
   pinned by commit, vendored under `benches/fixtures/` (submodule or pinned
   tarball — not copied source). Same repos used for internal-only Codeate
   benchmarking and the 3-way comparison, so numbers are directly comparable.
2. **Isolated comparison-tool environment.** Install `code-review-graph` (pip) and
   `graphify`/`graphifyy` (uv/pipx) into a throwaway venv under the scratchpad
   directory, never the main dev environment — pin the exact versions tested and
   record them in the results doc (both are actively developed; a re-run months
   later needs to note if versions moved). **Needs your explicit go-ahead before I
   install third-party packages from PyPI**, even into an isolated venv.
3. **Metric harness.** For each tool × each fixture, capture: cold analyze/build
   wall time, incremental-update wall time (single meaningful file change), peak
   memory, and a determinism check (run twice, diff output — expected to hold for
   all 3 given their claims). Use their own code-only/no-LLM commands
   (`codeate analyze`, `code-review-graph build`, `graphify extract --code-only`)
   so the comparison is apples-to-apples on what each tool does without an LLM in
   the loop.
4. **Coverage/depth comparison — explicitly not a race.** A separate table, not
   blended into the speed numbers: entity/edge types each tool extracts (Codeate's
   resolved symbols/types/calls/structural edges vs. their AST-level extraction),
   and which edges are inferred vs. explicit. This is where Codeate's actual
   differentiation lives, and conflating it with raw speed would be misleading.
5. **Token-efficiency comparison (stretch).** Code Review Graph's headline claim is
   agent-context token reduction. Replicate the shape of that comparison using
   Codeate's Phase 11 query engine (deterministic facts out, per Phase 16's
   documented LLM-consumption contract) if it's a fair comparison to construct —
   confirm feasibility before committing to this task; skip rather than force a
   misleading number.
6. **Results doc.** `docs/benchmarks.md`: methodology (fixtures, pinned versions,
   hardware, exact commands run), a results table per language, and the coverage
   table from task 4. Regenerated (not hand-edited) whenever re-run.
7. **Repo/branding structuring — deferred.** Per explicit user decision, results
   stay in `docs/benchmarks.md` only for this phase; no comparative claims go into
   the public README yet. Revisit once results exist and are reviewed.
8. **Regression gate (stretch, confirm before building).** Whether 18.6's CI also
   gates on benchmark regressions, or the baseline stays a manual reference — your
   call, since benchmark stability on shared CI runners is often noisy.

### 18.2 — Correctness/robustness hardening

1. **Malformed-input fixture set.** Per adapter (Rust/Python/Go): truncated file,
   syntax error mid-file, invalid UTF-8/encoding, empty file, deeply nested
   structure, single-line huge file. Assert no panics, explicit error/partial
   result, not silent data loss.
2. **Property-based tests.** Add `proptest` as a dev-dependency; generate randomized
   valid-ish source mutations (token deletion/duplication) per language, assert the
   determinism guarantee (same input twice → identical output) and crash-freedom.
3. **Fix whatever the above finds** before moving to 18.3 — this task's "done" is
   defined by the tests it adds, not a fixed bug count.

### 18.3 — Documentation

1. `docs/architecture.md` — ingestion → detection → adapters → resolution → graph →
   store → query, referencing the actual crate/module boundaries.
2. `docs/adapter-authoring-guide.md` — concrete enough that Phase 17's resumed
   language work (17.2 TypeScript onward) can follow it directly; written by
   generalizing what the Rust/Python/Go adapters actually did, not a fresh design.
3. `docs/query-api-reference.md` — every Phase 11/16 query kind, params, examples.
4. CLI usage docs — expand `codeate-cli`'s own `--help` output and/or a
   `docs/cli.md`.

### 18.4 — Packaging/distribution

1. Fill in crate metadata (`description`, `repository`, `keywords`, `categories`,
   `readme`) for `codeate-core`, `codeate-cli`, `codeate-api` in their `Cargo.toml`s.
2. `cargo publish --dry-run` for all 3 crates, fix whatever it flags.
3. Binary release process: document (and script, if it's not much more work) a
   `cargo build --release` + platform-target matrix for at least Windows/Linux/macOS.

### 18.5 — Dependency/security audit

1. Install/run `cargo audit` against the workspace `Cargo.lock`.
2. Triage findings: upgrade, or explicitly document an accepted risk with reasoning
   (no silent ignores).

### 18.6 — CI

1. `.github/workflows/ci.yml` (or equivalent) running `cargo fmt --check`,
   `cargo clippy -- -D warnings`, `cargo test --workspace` on push/PR — mirrors
   `scripts/check.ps1` rather than diverging from it.
2. Confirm it runs cleanly on a fresh checkout (no reliance on local `D:\` toolchain
   quirks — CI runners use standard `~/.cargo`).

### 18.7 — Versioning policy

1. `docs/versioning-policy.md` (or fold into an existing doc): SemVer commitments
   for the canonical model (Phase 3), storage format (Phase 10), and `codeate-api`'s
   public surface (extends Phase 16's policy).
2. Explicit "1.0.0 beta" scope statement: what's stable now vs. what can still
   change before a real 1.0.0 (e.g. "3-language support is beta-stable; adding a
   4th+ language per Phase 17 won't be a breaking change").

---

This is the full draft — flag anything to reorder, merge, drop, or rescope. Once
confirmed, work starts at 18.1 following `CLAUDE.md`'s TDD workflow, same as 17.1.

---

## Deferred: Phase 17 — Additional Language Adapters (paused after 17.1)

Full required set and rationale in `ROADMAP.md` Phase 17. Resumes after Phase 18 closes.
Tier 4 (template/config formats) has been dropped from scope entirely per user decision
on 2026-09-30.

**Tier 1 — mainstream general-purpose** (12 remaining)
- [x] 17.1 Go
- [ ] 17.2 TypeScript
- [ ] 17.3 JavaScript
- [ ] 17.4 Java
- [ ] 17.5 C#
- [ ] 17.6 Kotlin
- [ ] 17.7 C
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

**Tier 3 — enterprise / legacy / niche**
- [ ] 17.21 VB.NET
- [ ] 17.22 Objective-C
- [ ] 17.23 Elixir
- [ ] 17.24 Solidity
- [ ] 17.25 Zig
- [ ] 17.26 Verilog
- [ ] 17.27 SystemVerilog
- [ ] 17.28 Nix
- [ ] 17.29 GDScript
- [ ] 17.30 ReScript
- [ ] 17.31 Luau
