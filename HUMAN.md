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
swarm quota                          # Claude usage left per login, and the auto-pause settings
# usage: the pulse checks Claude usage every 5 min (both CLIProxyAPI logins, the 5-hour limit).
#   At 40% left it pauses the swarm the same natural way; at 45% it resumes by itself.
#   Your own `swarm pause` is never auto-resumed. `swarm resume` while usage is low holds off the
#   auto-pause until usage recovers. Tune: swarm quota --pause-below 40 --resume-at 45 [--windows five_hour,seven_day] [--off]
# disk: when free space drops below 15 GB the pulse deletes idle peers' target/ (build output) until ~25 GB is free
swarm sudo --log                     # every root command a peer ran, with its reason (also on the channel)
# sudo: peers run root commands through `swarm sudo`, using /tmp/pass.txt (keep it chmod 600).
#   Revoke anytime with `rm /tmp/pass.txt`; /tmp is tmpfs, so a reboot revokes it too.
#   Disks, bootloader, users/sudoers, reboot, killing your session and removing packages are refused.
swarm down                           # interrupt all peers and stop the pulse (state is kept)
swarm up --peers ash,birch           # bring it back (`swarm spawn NAME --resume` revives a peer's session)
```

Live visualizer: **http://localhost:7777** (`swarm viz`, started by `swarm up`). It shows who talks to whom, what each
agent is thinking and the latest messages. Hover an agent to see its conversations. Press "Replay the whole build" (or
add `?replay`) for a time-lapse. `swarm viz --host 0.0.0.0` shares it on your LAN.

You can also just walk into any peer's pane and type to it.

What you'll be asked for: hardware checks on the real Touch Bar, taste calls, decisions, anything
public outside this repo, changes to your live desktop, secrets, and root commands that `swarm sudo`
refuses (or all root work, if you've revoked the password).
Peers keep working on other things while they wait for you.

State lives in `.git/swarm/` (board, channel, inboxes). Peer worktrees live in `~/Projects/sliver.peers/`.
