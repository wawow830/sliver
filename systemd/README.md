# systemd services and worker policy

The system broker owns the Touch Bar before login. Create a group for users
who may run a supervisor, add those users, and install `sliver-broker.service`
after the `sliver` account has the required DRM, input, and uinput device
permissions:

```sh
sudo groupadd --system sliver-supervisors
sudo usermod --append --groups sliver-supervisors "$USER"
```

Enable a lingering user manager for `sliver` so its restricted fallback worker
can start before login:

```sh
sudo loginctl enable-linger sliver
```

Enable the broker only as the administrator's explicit takeover step:

```sh
sudo systemctl enable --now sliver-broker.service
```

The broker conflicts with `tiny-dfr.service`; stopping it does not start
`tiny-dfr` again. Enable `systemd/user/sliver-supervisor.service` for each user
that should run a Lua supervisor. Log out and back in after changing group
membership. The supervisor keeps its apply socket in that
user's `$XDG_RUNTIME_DIR` and starts workers with that user's user manager.

Install the worker drop-in under `/etc/systemd/user/sliver-lua-worker-.service.d/`
and run `systemctl --user daemon-reload` after changing it. The dash-truncated
unit name applies the policy to generated `sliver-lua-worker-*.service` units.

Workers have no Lua, TOML, or CLI setting for memory or task limits. A host
administrator changes those limits here. The user who owns the user manager can
still change that manager's units; use a system-manager `user-UID.slice` limit
when the ceiling must include all units for an account.

`MemoryMax=512M` is the RAM limit. Swap is not counted toward that value.
`TasksMax=64` counts kernel tasks, including threads and descendants. Set an
explicit `MemorySwapMax=` in a site policy if swap must be included in the
budget.

User-worker stdout and stderr go to the user journal. Hardware broker output
belongs to the system service that owns the hardware.
