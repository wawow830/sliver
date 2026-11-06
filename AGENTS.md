# AGENTS.md (sliver)

Read `GOAL.md` first: what sliver is and the invariants that are never traded away. Accepted
architecture decisions are in `docs/decisions/`. Write a new ADR to change one.

## Repo
- A Cargo workspace (ADR-0001, ADR-0006). Crates are in `crates/`:
  - bins: `sliverd` (system daemon), `sliver-agent` (session agent), `sliverctl` (CLI),
    `sliver-settings` (GTK4 app)
  - libs: `sliver-config` (layout model + TOML schema), `sliver-ipc` (sliverd<->agent protocol),
    `sliver-render` (rendering + headless PNG), `sliver-api` (public D-Bus API), `sliver-import`
    (waybar and tiny-dfr importers), and smaller helpers. A crate needs a boundary (ADR-0006):
    code only the agent uses lives in the agent, so stats, reload and feeds are agent modules.
    Shared deps go in `[workspace.dependencies]`; crates opt into `[lints] workspace = true`.
- `scripts/check` is the gate: fmt, clippy (`-D warnings`), tests, packaging lints. CI
  (`.github/workflows/ci.yml`) runs it on aarch64. Keep it fast and green; run it before pushing.
- Docs (ADR-0006): a doc exists only if a test checks it against the code (`docs/dbus/` whole;
  the man pages in `man/` name everything the code accepts), it's a procedure (README, `docs/user-guide.md`, `docs/safe-swap.md`,
  `docs/packaging.md`, `docs/api.md`), or it's an ADR (`docs/decisions/`). How and why code works
  goes in its module's `//!` comment. No feature inventories, roadmaps or status tables.
  The one exception is `docs/hardware.md`: measured hardware facts that several crates cite.
- `harness/`: the safe-swap harness and guided hardware checks. `packaging/`: RPM, systemd, udev,
  D-Bus files.

## Hardware safety
The F-keys exist only on the Touch Bar, so a broken bar strands the user.
- Default to the headless path: render to PNG and simulate input. Look at the PNGs to check visuals.
- Never stop `tiny-dfr` or the installed `sliverd`, take DRM master on the bar's card, or open
  its input devices (`evtest`, `libinput debug-events`, `EVIOCGRAB`) by hand. Real-hardware runs
  go only through the safe-swap harness (`docs/safe-swap.md`), which always gives the bar back.
- Tests run without hardware. Every hardware-facing piece has a fake/headless implementation.
- GUI apps are tested in a nested or headless compositor, never in the user's live session.
- Root, changes to the live desktop, and anything public (releases, COPR, upstream) need the
  human's OK first.

## Writing style (code comments and docs)
- Comments say *why* the code is the way it is. Who did it, when, and in which ticket is git's
  job: no task/ask IDs (`T99`, `H4`), names, commit hashes or "seen on ..." stories.
- A hardware fact that justifies code is stated as a fact ("the Mac14,7 bar reports no pressure"),
  not as an anecdote.
- Docs are reference for someone who has never seen the project's history: current behaviour,
  how to use it, why it's built that way. Verification history belongs in commit messages, not
  in feature tables.
- Commit messages: a short imperative subject (≤72 chars), details in the body.
