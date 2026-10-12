# You and the swarm

The swarm runs in a herdr tab called **swarm**. One pane runs the `pulse` (a dumb clock that wakes
idle peers that have work), and each other pane is a peer agent. When a peer needs you, you get a
herdr notification and a desktop notification.

Run these from `~/Projects/sliver` (or anywhere in the repo). `alias swarm=~/Projects/sliver/swarm/bin/swarm` helps.

```
swarm status                         # everything at a glance
swarm human                          # answer open questions one at a time (a / b / ok / any text)
swarm human list [--brief]           # read them all (--brief: one line each, * = recommended)
swarm human answer H3 b              # reply directly; the peer that asked gets woken
swarm human edit [H3 c]              # change an earlier answer; the peer is told it changed
swarm send ash "please prioritize per-app layouts"   # talk to one peer
swarm say "new priority: ..."        # talk to everyone
swarm task add "..." --body "..."    # add work
swarm pause                          # natural stop: peers finish their current turn, nobody gets woken,
                                     #   messages queue, panes and claims stay as they are
swarm resume                         # pick up exactly where they left off
swarm down                           # interrupt all peers and stop the pulse (state is kept)
swarm up --peers ash,birch           # bring it back (`swarm spawn NAME --resume` revives a peer's session)
```

Live visualizer: **http://localhost:7777** (`swarm viz`, started by `swarm up`). Add `?replay` or press REPLAY for a
time-lapse of the whole swarm history. Click an agent to pin its live thoughts. `swarm viz --host 0.0.0.0` shares it on your LAN.

You can also just walk into any peer's pane and type to it.

What you'll be asked for: `sudo` commands, hardware checks on the real Touch Bar, taste calls,
decisions, anything public outside this repo, changes to your live desktop, and secrets.
Peers keep working on other things while they wait for you.

State lives in `.git/swarm/` (board, channel, inboxes). Peer worktrees live in `~/Projects/sliver.peers/`.
