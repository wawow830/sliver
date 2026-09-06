# Disposable Lua worker lifecycle

Status: accepted

## Decision

Lua runs in a separate process for every candidate and active configuration. The
supervisor owns the worker control connection, a pidfd, the worker's shared
three-slot frame mapping, and the worker systemd unit and cgroup. The worker has
no DRM, evdev, uinput, backlight, or broker file descriptor.

Production workers run as unique transient user services. The supervisor starts
them with `MemoryMax=512M`, `TasksMax=64`, `OOMPolicy=kill`,
`KillMode=control-group`, `NoNewPrivileges=yes`, `PrivateDevices=yes`,
`DevicePolicy=closed`, and journal stdout and stderr. The
worker drop-in under `systemd/sliver-lua-worker-.service.d/` is the only limit
configuration surface. The memory limit covers RAM; swap is a separate
administrator policy. `TasksMax` counts kernel tasks, including threads.

The worker reports progress only from its owner loop. The supervisor enforces a
two-second wall-clock deadline around startup and each worker request. It
allows 500 milliseconds for one graceful `stop` callback during replacement,
logout, or shutdown. A timeout, process exit, malformed packet, or callback
failure never receives another Lua callback. The supervisor kills the worker
cgroup, sends `SIGKILL` through the worker pidfd, kills the process group, and
reaps the systemd launcher within a separate bounded cleanup window before
entering fixed recovery. A failed saved source on supervisor startup is tried
once, then the embedded default is started once without changing the selected
path; if that also fails, recovery takes over. Live worker failures use fixed
recovery and do not start a replacement automatically.

Touch transitions and worker output are bounded. Repeated moves coalesce by
contact. Down, up, cancel, Fn, and modifier transitions fail the worker when
the fixed queue is full. Frames use a fixed shared mapping with acquire and
release state transitions, so the supervisor never reads a slot while the
worker is writing it.

Lua stdout and stderr belong to the user worker journal. Hardware broker output
belongs to the system service that owns the hardware.

## Rejected alternatives

A Rust thread cannot be the failure boundary because a blocking native module or
process call can prevent the supervisor from running its watchdog. A process
group alone is not enough because descendants can create a new session. The
systemd unit and its control-group kill path provide the descendant boundary.

The callback protocol does not use `WatchdogSec=`. systemd service watchdogs
measure service keep-alives, not individual Lua callbacks.
