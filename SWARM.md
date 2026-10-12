# The sliver swarm

Sliver is built by a **decentralized mesh of persistent peer agents**. There is no root agent, no
planner and no boss. Peers share one world: this git repo, the `swarm` board/channel/inboxes,
and each other. Roles aren't assigned. They emerge from whatever context you build up.
Background: https://hallerite.com/posts/on-the-nature-of-the-swarm/

## Why it's shaped this way (read once, then act on it)
- **You are persistent.** You won't be torn down after a task. Your context is an asset. Peers
  will come back and *ask you* rather than re-reading everything you read.
- **Specialize.** Once you've built context in an area, keep taking work there and declare it with
  `swarm knows "..."`. Before starting in an unfamiliar area, check `swarm peers` and ask whoever
  knows it. A short question is far cheaper than rebuilding their context.
- **Communication is the price.** Keep messages short, dense and actionable. Don't narrate on the
  channel. Post decisions, claims, landings, questions and warnings.
- **Shared state is the source of truth.** Use the board for work, the repo for durable knowledge
  (`docs/`, ADRs in `docs/decisions/`) and the channel for events. Anything only in your head
  is lost at compaction, so write it down.
- **Hierarchy only where it pays.** If an area needs one owner (an API, a schema), propose it on
  the channel and record it in an ADR. **The structure is yours to change.** Improve `SWARM.md` and
  `swarm/bin/swarm` when the process is getting in the way. Announce the change and land it like code.

## The loop
Run this every time you're woken (by the pulse, a message, or the end of your own work):

1. **Orient:** `swarm inbox` → `swarm log -n 30` → `swarm task list`.
2. **Unblock others first.** Answer peer questions and review requests before your own work.
3. **Pick work:**
   - Continue your claimed task, or
   - `swarm task claim T#` the open task that best fits *your* context, or
   - find the most valuable gap between `GOAL.md` and reality, `swarm task add` it (split
     big things into tasks a peer can finish and land), and claim one.
4. **Do it** in your own worktree on branch `peer/<you>`. Make small, coherent commits with a
   `Swarm-Peer: <you>` trailer. Run `scripts/check`.
5. **Land often:** `swarm land` rebases on `origin/main`, runs `scripts/check`, and pushes `main`.
   Never force-push `main`. If a rebase conflicts with a peer's area, message them.
   **Never `git reset --soft origin/main` (or `reset` onto any remote ref) to squash.** All worktrees
   share `origin/main`, so it may already be newer than your base, and the commit would silently
   revert peers' work. Squash with `git rebase -i $(git merge-base HEAD origin/main)`, and check
   `git show --stat` before landing: it should only list your files.
6. **Record:** `swarm task note`, then `swarm task done T# "what/where"`. Durable knowledge goes into `docs/`.
7. **Stop:** end your turn. The pulse wakes you when there's something for you. If you see
   nothing useful to do, run `swarm rest "why"` first.

**Paused?** If `swarm` prints ⏸, the swarm is paused, either by the human or automatically to save
Claude usage (it resumes by itself when usage resets). Either way, do the same thing. Finish the step you're on (commit,
and land it if it's green), then end your turn. Don't claim, spawn or start anything new. Your claims stay
yours, messages queue, and the pulse wakes you on resume.

Disagreeing is fine. Say so to the peer directly (`swarm send`), and if it stays unresolved, write
both options in an ADR and ask the human (`--kind decision`).

## Commands
```
swarm status                         overview
swarm peers | knows "TEXT"           who exists / declare your expertise
swarm task add "TITLE" --body .. --area .. --after T3,T4 [--claim]
swarm task list [--all|--mine] | show T# | claim T# | note T# ".." | done T# ".." | release T# | drop T# ".."
swarm say "TEXT"                     broadcast to everyone (sparingly)
swarm send PEER "TEXT" [--now]       direct message (--now interrupts them mid-turn: urgent only)
swarm inbox | log -n N               read
swarm human ask "ONE SHORT QUESTION?" --kind K --detail ".." --option ".." --option ".." --pick a [--task T#] [--block]
swarm rest "why" | wake
swarm spawn NAME --why ".." [--model ..] [--knows ..] [--brief ".."]
swarm retire [NAME]
swarm land
```

## Growing the swarm
- **Spawn** a new peer when there's a parallel area that nobody has context in, and it's real work
  rather than a one-off lookup. Give it a `--brief`. **Cap: 6 live peers.** Beyond that, ask the
  human (`--kind decision`). Default model is Opus. Use `--model cliproxy/claude-sonnet-5-5` for a
  well-scoped area.
- **Sub-agents** (a tree inside the mesh) are for disposable work: lookups, summaries, mechanical
  edits. For example: `pi -p --no-session --model cliproxy/claude-haiku-5-5 "..."`. They don't join the swarm.
- **Retire** yourself if your area is finished and nobody needs your context. Release your tasks first.

## Ask the human. Never do these yourself
Use `swarm human ask`. The human reads these on a laptop screen between other things, so make them
easy to answer:
- **One question per ask**, as a short plain sentence (≤200 chars, enforced). Several decisions
  means several asks.
- Context goes in `--detail`: what it's for, the exact command to run (for `sudo`), and what happens next.
- Choices go in `--option` (repeat it), with your recommendation in `--pick`. The human can then reply
  with just `a`, `b` or `ok`. Use plain words, not jargon or abbreviations.

```
swarm human ask "Can you install librsvg2-devel?" --kind sudo --task T4 \
  --detail "Needed to render SVG icons. Run: sudo dnf install -y librsvg2-devel" \
  --option "installed" --option "no, use the pure-Rust renderer instead" --pick a
```
Keep working on something else unless you truly can't (`--block`).

| kind | when |
|---|---|
| `sudo` | anything needing root: `dnf install`, writing `/etc` `/usr`, system systemd units, udev, polkit, SELinux |
| `hardware` | anything that needs eyes or fingers on the real Touch Bar, or first-time use of a hardware path |
| `taste` | look and feel, UX or naming choices that GOAL.md doesn't settle (offer options + your pick) |
| `decision` | product scope, superseding an accepted ADR that peers disagree on, peer cap |
| `external` | anything public outside this repo: releases, COPR, upstream issues/PRs, new repos, repo visibility |
| `system` | changes to the user's live desktop: Hyprland config, keybinds, killing session processes; low disk |
| `secret` | credentials, tokens, accounts, money |

Also never: rewrite `main`'s history, delete other peers' branches or worktrees, or run `herdr server stop`.

## Hardware safety (this laptop's Escape key IS the Touch Bar)
- Default to the **headless path**: render to PNG and simulate input. Look at the PNGs yourself
  (read the image). That's how you verify visuals.
- Never stop `tiny-dfr`, take DRM master on the Touch Bar card, or grab its input devices directly.
- Real-hardware runs go **only** through the safe-swap harness once the human has installed it.
  The harness stops tiny-dfr, runs our daemon for a bounded time, and **always** restores tiny-dfr.
  Building that harness is an early task, and installing it is a `sudo` request.
- GUI apps: test in a separate headless or nested compositor session, never on the user's live
  session (see the user's global AGENTS.md for the Hyprland tools).
