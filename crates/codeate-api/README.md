# codeate-api

The stable, versioned entry point external consumers (IDE tools, code agents, other
developer systems) use to query a repository through Codeate, without depending on
`codeate-core`'s internals directly.

## Versioning

This crate's own version is the SemVer contract: [`Codebase`] and everything reachable
from it are covered. A breaking change to any of it is a major-version bump of
`codeate-api`. `codeate-core`'s internals — its module layout, anything not
re-exported here, the exact shape of `Graph`/`QueryEngine` — are **not** covered and
may change freely between `codeate-api` releases.

## The contract, for an LLM/agent consumer

Every query on [`Codebase`] returns **deterministic facts read from an already-built
graph** — the same repository state always produces the same answer. Nothing here
performs inference, ranking, or probabilistic reasoning; Codeate's core design
principle is that it stays deterministic so an agent can treat a result as ground
truth about the repository's *structure*, not a suggestion to weigh or a summary to
second-guess.

Concretely:

- A query result is either a fact ("`helper` is called by `run`") or empty
  ("no callers found"). It is never "probably" or "likely."
- Every reference/call/import site resolves to *something* — a real symbol, or an
  explicit placeholder (e.g. `external-crate:...`, `unknown-local:...`) — never
  silently dropped. An agent that sees a name it doesn't recognize among results
  should treat it as "this genuinely couldn't be resolved," not as missing data.
- Results reflect the repository as of the last [`Codebase::open`]/[`Codebase::refresh`]
  call, not necessarily the current on-disk state. Call `refresh()` after files
  change on disk before trusting new results.
- An agent should use these facts as *input* to its own reasoning (planning a
  refactor, assessing blast radius, navigating to a definition) — Codeate itself
  never suggests what to do with them.

## Example

```rust,no_run
let mut codebase = codeate_api::Codebase::open(".")?;

for symbol in codebase.symbol("UserRepository") {
    println!("{}  {}", symbol.id.as_str(), symbol.name);
    for caller in codebase.callers(&symbol.id) {
        println!("  called by {}", caller.name);
    }
}

// After the repository's files change on disk:
codebase.refresh()?;
# Ok::<(), codeate_api::Error>(())
```

See `src/lib.rs`'s module documentation for the full method list (symbol/definition
lookup, call graph, dependency analysis, implementation lookup, reference lookup,
impact analysis, repository exploration, and Phase 14's repository-level queries).
