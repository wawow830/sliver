# Sliver API

Owner: **cedar** (ADR-0003). Changes to anything in this file: message cedar or amend the ADR.
Status: **draft v1**. Nothing here is public until a release ships. Bus name settled by the human (H1).

Sliver has two API surfaces:

| | Public session API | Private IPC |
|---|---|---|
| between | any app/script/DE ⇄ `sliver-agent` | `sliver-agent` ⇄ `sliverd` |
| transport | D-Bus, session bus | `AF_UNIX` `SOCK_SEQPACKET`, `/run/sliver/sliverd.sock` |
| encoding | D-Bus types, `a{sv}` for anything that may grow | one JSON object per packet, fds via `SCM_RIGHTS` |
| stability | versioned, documented, permanent | versioned, may change between releases (both ship together) |
| trust | same uid; sandboxed apps restricted | sliverd trusts nothing the agent says |

Names (all defined once in `sliver_ipc::dbus`):

```
NS       = io.github.wawow830.Sliver
bus name = io.github.wawow830.Sliver            (owned by sliver-agent)
root     = /io/github/wawow830/Sliver
ifaces   = NS.TouchBar1  NS.Context1  NS.Control1  NS.Desktop1
```

The trailing digit is the **major** version. A breaking change creates `TouchBar2` alongside `TouchBar1`.
Additive changes bump the `Version` property (minor).

---

## 1. Mental model

```
  Esc zone   Main zone                                                Controls zone
 ┌─────┬──────────────────────────────────────────────────────────┬──────────────────────────────┐
 │ Esc │ ◀ ▶  [ App controls: focused app's context ]             │ [Status items] ☀ 🔊 🕒  ‹    │
 └─────┴──────────────────────────────────────────────────────────┴──────────────────────────────┘
```

Terms match docs/ux.md (elm). The **user's layout** (config, settings app) has three zones: **Esc**
(pinned), **Main**, and **Controls** (collapsible control strip). Apps never decide the layout. Their
items only appear where the user put one of two **placeholder items**:
- **App controls**: the focused app's context. The default layout puts it in Main.
- **Status items**: third-party `status` contexts. The default layout puts it in Controls.

If a layout has no placeholder, those items don't show. That's how the user opts out per layout.
Rules, layers and layout resolution all happen in the agent. sliverd only knows the safe layer.

- A **context** is a list of **items** published by one D-Bus client (NSTouchBar analogue).
  It has a **scope**:
  - `app`: fills App controls while one of *the caller's own* windows is focused (verified, see §4).
  - `target`: fills App controls while a window matching a selector is focused
    (a script adding controls for another app; BetterTouchTool-style). Unverified, lower priority.
  - `status`: shown in Status items regardless of focus (StatusNotifierItem/Control Strip analogue).
- An `app` context may be **modal** (color picker, recording controls). It then covers Main and
  Controls, **never Esc**, if the user's per-app "may take the whole bar" setting allows (default on).
  The agent always draws a ✕ on it. Modal ends on ✕, on focus loss, or when the app hides the context.
- **Items** are declarative (button, toggle, slider, …). The agent renders them, does hit-testing and
  gesture recognition, and sends **events** back to the owner. Apps never draw pixels.
- Contexts live exactly as long as the owner's bus connection. Persistent customization belongs in
  config (via `Control1` or the settings app), not in long-lived D-Bus objects.
- If the agent is gone, sliverd shows its built-in safe layer (Esc, F1–F12, brightness/media).

## 2. `NS.TouchBar1` at `root`

The entry point for apps.

Methods
```
CreateContext(a{sv} options) -> (o context)
```
`options` (all optional):

| key | type | meaning |
|---|---|---|
| `scope` | s | `app` (default), `target`, `status` |
| `provider` | s | stable reverse-DNS id of the publisher, e.g. `org.example.Pomodoro`. Keys user prefs (hide, reorder). Default: verified app id, else the unique name (prefs then don't persist). |
| `title` | s | human-readable name for the settings app |
| `window` | s | `app` scope: bind to one toplevel, portal-style handle `wayland:<xdg-foreign handle>`, `x11:<hex xid>`, or `hyprland:<address>`. The agent falls back to app-level binding if it can't resolve it. |
| `target` | a{sv} | `target` scope selector: `app-id` s, `workspace` s, `title-regex` s. All given keys must match. |
| `include-children` | b | `app` scope: also match when the focused window's process is an ancestor of the caller (a program in a terminal). Lower priority than the terminal's own context. |
| `modal` | b | `app` scope: cover Main + Controls while on screen (see §1). The owner gets `Event("", "closed", {})` when the user dismisses it. |

Item identity: `(provider, item id)` is the stable key the user's prefs use to hide, reorder or pin app
items into their own layouts. **Keep item ids stable across releases.**

Properties (read-only, `PropertiesChanged` emitted)

| name | type | meaning |
|---|---|---|
| `Version` | u | minor version of this interface (starts at 0) |
| `Features` | as | optional capabilities, e.g. `item:slider`, `item:popover`, `gesture:long-press` |
| `DisplayState` | s | `on`, `dimmed`, `off`. **Stop non-essential updates while `off`** (power invariant). |

## 3. `NS.Context1` at `root/context/<n>`

Only the creating connection may call these. Everyone else gets `NS.Error.AccessDenied`.

Methods
```
SetItems(aa{sv} items) -> (as warnings)          replace all items atomically
Update(a{sa{sv}} changes) -> (as warnings)       item id -> partial props, applied atomically
SetVisible(b visible)                            hide without destroying; default visible
Destroy()
```
`warnings` lists ignored unknown keys/types and clamped values, so newer clients degrade
gracefully on older agents.

Properties: `OnScreen` b (is any of this context currently drawn on the bar), `Scope` s, `Provider` s.
Clients **should** stop updating while `OnScreen` is false.

Signal (sent **unicast** to the owner only)
```
Event(s item, s kind, a{sv} data)
```

| kind | data | sent when |
|---|---|---|
| `tap` | `{}` | button/toggle activated (touch up inside, no other gesture recognised) |
| `toggled` | `state` b | toggle changed (the agent flips it immediately, optimistic) |
| `long-press` | `{}` | only if the item lists `long-press` in `gestures` |
| `swipe` | `direction` s (`left`/`right`) | only if the item lists `swipe` in `gestures` |
| `value` | `value` d, `phase` s (`begin`/`change`/`end`) | slider dragged (agent moves it immediately; changes coalesced to ≤ 60/s) |
| `selected` | `index` i | segmented control |
| `popover` | `open` b | popover opened/closed |
| `closed` | `{}` | modal context dismissed by the user (item is `""`) |

Optimistic state: the agent updates toggles/sliders/segments locally at once. The app is the source
of truth and may correct it with `Update`.

Within one client, among visible contexts with the same scope and binding, the **most recently**
created/shown one wins (responder-chain analogue: swap contexts as your internal focus changes).

### Items

Each item is an `a{sv}` with required `id` (s, unique in the context) and `type` (s).

Common props: `label` s · `icon` s (XDG icon name; `-symbolic` icons are recoloured) ·
`icon-data` ay (PNG or SVG bytes, ≤ 256 KiB) · `enabled` b · `visible` b ·
`fg` / `bg` s (`#rrggbb[aa]`, may be overridden by the theme) · `width` a{sv} (`min`, `max` in
logical px; default: fit content) · `priority` i (higher survives overflow; NSTouchBar
visibilityPriority) · `gestures` as · `a11y-label` s · `customizable` b (user may hide/reorder it;
default true).

| type | extra props |
|---|---|
| `button` | — |
| `toggle` | `state` b |
| `label` | `label` only; not interactive |
| `slider` | `value` d, `min` d (0), `max` d (1), `step` d (0 = continuous), `label`/`icon` as leading glyph |
| `segmented` | `segments` aa{sv} (each `label`/`icon`), `selected` i (-1 = none) |
| `progress` | `value` d in [0,1], or `indeterminate` b |
| `spacer` | `size` s: `small`, `large`, `flexible` |
| `group` | `items` aa{sv} (one level of nesting, laid out together, overflow as a unit) |
| `popover` | `items` aa{sv}; tapping shows them full-width with a close button (NSPopoverTouchBarItem) |

Reserved for later minors: `scrubber`, `color-picker`, `image`. Unknown types are skipped with a warning.

Labels are plain text (no markup), control characters stripped, truncated to 64 chars.
Emoji are just text.

## 4. Focus, priority and permissions

### Binding an `app` context to windows (verified)
The agent knows the focused toplevel (`app-id`, `pid`, workspace, title) from its compositor backend
(Hyprland/sway IPC, or a DE integration via `Desktop1`). An `app` context of a client with pid `P`
(from `GetConnectionCredentials`, pidfd when available) is **active** when the focused window's pid
is `P`, or both are in the same app instance: same Flatpak instance (`/.flatpak-info`), or the same
systemd app unit (`app-*.scope` / `app-*.service` cgroup). Claims a client makes about its own app id
are never trusted for `app` scope.

### Who fills App controls, highest first
1. **The user.** A per-app layout in config always wins (`app_source = "user"`), and the user can pin
   a specific provider or disable app contexts per app.
2. The focused app's own `app` context (window-bound before app-bound).
3. `app` contexts with `include-children` (e.g. vim in a terminal).
4. `target` contexts whose selector matches (most recently shown first).
5. Nothing: the placeholder collapses.

While the session is **locked**, App controls and Status items are empty whatever the layout says (privacy).

Status zone: all visible `status` contexts, ordered by user prefs, then provider id. Overflow drops the
lowest `priority` items; the user can hide any provider.

### Client tiers

| | unsandboxed (same uid) | sandboxed (Flatpak/Snap) |
|---|---|---|
| `app` scope | yes | yes |
| `target` scope | yes (user can disable globally) | no |
| `status` scope | yes, user can hide | only after the user allows that app id |
| `Control1`, `Desktop1`, focus info | yes | no |

Sandboxed apps also need `--talk-name=NS` in their manifest. Same-uid unsandboxed code can already edit
our config, so these rules are about **fairness and UX**, not a security boundary. The security boundary
is sliverd (§6).

### Abuse limits (per connection; exceeding returns `NS.Error.LimitExceeded`)
16 contexts, 128 items per context, 512 KiB of icon data per context. Updates are coalesced, never
queued: the agent renders at most once per panel refresh and only on change, so a client calling
`Update` in a loop costs one render per frame, and sustained abuse (> 120 calls/s) is throttled.
Clients can't emit key presses, read other clients' items, or force the bar on.

## 5. `NS.Control1` and `NS.Desktop1` at `root`

For `sliverctl`, the settings app and DE integrations. Unsandboxed clients only.

`NS.Control1`
```
SwitchLayer(s name)                 PushLayer(s name, u timeout_ms)   PopLayer()
SetProfile(s name)                  ReloadConfig() -> (as errors)
ValidateConfig(s toml) -> (as errors)
Preview(s layout_toml, u timeout_ms)       show on the real bar, auto-revert (settings live preview)
RenderPreview(s layout_toml, a{sv} options) -> (ay png)   same renderer, for on-screen WYSIWYG
SetVariable(s name, v value)        user variables usable in context conditions ("per state" layouts)
ListContexts() -> (aa{sv})          provider, title, scope, on-screen; for the settings app
```
Properties: `ActiveLayer` s, `Layers` as, `ActiveProfile` s, `Profiles` as, `Variables` a{sv}, `Healthy` b
(agent connected to sliverd and driving the bar).

`NS.Desktop1`: lets a compositor integration that the agent can't query directly (GNOME extension, KWin
script) feed it state.
```
SetFocus(a{sv} window)      app-id s, pid u, title s, workspace s, window s (handle as in §2)
SetWorkspaces(aa{sv})       id s, name s, active b, urgent b
```
The first connection to call `SetFocus` becomes the focus source until it disconnects (others get
`NS.Error.Busy`). A configured backend (Hyprland/sway IPC) takes precedence.

## 6. Private IPC: `sliverd` ⇄ `sliver-agent`

### Split of responsibilities
`sliverd` (privileged, small, crash-proof) owns DRM, touch input, the Touch Bar backlight, the Fn key
and uinput. It knows nothing about apps, config or widgets. `sliver-agent` (user) renders whole frames
and decides what they mean. sliverd **does** know:
- **key regions:** rectangles that emit evdev keys. Hit-testing, pressed feedback and key emission
  happen inside sliverd, so Esc/F-keys never wait on the agent and keep working if it hangs.
- **two layers:** `base` and `fn`. sliverd swaps them locally while Fn is held (no round trip).
- its **built-in safe layer**, shown whenever no healthy agent drives the bar.

### Transport
- `AF_UNIX`, `SOCK_SEQPACKET` at `/run/sliver/sliverd.sock` (`RuntimeDirectory=sliver`, mode 0666).
  One packet = one message, ≤ 64 KiB, UTF-8 JSON object tagged by `"t"`. Unknown fields are ignored;
  unknown `t` gets `Error{code:"unknown_message"}` and is otherwise ignored.
- Authorisation: `SO_PEERCRED` uid must own the **active session on seat0** (logind). Connections from
  other uids are kept in `standby` and promoted on session switch. If a second connection from the
  active uid says `Hello`, it takes over and the old one gets `Revoked`.
- Pixels travel in sealed memfds, never in packets.

Implemented in `crates/sliver-ipc`. `proto` has the types and codec, `validate` and `keys` the checks below,
`transport` the seqpacket connection with fd passing (peer hangup *and* reset both read as `Ok(None)`),
`shm` the sealed memfd buffers, and `router` the key-region touch semantics. Feature `fake` adds
`fake::FakeDaemon`, an in-process sliverd that speaks the real protocol: the agent's headless tests run
against it, and T8 can use it as a reference. Error codes on the wire are the snake_case `ErrorCode` variants.

### Handshake
```
agent → {"t":"hello","proto":[1,0],"agent":"sliver-agent 0.1.0"}
sliverd → {"t":"welcome","proto":[1,0],"panel":{"width":2008,"height":60,"format":"xrgb8888",
           "max_brightness":255,"model":"Mac14,7"},"role":"active"|"standby","features":[]}
```
Major mismatch: sliverd sends `Error{code:"proto"}`, closes, and keeps the safe layer.
Minor: both sides use the lower one. Geometry is **logical**: landscape, origin top-left as the user sees it;
sliverd rotates to the panel (`60x2008` on Mac14,7). Intel T2 bars report their own size.

### Agent → sliverd
| t | fields | |
|---|---|---|
| `attach_buffer` | `id` u32, `width`, `height`, `stride`, `format` + **1 fd** | memfd must carry `F_SEAL_SHRINK`, `F_SEAL_GROW` and be ≥ stride×height, else rejected. ≤ 8 buffers. |
| `detach_buffer` | `id` | |
| `present` | `serial` u32, `layer` (`base`/`fn`), `image` id, `pressed` id?, `regions` [Region], `damage` [[x,w]]? | sliverd **copies** the buffer(s) and replies `presented`. Agent may reuse the buffer after that. |
| `clear_layer` | `layer` | `fn` only: fall back to the built-in fn layer |
| `display_policy` | `brightness` (0..max), `dim_after_ms`, `dim_brightness`, `off_after_ms`, `follow_screen` b | sliverd keeps its defaults until set |
| `wake` | — | leave dim/off (e.g. new notification); rate-limited 1/s |
| `emit_keys` | `keys` [u16], `mode` (`tap`/`down`/`up`) | for keys decided by agent regions (long-press → key, macros). Rate-limited. |
| `pong` | `nonce` | |
| `bye` | — | clean exit: safe layer now |

`Region`: `{"id":u32,"rect":[x,y,w,h],"kind":"key","keys":[u16…]}` or `{"id":…,"rect":…,"kind":"agent"}`.
`keys` is pressed in order and released in reverse (combos). `pressed` is an optional second image with
every region drawn pressed; sliverd blits the pressed region's rect from it, or applies a generic
highlight if absent.

### sliverd → agent
| t | fields | |
|---|---|---|
| `presented` | `serial` | buffer copied; also implies the frame is on screen (or will be when the display wakes) |
| `touch` | `region` u32, `slot` u8, `phase` (`down`/`move`/`up`/`cancel`), `x`, `y` (f32 logical px), `time_us` u64 | only for touches that **started** in an `agent` region; captured until up even if they leave it |
| `fn` | `held` b | |
| `display` | `state` (`on`/`dimmed`/`off`) | agent stops rendering and widget timers while `off` |
| `role` | `role` (`active`/`standby`/`revoked`) | session switches; non-active agents' presents are dropped |
| `ping` | `nonce` | |
| `error` | `code`, `msg`, `serial`? | rejected message; never fatal by itself |

### Safety rules sliverd enforces (invariant 1)
- **Liveness:** an agent crash is a socket hangup, detected at once. Hangs are checked only when they
  matter, with no idle timers (invariant 3): sliverd sends `ping` when the display wakes, and on an
  agent-region touch if it hasn't pinged in the last 5 s. No `pong` within 2 s, a disconnect, `bye`, or a
  malformed frame → built-in safe layer at once. Key regions of the last good frame keep working until then.
- **Escape floor:** a frame is accepted only if `base ∪ fn` (with the built-in fn layer standing in
  for a cleared one) contains single-key regions for `KEY_F1`–`KEY_F12`, plus `KEY_ESC` on machines
  without a physical Esc. Each must be ≥ 32 px wide and fully on screen. Physical Esc follows tiny-dfr's
  heuristic: bars ≥ 2170 px (2016–19) have none, and the 2008 px bars (this Mac14,7, confirmed in H2)
  have one. `/etc/sliver/sliverd.toml` can override it. Otherwise sliverd replaces the `fn` layer with
  its built-in one and sends `error{code:"escape_floor"}`. The settings editor is stricter (docs/ux.md §7).
- **Validation:** geometry in bounds, ≤ 128 regions, no overlapping key regions, keycodes in range.
  **`KEY_SYSRQ` is refused** in regions and `emit_keys` (a same-uid process must not get kernel SysRq).
- **No trust in the buffer:** sealed memfds only, copied before use, mmap failures and short sizes → reject.
- Bounded everything: queues, buffers, message size. A misbehaving agent is disconnected, never followed.

### Key-region touch semantics
A touch that starts on a key region presses its keys, and lifting releases them. Sliding off the region
releases them, and sliding back presses them again. A touch never presses a second region. Each
slot is independent, so multi-touch chords work. This is tiny-dfr parity (docs/hardware.md), with one
deliberate change: combos are released in **reverse** order (modifiers last). T8 implements it.

## 7. Lifecycles
- **Agent restart:** clients see `NameOwnerChanged` for `NS` and must re-create their contexts.
  The Rust client crate does this automatically. Meanwhile sliverd shows the safe layer.
- **Client exit:** its contexts vanish immediately (the agent watches `NameOwnerChanged` for unique names).
- **Activation:** `sliver-agent.service` is a systemd user unit wanted by `graphical-session.target`,
  with a D-Bus activation file pointing at it.
- **Display off:** sliverd → `display off` → agent stops rendering and sets `DisplayState=off` →
  well-behaved clients stop updating. Touch wakes the bar; that touch is swallowed.

## 8. Examples

Shell (`busctl`):
```sh
ctx=$(busctl --user call io.github.wawow830.Sliver /io/github/wawow830/Sliver \
  io.github.wawow830.Sliver.TouchBar1 CreateContext 'a{sv}' 2 scope s status provider s org.example.Hello \
  | cut -d'"' -f2)
busctl --user call io.github.wawow830.Sliver "$ctx" io.github.wawow830.Sliver.Context1 SetItems \
  'aa{sv}' 1 3 id s hi type s button label s 'Hello'
# The context dies with busctl's connection: real scripts use `sliverctl publish` (stays connected,
# prints one event per line) or the Python example.
```

`sliverctl` (planned):
```sh
sliverctl publish --scope status --provider org.example.Hello button:hi:Hello |
  while read -r item kind _; do notify-send "$item $kind"; done
```

## 9. Prior art and what we took
- **NSTouchBar:** declarative items, app provides the bar for its own focus, popovers, visibility
  priority, customizable items. Not taken: the responder chain (we use "latest visible context wins").
- **Control Strip:** a Controls zone apps can't touch (except via the Status items placeholder).
- **BetterTouchTool / MTMR:** per-app user layouts beating the app, user variables as conditions, scripts targeting other apps.
- **Pock:** widgets as a first-class concept (built in to the agent, status contexts for external ones).
- **MPRIS:** `PropertiesChanged`-driven state, re-discovery on name owner change.
- **StatusNotifierItem:** avoided its pain: no raw ARGB pixmaps in network byte order, no separate menu
  protocol (popover is an item), no host/watcher split, re-registration made automatic in the client crate.
