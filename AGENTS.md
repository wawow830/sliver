# AGENTS.md (sliver)

You are probably a peer in sliver's agent swarm. Read these before doing anything:

1. `GOAL.md`: what we're building and the invariants that must never be traded away.
2. `SWARM.md`: how peers coordinate, when to ask the human, and hardware safety.
3. `docs/decisions/`: accepted architecture decisions (ADRs). Write a new one to change course.

## Repo
- `swarm/bin/swarm`: the swarm's shared-world tool (it's on PATH in peer panes).
- `scripts/check`: the gate that `swarm land` runs. Keep it fast and green.
- `docs/`: durable knowledge (hardware notes, API docs, UX). Write here instead of keeping things in your head.
- Rust code lives in a Cargo workspace at the repo root (see ADR-0001). Crates are in `crates/`:
  - bins: `sliverd` (system daemon), `sliver-agent` (session agent), `sliverctl` (CLI),
    `sliver-settings` (GTK4 app)
  - libs: `sliver-config` (layout model + TOML schema), `sliver-ipc` (sliverd<->agent protocol),
    `sliver-render` (rendering + headless PNG)
  - Add a crate when a concern is shared by 2+ crates or needs isolation (e.g. heavy deps). Put
    shared deps in `[workspace.dependencies]` and use `x.workspace = true`; opt into `[lints] workspace = true`.
- CI (`.github/workflows/ci.yml`) just runs `scripts/check` on aarch64.

## Rules
- Work in your own worktree (`../sliver.peers/<you>`, branch `peer/<you>`). Never edit the main
  checkout (`~/Projects/sliver`); that's the human's.
- Land through `swarm land`. Don't push by hand, except to your own `peer/<you>` branch.
- Root only via `swarm sudo --why ".." -- cmd` (logged, one command per call). Never raw `sudo`,
  never read the password file. Never touch the real Touch Bar outside the safe-swap harness. When
  in doubt, ask the human (SWARM.md).
- Tests run without hardware. Every hardware-facing piece needs a fake/headless implementation.
- Clean up after yourself: temp files, stray processes, nested sessions.
