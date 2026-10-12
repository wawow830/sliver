# Putting your app on the Touch Bar (plain D-Bus)

You don't need a Sliver library. Every toolkit with a D-Bus binding can publish buttons, toggles,
sliders and more, and react to touches. The full reference is `docs/api.md` (§2, §3 and §7);
this page is the short path through it, with a working recipe per toolkit.

| Toolkit | File | Status |
|---|---|---|
| Python / GLib | `examples/python/pomodoro.py` | run by a test on a private bus |
| Node.js / **Electron** (dbus-next) | `examples/node/counter.js` | run by a test on a private bus (`node_counter`; skipped when `dbus-next` isn't installed) |
| GTK / GNOME apps in **GJS** (Gio) | `examples/gjs/counter.js` | written against Gio, which the Python example exercises; **not run** (no `gjs` here) |
| **Qt** 6 (QtDBus, C++) | `examples/qt/counter.cpp` | **not compiled** (no Qt headers here); please send fixes |
| Shell | `sliverctl publish`, `busctl` (api.md §8) | tested |
| Rust | the `sliver-client` crate | tested |

## The recipe (every language does the same five things)

1. **Wait for the agent.** Watch the bus name `io.github.wawow830.Sliver` on the **session bus**
   (`Gio.bus_watch_name`, `QDBusServiceWatcher`, `NameOwnerChanged`). It may start after you, and
   it may restart. Don't treat "not there" as an error: publish when it appears, and publish
   **again** whenever its name gets a new owner, because your contexts die with it (api.md §7).
2. **Create a context.** `CreateContext(a{sv})` on `/io/github/wawow830/Sliver`, interface
   `…Sliver.TouchBar1`. Options: `scope` (`app`: shown while *your* window has focus; `status`: a
   background item in the user's "Status items"), a stable reverse-DNS `provider` (the user's
   preferences hang on it), and a `title`. It returns the context's object path.
3. **Publish items.** `SetItems(aa{sv})` on that path, interface `…Sliver.Context1`. Each item is a
   dictionary of variants with at least `id` (s) and `type` (s: `button`, `toggle`, `label`,
   `slider`, `segmented`, `scrubber`, `progress`, `spacer`, `group`, `popover`). Then `label`,
   `icon` (an XDG icon name), `classes`, `gestures`, `values` (named values for the user's own
   `format` string)… Keep item ids **stable between releases**: they're how users pin and style
   your items.
4. **React.** The `Event(s item, s kind, a{sv} data)` signal on the context says `tap`, `hold`,
   `toggled`, `value`, … It is sent to **your connection only** (so no other app sees your
   presses), but you still subscribe to it by the agent's name. `hold`, `swipe-*` and
   `double-tap` only arrive if the item lists them in `gestures`.
5. **Update, don't republish.** `Update(a{sa{sv}})` takes `{item id: {changed props}}` and applies
   atomically. The agent already flipped your toggle/slider optimistically; correct it with
   `Update` if your state disagrees.

## Be a good citizen of the bar

The Touch Bar is an OLED strip on a laptop battery. The agent redraws at most 30 times a second
however fast you call, and throttles sustained abuse (> 120 calls/s, api.md §4), so:

* **Update only on change** and only when something visible changed (a clock label once a
  minute, not once a second).
* **Stop while the bar is off or not showing you.** Read `NS.TouchBar1.DisplayState`
  (`on`/`dimmed`/`off`) and your context's `OnScreen` property (both emit `PropertiesChanged`) and
  pause timers while they say so.
* **Hide, don't destroy,** with `SetVisible(false)` when your feature is idle.

## Node.js and Electron (dbus-next)

```js
const dbus = require('dbus-next');
const { Variant } = dbus;
const bus = dbus.sessionBus();
const root = await bus.getProxyObject('io.github.wawow830.Sliver', '/io/github/wawow830/Sliver');
const path = await root.getInterface('io.github.wawow830.Sliver.TouchBar1').CreateContext({
  scope: new Variant('s', 'status'),
  provider: new Variant('s', 'org.example.MyApp'),
});
const ctx = (await bus.getProxyObject('io.github.wawow830.Sliver', path))
  .getInterface('io.github.wawow830.Sliver.Context1');
ctx.on('Event', (item, kind) => console.log(item, kind));
await ctx.SetItems([{ id: new Variant('s', 'go'), type: new Variant('s', 'button'),
                      label: new Variant('s', 'Go') }]);
```

* Every value of an `a{sv}` is a `new Variant(signature, value)`; arrays of strings are
  `new Variant('as', [...])`, nested dictionaries `new Variant('a{sv}', {...})`.
* In **Electron** do this in the **main process** (the renderer has no D-Bus) and forward events
  to windows with `webContents.send`. `examples/node/counter.js` is a complete, tested program
  with the restart handling.
* A **Flatpak** Electron app needs `--talk-name=io.github.wawow830.Sliver`; the user may also have
  to allow it for `status` scope (api.md §4, "Client tiers").

## GTK and GNOME apps (GJS, Gio)

Gio is the same API in every language, so `examples/gjs/counter.js` mirrors the Python example:
`Gio.bus_watch_name_on_connection` for the agent's name, `bus.call_sync` with `GLib.Variant`s
(`'(a{sv})'` for `CreateContext`, `'(aa{sv})'` for `SetItems`, `'(a{sa{sv}})'` for `Update`), and
`bus.signal_subscribe(BUS, CONTEXT, 'Event', …)`. For a GTK 4 app with a main loop already running
use the async `bus.call` instead of `call_sync`, so the UI never waits on the agent. Variants are
explicit: `GLib.Variant.new_string`, `new GLib.Variant('as', [...])`, `new GLib.Variant('a{sv}', {x: new GLib.Variant('i', 1)})`.

## Qt (QtDBus)

`examples/qt/counter.cpp` is a small Qt 6 program. The two things Qt needs spelled out:

* `aa{sv}` and `a{sa{sv}}` are `QList<QVariantMap>` and `QMap<QString, QVariantMap>`; register them
  once with `qDBusRegisterMetaType<…>()` before the first call.
* Plain `QVariantMap` is `a{sv}` (values are marshalled by their Qt type: `QString` → `s`,
  `int` → `i`, `bool` → `b`, `double` → `d`, `QStringList` → `as`). Qt has no unsigned/`short`
  surprises here; if you need a specific D-Bus type use `QDBusVariant`/`QDBusArgument`.

Connect to `Event` with `QDBusConnection::connect(service, path, interface, "Event", receiver,
SLOT(…))` using the agent's well-known name as `service`.

## Checking your work

```sh
busctl --user introspect io.github.wawow830.Sliver /io/github/wawow830/Sliver   # the interfaces
sliverctl contexts           # who has published what (and whether it is on screen)
sliverctl render -o out.png           # what the bar looks like, no hardware needed
```

`sliverctl publish` is the quickest way to see what a published item is supposed to look like
before you code it. The settings app's Apps page lists your provider and lets the user hide or
reorder it. If your items don't show up: the context may be `status` scope but hidden by the user,
or `app` scope while a different window has focus. Check `sliverctl contexts`.
