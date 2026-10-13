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
- **Touch feedback** on agent regions: a press re-renders only that frame's image (`Frame::repress`, ~0.9 ms release vs ~2 ms for a full redraw; `cargo run --release -p sliver-agent --example bench`). Run release builds on hardware: debug renders are ~10x slower.
- **Power (invariant 3).** Nothing renders unless something changed. The only timer is the
  earliest `next_change` of a module (clock ticks), and there are none while the display is off.
  The link drops frames whose pixels and regions didn't change, coalesces frames while sliverd
  hasn't confirmed the last one, and sends damage spans.
- **Stylesheet:** the built-in `sliver` theme, then `style.css` next to the config (or
  `stylesheet = "…"`) on top.
- **Config errors** at startup fall back to the built-in defaults. After that, a broken save
  keeps the last good config (see Hot reload).

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
- shell/exec: `{value}` substituted; started in their own process group and reaped. Under
  systemd (`INVOCATION_ID` set) they run in their own transient scope
  (`systemd-run --user --scope`, unit `app-sliver-<prog>-…`), so restarting
  `sliver-agent.service` doesn't kill apps opened from the bar. Plain spawn otherwise.
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
the icon until touched: it opens on press, and sliding without lifting moves the value relative to
where it was; it closes 5 s after the last touch), `mpris` (values escaped as markup), `idle-inhibitor`,
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

## Lock-screen privacy (T87)

While the context says `locked` (see `context::plausibly_locked`), nothing private reaches the
bar, whatever the layout says (ux.md §5). `privacy.rs` does this in two layers:

1. Modules are built from `privacy::sanitize(world)`. That copy has no media title, artist,
   album or art, no latest-notification text, no window titles or window list, no script
   output, and nothing published by apps (so no App controls, Status items or modal context
   either).
2. `privacy::allowed` drops whole kinds: `custom/*` (arbitrary output, and commands run from a
   locked bar), `image` (a picture a script or the config picked: album art, a photo),
   published items and their slots, `window`, `taskbar`, and `emoji` (recents are a history).

Keys, sliders, toggles, the clock, battery, workspaces, the keyboard layout, the notification
count and media play/pause (status glyph only) stay. Both layers apply to the base and Fn
frames. Tested in `tests/agent.rs` (`locked_screen_shows_nothing_private`).

## Untrusted input (T98)

Script output, app-published items and user stylesheets are untrusted. None of them may take
the agent down, because systemd would restart it into the same input over and over.

- **Raster icons** (`icon-data`, files, album art) larger than 4096 px on either side are
  refused before decoding (`sliver_render::icon::MAX_RASTER_SIDE`). A few bytes of PNG can
  claim 30000x30000. JPEGs with less pixel data than their header promises are an error, not a
  panic. SVGs never decode embedded raster images (resvg's raster feature is off).
- **Tests:** fixed-seed random inputs check that nothing panics. That covers markup, text and
  mangled stylesheets through to pixels (`sliver-render/tests/robustness.rs`), script output
  lines (`sliver-modules` `output.rs`), and random format strings with random outputs through
  a full agent frame (`sliver-agent/tests/robustness.rs`).
- **Panic guard:** building a frame runs under `catch_unwind`. If a module panics anyway, that
  layer keeps its last good frame, the error is logged to the journal
  (`journalctl --user -u sliver-agent`), and the next change renders normally.

## Memory

A fresh agent uses about 20 MB. Two things made it look far bigger (hazel saw systemd report a
242–251 MB peak on the real install):

- **The font scan read every font file.** fontdb maps each system font to parse it, and a plain
  mapping reads ahead around each page fault: with the usual 4 MB `read_ahead_kb`, most of the
  file. So startup read ~214 MB (540 fonts) into the page cache, and systemd's MemoryPeak counts
  page cache charged to the unit's cgroup, though RSS stayed at 20 MB.
  `sliver_render::fontscan` scans instead, with the same fontconfig dirs and generic families.
  Each file is mapped once with `MADV_RANDOM` and kept in the database (`SharedFile`), which
  cosmic-text reuses rather than mapping the file again. The scan now reads ~14 MB, and the
  startup peak in a cold-cache cgroup dropped from 256 MB to ~42 MB.
- **A glyph that's in none of the fallback fonts loads every font.** cosmic-text keeps every
  font it loads, with its shaping data. Such a glyph (an odd symbol in a song title) makes it try
  every system font, whether one has it in the end (Nerd Fonts cover most private-use
  codepoints) or none does. That held 11 MB of heap forever, and with plain mappings it read
  every file again. Now the search reads only each font's cmap, ~2.5 MB of page cache in all.
  When more than 48 font files are loaded (`fontscan::loaded_files`: each loaded font holds a
  clone of its file's mapping), `Fonts` unloads them all, and also whenever the label cache
  turns over (`trim_loaded_fonts`). That rebuilds the `FontSystem` from the same database and
  drops the pages the mappings touched (`fontscan::release_pages`, `MADV_DONTNEED`). Shaped
  labels survive, so the same text never searches again. A new text that searches costs ~4 ms
  warm.
- **Each new font style reads every font.** For each new family/weight/style, cosmic-text builds
  a match list and reads every font's `fvar` table to do it (a variable font may cover the
  weight). That's a page or two of each font, ~14 MB, which used to stay in RSS (hazel's fresh
  agent showed 37 MB, 16 MB of it font pages). `Fonts` remembers the styles it has shaped and
  releases the pages after a shaping that needed a new one, so only the fonts in use stay
  resident (under 2 MB).

Measure in a cgroup of its own, after evicting the fonts from the page cache (any
`posix_fadvise(POSIX_FADV_DONTNEED)` loop over `/usr/share/fonts`):

```sh
cargo build --release -p sliver-render --examples
systemd-run --user --scope -q target/release/examples/fontscan [fontdb]   # scan only
systemd-run --user --scope -q target/release/examples/memprobe            # fallback texts
```

**Soak (T118):** `scripts/soak` runs the release agent on private buses and churns it for a few
minutes. An app publishes items with fresh ids, labels (some with missing glyphs) and PNG icons,
and drops and re-creates its context. A continuous script prints 20 lines a second, `sliverctl`
pushes text and toggles a layer, touches go in through sliverd's `--touch-socket` (taps, slides,
Fn presses), and the clock ticks every second. It fails if RSS grows by more
than a budget after warm-up. First run: heap flat at 25.4–25.5 MB over 3 min (about 1,700
publishes); RSS moves between 36 and 48 MB as fonts load and unload. No leak.

## Public D-Bus API
`api.rs` serves cedar's `sliver_api` on the session bus. `Host::changed` sets a flag and pokes a
wake socket the main loop polls. `Host::control` hands `Control1` requests to the main loop and
waits up to 2 s for the answer, so the core stays single-threaded.
- The agent's compositor focus goes to `Api::set_focus`. `published.rs` snapshots who fills App
  controls / Status items.
- The `app-controls` / `status-items` placeholder modules turn published items into cells:
  - classes `.app`/`.status`, the item type (`.button`, `.toggle`, `.slider`…), `.focused-app`
    and the item's own classes;
  - `:checked` for toggles that are on and the selected segment, `:disabled`, and the item's
    `fg`/`bg` colours.
- Touches on published items become `Api::interact`: gestures, `Select` for segments, and slider
  Begin/Change/End in the item's units, snapped to its step. A user `on-*` binding on the
  placeholder wins (and also sends the app event with `pass-through`).
- After each render the drawn contexts go to `OnScreen`, and sliverd's display state to the
  API. `Control1`: layer switch/push/pop/toggle, signal, refresh, trigger, bar show/hide/toggle,
  variables, preview PNGs, config validation, the module list, `SetLayout` (also the `layout`
  action: a config copy whose current layer shows that layout; empty name = back to rules),
  per-module show/hide/toggle/reset (runtime only), and `Update` (output pushed to exec-less
  custom modules), and profiles (`SetProfile`, persisted; also the `profile` action and
  sliverctl). `ReloadConfig` re-reads the config at once and answers with its errors (T26).
- Modal app contexts cover every zone but the user's first and get a × that sends `CloseModal`
  (`bar.modal`, `zone.zone-modal`, `#modal-close`).

Tests: `tests/api.rs` runs the agent with the API on a private `dbus-daemon` and a real client
(publish → render → tap/drag → `Event` signals).

## Status
Works: everything above, plus `--render` and the real sliverd loop. Verified end to end with
`sliverd --fake-hardware` + `sliverctl publish/status/modules/layer`.

Previews: `World::sample(now)` (sample battery, volume, brightness, now playing, window) with
`preview = true` draws empty App controls / Status items as outlined `.placeholder` cells.

The desktop and system modules (`workspaces`, `taskbar`, `notifications`, `cpu`, memory,
temperature, network…) are in `modules.rs`; `docs/features.md` has the full list and status.

## Hot reload (T26)
`main.rs` `Reloader` wraps `sliver-reload` (fir). inotify watches the *directories* of the
config, its includes, the stylesheets (including `@import`s) and where a user config would
appear, so atomic rename-saves and a config created later both work. Changes are debounced.
`sliverctl reload` (Control1 `ReloadConfig`) and SIGUSR2 reload at once.
A config with parse **or validation** errors is never applied: the last good config stays, the
errors are logged and listed in Control1 `Errors` (`sliverctl status`) until a good save (or a
revert). User modules restart
only if their specs changed (`Feeds::start_modules`). The realtime-signal forwarder is installed
once and retargeted, so replaced runtimes really stop.
