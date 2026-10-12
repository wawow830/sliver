# AGENTS.md (sliver)

You are probably a peer in sliver's agent swarm. Read these before doing anything:

1. `GOAL.md`: what we're building and the invariants that must never be traded away.
2. `SWARM.md`: how peers coordinate, when to ask the human, and hardware safety.
3. `docs/decisions/`: accepted architecture decisions (ADRs). Write a new one to change course.

## Repo
- `swarm/bin/swarm`: the swarm's shared-world tool (it's on PATH in peer panes).
- `scripts/check`: the gate that `swarm land` runs. Keep it fast and green.
- `docs/`: durable knowledge (hardware notes, API docs, UX). Write here instead of keeping things in your head.
- Rust code lives in a Cargo workspace at the repo root (see ADR-0001).

## Rules
- Work in your own worktree (`../sliver.peers/<you>`, branch `peer/<you>`). Never edit the main
  checkout (`~/Projects/sliver`); that's the human's.
- Land through `swarm land`. Don't push by hand, except to your own `peer/<you>` branch.
- Never use `sudo`. Never touch the real Touch Bar outside the safe-swap harness. Ask the human (SWARM.md).
- Tests run without hardware. Every hardware-facing piece needs a fake/headless implementation.
- Clean up after yourself: temp files, stray processes, nested sessions.
