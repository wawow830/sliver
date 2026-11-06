# ADR-0006: One path per idea, docs that can't drift

Status: accepted · Amends: ADR-0001 (crate layout), ADR-0004 (config aliases)

## Context
Sliver grew by accretion. The same idea often had several spellings in the code (a slider was
either a `*/slider` module kind or `slider = true`, and code branched on both). Crates with a
single user carried their own manifests and boundaries without a reason to exist. Documentation
duplicated the man pages, and inventories, design specs and "not implemented" lists described a
product rather than the code. Every one of those had to be kept in sync by hand, and wasn't. They
also primed readers, human or agent, to build towards the document instead of from the code.

## Decision
- **One path per idea.** Config spellings that mean the same thing (waybar aliases, `X/slider`
  for `X` with `slider = true`) are normalised once, when the config is loaded. Nothing after
  the loader branches on a spelling. A presentation such as `slider` applies to every module
  that has the value it needs. Built-ins get no paths user modules lack.
- **A crate needs a boundary.** It's a library more than one crate uses, or it marks a hard
  boundary: privilege (`sliverd`, `sliver-hw`), a wire contract (`sliver-ipc`, `sliver-api`,
  `sliver-client`), or test isolation (`sliver-testbus`). Code only the agent uses lives in the
  agent. Importers of foreign configs (waybar, tiny-dfr) live together in `sliver-import`.
- **A doc exists only if it can't silently drift.** That means one of:
  1. a reference a test checks against the code: the man pages (module types, keys, gestures,
     stylesheet properties, CLI options) and the D-Bus introspection XML;
  2. a procedure a person must follow: the README, `docs/safe-swap.md`, `docs/packaging.md`,
     `docs/api.md` for app authors;
  3. a decision record (this directory), which is dated and isn't edited after acceptance except
     to change its status or to keep its references pointing at what exists (the gate checks
     that every `docs/` path named anywhere exists).

  How a piece of code works, and why, goes in its module's `//!` comment, where it's read and
  changed with the code. Feature inventories, roadmaps, design specs, status tables and lists of
  what isn't implemented aren't kept: the code and its tests are the inventory.

## Consequences
- The customization and styling guides fold into `sliver-config(5)` and
  `sliver-stylesheet(5)`, which tests keep complete.
- The agent, sliverd and integration guides move into module comments. The feature list, the UX
  notes and the T2 notes go. `GOAL.md` keeps the mission, the design
  principle and the invariants.
- `sliver-stats`, `sliver-reload` and `sliver-feeds` become modules of `sliver-agent`.
  `sliver-waybar` becomes `sliver-import` and takes the tiny-dfr importer from `sliver-config`.
