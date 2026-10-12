# sliver-agent

The per-user session agent (ADR-0001). It resolves the user's config for the current context,
runs modules, renders frames with sliver-render, and presents them to sliverd over the private
IPC (api.md §6). Owner of the core loop: ash (T24).

## Shape
```
config (sliver-config) ─resolve(layer, ctx)─► zones of ModuleInstances
   └─ modules.rs: module runtime ─► Cells (content, classes, width, touch kind, bindings, next change)
        └─ frame.rs: Scene ─► sliver-render ─► image + all-pressed image + regions
             └─ link.rs: ShmBuffers ─present─► sliverd   (◄─ touch / fn / display / hatch / ping)
agent.rs: the sans-IO core that ties it together. main.rs: config/stylesheet loading + poll loop.
```
- **Two IPC layers.** sliverd's `base` slot shows the top custom layer (or `base`), and its
  `fn` slot shows config layer `fn`. sliverd swaps them on Fn by itself.
- **Key modules are key regions.** A module whose only binding is "tap → key, fire on press"
  becomes a sliverd `key` region, so Esc and the F-keys never wait on the agent and keep working
  if it hangs. Everything else interactive becomes an `agent` region for gesture recognition.
- **Regions** are full height and reach up to 12 px into neighbouring gaps (split in the middle),
  so near-misses count but empty bar space doesn't.
- **Power (invariant 3).** Nothing renders unless something changed. The only timer is the
  earliest `next_change` of a module (clock ticks), and there are none while the display is off.
  The link drops frames whose pixels and regions didn't change, coalesces frames while sliverd
  hasn't confirmed the last one, and sends damage spans.
- **Stylesheet:** the built-in `sliver` theme, then `style.css` next to the config (or
  `stylesheet = "…"`) on top.
- **Config errors** fall back to the built-in defaults (last-good fallback comes with hot
  reload, T26).

## Running
```
sliver-agent --render out              # headless: out-base.png, out-fn.png (system fonts/icons)
sliver-agent [--config FILE] [--socket PATH]
```
Tests: `crates/sliver-agent/tests/agent.rs` runs the real link against `sliverd::testing::TestDaemon`
(real socket, memfds, validation, touch routing) and snapshots frames (`SLIVER_BLESS=1`).

## Gestures and actions
`gesture.rs` recognizes tap, double tap, hold (500 ms), release, swipes (40 px, or
`swipe-threshold`) and drag on agent regions, from sliverd's touch timestamps (CLOCK_MONOTONIC µs).
Only gestures a module binds are recognized, so taps fire at once unless `on-double-tap` is bound,
and there's no hold timer without `on-hold`. Pressed feedback for agent regions is drawn by the
agent (sliverd only draws key regions'). Region ids of the `fn` frame start at 10000.

Actions (`agent.rs` → `Effect`s run by main.rs):
- `key`/keys: sent to sliverd as `emit_keys`.
- shell/exec: `{value}` substituted; started in their own process group and reaped.
- `open`: `gtk-launch`. `url`: `xdg-open`. D-Bus without args: `busctl call`.
- `layer`: toggle/oneshot/hold. Hold layers close when that finger lifts, oneshot layers after the
  next action taken in them. `close-layer`.
- `toggle-alt`, and macros (steps with `sleep` are scheduled).
- Drawer handles open and close their group, and `collapse-after` closes it again.
- `on-drag` runs at most every 50 ms, always with the final value. Sliders report 0–100 along
  their trough, other modules the bar x.

## Context and data
The live world comes from fir's `sliver-feeds` (desktop backend, session sources, and the user
module runtime for `custom/*` with `exec`) through one wake fd the main loop polls. The rule
context is `Aggregate::context()` plus the agent's own keys (`context.rs`: time, weekday,
physical_esc, layer). logind's `LockedHint` is only believed on Hyprland/sway/niri while a locker
process runs, because it's verified stale on this machine, and believed as-is on GNOME/KDE.

User modules render through `sliver_modules::view`. An action on one re-runs it (waybar
`exec-on-event`). `{ signal = N }` and `{ update = "custom/x" }` reach the runtime, and its timers
stop while the display is off.

Data modules: `battery` (tiny-dfr icons, states with lower-is-worse), `backlight`, `kbd-backlight`,
`audio` (`node = "source"` for the mic), their `/slider` variants (`collapsed = true` shows just
the icon until tapped, then closes after 5 s), `mpris` (values escaped as markup), `idle-inhibitor`,
`power-profiles`, `language`, `window`, `mode`. Modules with no data are hidden. Format output is
markup (waybar).

Built-in behaviour goes through `Action::ModuleAction` exactly like user `actions = { … }`:
- slider drag → `set`;
- audio tap → `toggle-mute`;
- mpris tap → `play-pause`, swipe left → `next`, swipe right → `previous`;
- idle-inhibitor tap → `toggle`;
- `expand` on any module.

User `on-*` bindings win. Session changes run on an ops thread (logind `SetBrightness`, wpctl,
MPRIS, the caffeine inhibitor), so the main loop never blocks on them.

`sliver-agent --render out` uses the live sources too, and prints every cell with its rect,
module key, CSS id/classes and content, which is handy for debugging configs and stylesheets.

## Status
Works: everything above, plus `--render` and the real sliverd loop.

Not yet: the public D-Bus API (`sliver_api`: App controls/Status items placeholders, published
items), `workspaces`/`taskbar`/`notifications`/`cpu`… modules, the `layout` action, hot reload
(T26).
