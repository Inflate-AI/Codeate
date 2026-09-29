# Codeate — Project Instructions

Codeate is being built in **Rust**. See `README.md` for the product vision (deterministic,
language-agnostic code analysis engine — no LLMs, no embeddings, no probabilistic inference
in the core).

These instructions define how work on this repository is planned, implemented, and closed out.
They override default behavior for this project.

---

## Git identity

This repo uses a local (repo-scoped) git identity, not the global one:

```
user.name  = Saran Mahadev
user.email = sarandevnet@gmail.com
```

Never change this to the global identity. Never commit with `--no-verify` or amend
published commits.

---

## Planning documents

Three files drive all work and must be kept current:

- **`ROADMAP.md`** — the **Phases** of implementation. Each phase is a coherent,
  independently shippable slice of Codeate (e.g. "Phase 1: Repository ingestion +
  single-language parser", "Phase 2: Symbol resolution", ...). Phases are ordered.
  A phase only gets added or reordered with user approval.
- **`TASKS.md`** — the task breakdown for the **current phase only** (past phases'
  task lists can be cleared or archived once closed). Each task is a concrete,
  independently testable unit of work with a clear "done" definition.
- **`CHANGELOG.md`** — a record of closed phases, in Keep a Changelog style,
  written when a phase is closed (see workflow below).

Do not put planning content in memory or in ad-hoc scratch files — it belongs in these
three files so it survives and stays visible to the user.

---

## Workflow (follow this exact sequence, per phase)

1. **Plan the phase.** Before writing any implementation code for a phase, write/update
   its entry in `ROADMAP.md` and break it into tasks in `TASKS.md`. Use Plan mode /
   present the task breakdown to the user.
2. **Get user approval.** Do not start implementing until the user explicitly approves
   the phase's task breakdown in `TASKS.md`.
3. **Check git is clean.** Run `git status` before starting implementation. If the
   working tree is not clean, stop and surface what's there — do not implement on top
   of unexplained or uncommitted state. Only proceed once it's clean (committed, stashed,
   or explicitly disregarded by the user).
4. **Implement using TDD.** For every task:
   - Write a failing test first (unit/integration as appropriate for Rust — `cargo test`).
   - Write the minimum code to make it pass.
   - Refactor with tests green.
   - No implementation code without a preceding failing test, except pure scaffolding
     (Cargo.toml wiring, module stubs) that has no behavior to test.
5. **Present "Happenings."** After implementing the phase's tasks, summarize for the
   user what was actually built/changed — concrete, concise, not a diff dump. Call this
   section "Happenings."
6. **Wait for review confirmation.** Do not close the phase until the user confirms the
   Happenings review.
7. **Close the phase with structured commits.** On confirmation:
   - Turn the reviewed Happenings into a structured `CHANGELOG.md` entry *before*
     committing: a `## [Phase N] — <name>` section (Keep a Changelog style — `Added`
     / `Changed`/ `Fixed` groups as applicable), one bullet per task, written from the
     Happenings content rather than re-derived from the diff.
   - Commit the work in logical, structured commits (not one giant commit) — one
     commit per task or per coherent unit, with clear messages. The `CHANGELOG.md`
     update and `ROADMAP.md`/`TASKS.md` bookkeeping land in their own closing commit.
   - Mark the phase complete in `ROADMAP.md` and clear/archive its section in `TASKS.md`.

Never skip a step in this sequence, and never start the next phase's implementation
until the current phase is closed.

---

## Rust conventions

- Standard `cargo` project layout; workspace crates if/when the architecture calls for
  splitting (e.g. `core`, `language adapters`, `cli`) — don't split prematurely.
- `cargo test` must pass and `cargo clippy` should be clean before a phase is closed.
- Prefer explicit, deterministic code over cleverness — this matches the project's own
  design principle (see README "Design Principles → Deterministic").
- No LLM/embedding dependencies in the core analysis path, per the project's non-goals.

---

## Toolchain location (Windows-specific — read before running any `cargo`/`rustc`/`rustup` command)

The Rust toolchain lives on **`D:\`**, not the default `C:\Users\<user>\.cargo` /
`.rustup` location:

```
CARGO_HOME  = D:\cargo
RUSTUP_HOME = D:\rustup
```

The Windows user-level `PATH` and `CARGO_HOME`/`RUSTUP_HOME` environment variables are
set correctly at the OS level, but **tool shells spawned in an agent session may inherit
a stale/parent-process environment that doesn't see them** — `cargo`/`rustc` can appear
completely missing even though the toolchain is installed and fine. If that happens:

1. **Do not reinstall the toolchain.** It is not actually missing — the shell just can't
   see it. Reinstalling to the default `C:\` location creates a confusing duplicate
   install and masks the real (environment) problem. (This happened once already — see
   `CHANGELOG.md`'s Phase 8 entry for the fallout, since deleted and moved back to `D:\`.)
2. Verify first: `ls D:\cargo\bin` / `ls D:\rustup` should show the installed binaries
   and toolchains.
3. Set the environment explicitly for the session/command instead:
   - PowerShell: `$env:CARGO_HOME = "D:\cargo"; $env:RUSTUP_HOME = "D:\rustup"; $env:PATH = "D:\cargo\bin;$env:PATH"`
   - Bash: `export CARGO_HOME=/d/cargo; export RUSTUP_HOME=/d/rustup; export PATH="/d/cargo/bin:$PATH"`
   - Without `RUSTUP_HOME`/`CARGO_HOME` set, `cargo`/`rustc` fall back to the default
     `~/.cargo`/`~/.rustup` location even if `D:\cargo\bin` is first on `PATH` (the
     binaries there are rustup proxies that need `RUSTUP_HOME` to find the actual
     toolchain) — setting `PATH` alone is not enough.
