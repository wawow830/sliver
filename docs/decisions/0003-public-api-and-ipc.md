# ADR-0003: Public D-Bus API and sliverd⇄agent IPC

Status: accepted (cedar, T5). The full spec is in `docs/api.md`. Bus name prefix is pending human answer H1.

## Ownership
**cedar owns both API surfaces** (D-Bus schema and IPC wire format) and the `sliver-ipc` crate that
defines them. Other peers propose changes to cedar or amend this ADR. Single ownership matters here
because these are the only interfaces other people's code will depend on.

## Decisions
1. **Push model on the session bus.** Clients call `CreateContext` on the agent and `SetItems`/`Update`
   declarative items. The agent renders them and sends `Event` signals **unicast** to the owner.
   Contexts die with the owner's connection.
   - Rejected: the MPRIS/SNI pull model, where apps export objects that the agent reads. Shell scripts
     can't export objects, and SNI showed that registration/re-discovery is fragile.
   - Rejected: apps sending pixels. That costs theme consistency, power and hit-testing.
2. **Zones belong to the user. Apps only fill the app zone (own, verified focus) or status zone.**
   Precedence: user per-app layout > app's own context > include-children > `target` contexts > default.
   App identity is verified from the connection pid (same pid, Flatpak instance, or systemd app cgroup),
   never from a claimed app id. Sandboxed clients are limited to their own app. Limits are per connection.
3. **Versioning:** major in interface names (`TouchBar1`), minor in a `Version` property. Every
   extensible structure is `a{sv}`. Unknown keys and types come back as warnings and are never errors.
4. **IPC = `SOCK_SEQPACKET` + JSON messages + sealed memfds for pixels.** The agent renders full logical
   frames. sliverd copies them, so a later agent write can't tear or SIGBUS sliverd.
   - JSON over postcard/bincode: schema evolution by field name, debuggable, tiny next to the pixel data.
     Rejected D-Bus on the system bus (needs bus policy, a dbus-daemon dependency in the critical path)
     and varlink (no standard fd passing).
5. **sliverd understands key regions and two layers (base/fn), and nothing else.** Esc/F-keys are
   hit-tested and emitted inside sliverd, and Fn swaps layers locally. So keys never wait on the agent
   and survive an agent hang. Everything else (sliders, gestures, widgets) is an `agent` region and gets
   raw touch events.
6. **sliverd enforces invariant 1 itself:** hangup or a missed on-demand ping (2 s; no idle timers) → safe layer; an "escape floor"
   (Esc + F1–F12 must exist as key regions in base ∪ fn, else the built-in fn layer is substituted);
   strict validation; `KEY_SYSRQ` refused; only the active seat0 session's uid can drive the bar.

## Consequences
- The agent needs a renderer (ADR-0002) able to produce `xrgb8888` frames and a "pressed" variant.
  sliverd needs one too, for the safe layer only.
- One client crate (`sliver-client`) should hide re-registration on agent restart.
- Plugins (open in ADR-0001) can be out-of-process `status`/`target` clients of this API. Built-in
  widgets stay in the agent.
- Revisit if per-frame copies show up in power measurements (option: buffer release protocol, as in wl_shm).
