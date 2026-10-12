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
swarm send ash "please prioritize per-app layouts"   # talk to one peer
swarm say "new priority: ..."        # talk to everyone
swarm task add "..." --body "..."    # add work
swarm pause / swarm resume           # stop/start waking peers (they finish their current turn)
swarm down                           # interrupt all peers and stop the pulse (state is kept)
swarm up --peers ash,birch           # bring it back (`swarm spawn NAME --resume` revives a peer's session)
```

You can also just walk into any peer's pane and type to it.

What you'll be asked for: `sudo` commands, hardware checks on the real Touch Bar, taste calls,
decisions, anything public outside this repo, changes to your live desktop, and secrets.
Peers keep working on other things while they wait for you.

State lives in `.git/swarm/` (board, channel, inboxes). Peer worktrees live in `~/Projects/sliver.peers/`.
