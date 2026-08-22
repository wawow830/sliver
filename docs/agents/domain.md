# Domain docs

Sliver is a Rust Cargo workspace with three crates: `sliver-core`, `sliverd`, and `sliver-edit`. They share one domain context. Do not split the glossary or decisions by crate unless the repository adopts a context map later.

## Before changing code

1. Read the root `CONTEXT.md` when it exists.
2. Read the relevant records under `docs/adr/` when they exist.
3. Use the terminology from `CONTEXT.md` in issues, plans, tests, and code comments.

If those files are absent, continue without calling out their absence or creating them as setup work. The domain-modeling workflow creates them when the project needs to settle a term or record a decision.

## Decisions and conflicts

Record system-wide architecture decisions in `docs/adr/`. If a proposed change conflicts with an existing ADR, name the ADR and surface the conflict instead of silently overriding it.
