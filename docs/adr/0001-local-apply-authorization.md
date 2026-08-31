# Local apply authorization

Status: accepted

`sliver FILE` and the no-argument embedded-default reset are authorized only on the per-user Unix supervisor socket. The supervisor reads kernel peer credentials, and a private logind adapter requires a non-root peer in the active local non-remote session on its seat. The adapter supplies a monotonic session generation from logind monitor events, so a grant is invalid if the session changes even when it later returns to the same snapshot. The supervisor checks the grant before staging and again at the last point before the first Sliver-owned commit mutation, either the selected-path state rename or its removal. That check is a logical transaction point, not an atomic lock across logind, the filesystem, or hardware. Lua workers inherit the supervisor's systemd user-manager environment because the client protocol carries only a tagged path-or-default selection and no client environment; there is no TCP or token path.

The real adapter uses stable `sd-login` passive APIs and the `seat` category of its login monitor. Unrelated session and user-manager churn must not advance the authorization generation. Tests use a private fake adapter with explicit active-seat generation changes and fake broker hardware.
