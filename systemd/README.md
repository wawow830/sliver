# systemd services and worker policy

Enable `sliver-supervisor.service` only after the `sliver` account has the
required DRM, input, and uinput device permissions. The service starts the
embedded default before login and performs session handoff through logind. The unit owns the hardware
before login and conflicts with `tiny-dfr.service`; stopping it does not start
`tiny-dfr` again. Enabling the unit is the administrator's explicit takeover
step.

Install the worker drop-in under `/etc/systemd/user/sliver-lua-worker-.service.d/`
and run `systemctl --user daemon-reload` after changing it. The dash-truncated unit
name applies the policy to generated `sliver-lua-worker-*.service` units.

The worker has no Lua, TOML, or CLI setting for memory or task limits. A host
administrator changes those limits here. The user who owns the user manager can
still change that manager's units; use a system-manager `user-UID.slice` limit
when the ceiling must include all units for an account.

`MemoryMax=512M` is the RAM limit. Swap is not counted toward that value. `TasksMax=64`
counts kernel tasks, including threads and descendants. Set an explicit
`MemorySwapMax=` in a site policy if swap must be included in the budget.

The worker's stdout and stderr go to the user journal. Hardware broker output
belongs to the system service that owns the hardware.
