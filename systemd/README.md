# systemd services and worker policy

The Fedora package creates the `sliver` broker account and the
`sliver-supervisors` group. Add users who may run a supervisor to that group.
The package also installs the M2 udev rules that grant the broker its DRM,
input, uinput, and backlight access through dedicated groups. It does not
enable either service.
See [the Fedora package instructions](../packaging/fedora/INSTALL.md) for the
initial account setup and explicit takeover command.

Enable a lingering user manager for `sliver` so its restricted fallback worker
can start before login:

```sh
sudo loginctl enable-linger sliver
```

The broker conflicts with `tiny-dfr.service`; stopping it does not start
`tiny-dfr` again. The documented takeover command globally enables
`sliver-supervisor.service`; `ConditionGroup=sliver-supervisors` keeps it
inactive for users who were not granted access. Log out and back in after
changing group membership. The supervisor keeps its apply socket in that
user's `$XDG_RUNTIME_DIR` and starts workers with that user's user manager.

The worker drop-in belongs under `/usr/lib/systemd/user/sliver-lua-worker-.service.d/`
when installed from the package. The dash-truncated unit name applies the
policy to generated `sliver-lua-worker-*.service` units.

Workers have no Lua or CLI setting for memory or task limits. A host
administrator changes those limits here. The user who owns the user manager can
still change that manager's units; use a system-manager `user-UID.slice` limit
when the ceiling must include all units for an account.

`MemoryMax=512M` is the RAM limit. Swap is not counted toward that value.
`TasksMax=64` counts kernel tasks, including threads and descendants. Set an
explicit `MemorySwapMax=` in a site policy if swap must be included in the
budget.

User-worker stdout and stderr go to the user journal. Hardware broker output
belongs to the system service that owns the hardware.
