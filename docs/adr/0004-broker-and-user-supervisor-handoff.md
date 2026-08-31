# Hardware broker and user supervisor handoff

Status: accepted

The system `sliver-broker` service owns DRM, Touch Bar input, uinput, and
backlight before login. It starts one embedded Lua worker under the broker
account's restricted user manager when no local seat session is active.

Each logged-in user runs `sliver-supervisor` in that user's systemd user
manager. The supervisor owns that user's selected-path state and Lua worker. It
connects to the broker over one mode-0660 Unix socket. The broker verifies the
kernel peer credentials and the `sliver-supervisor.service` cgroup, authorizes
its UID against the active local seat session, and rechecks that ownership
before every hardware request. It does not inspect `/proc/<pid>/exe`: the
deliberately unprivileged broker cannot read that link across UIDs, and the
peer UID plus service cgroup are the production identity boundary. The public
CLI still talks only to the
user supervisor socket, where the CLI peer receives the full logind session
check from ADR 0001.

A user supervisor claims the broker before it stages a worker. The broker stops
the fallback but leaves its last frame on the panel. The user supervisor can
then stage and render without creating a blank interval. A complete presented
frame transfers ownership. A broker-side session revocation cancels known contacts, releases tracked
synthetic keys, and waits for a logout acknowledgement. The user supervisor
runs its graceful logout callback before sending that acknowledgement, and the
broker starts the fallback only after it receives it.

The broker accepts one user connection at a time. Its wire format carries only
normalized hardware events, complete logical frames, normalized backlight
levels, and generic key events. It carries no Lua source, client environment,
path selection, token, or TCP listener. The fallback and user workers use the
same Lua runtime and systemd resource and device policy; their process roles
are selected by the supervisor that stages them.
