#!/usr/bin/env python3
"""A Pomodoro timer on the Touch Bar, in plain D-Bus (GLib/Gio, no sliver libraries).

    python3 examples/python/pomodoro.py

It publishes a `status` context (it shows up in the user's "Status items"), updates its label
every second, and reacts to taps. If sliver-agent restarts, it publishes again, like every good
client should (docs/api.md section 7). Customize it like any module:
`app/org.example.Pomodoro#timer` in your config and stylesheet.
"""

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

BUS = "io.github.wawow830.Sliver"
ROOT = "/io/github/wawow830/Sliver"
TOUCHBAR = BUS + ".TouchBar1"
CONTEXT = BUS + ".Context1"
WORK = 25 * 60


class Pomodoro:
    def __init__(self):
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION)
        self.path = None
        self.left = WORK
        self.running = False
        Gio.bus_watch_name_on_connection(
            self.bus, BUS, Gio.BusNameWatcherFlags.NONE, self.on_agent, self.on_agent_gone
        )
        # Event signals are unicast to us, so this subscription only ever sees our own events.
        self.bus.signal_subscribe(
            BUS, CONTEXT, "Event", None, None, Gio.DBusSignalFlags.NONE, self.on_event
        )
        GLib.timeout_add_seconds(1, self.tick)

    def call(self, path, iface, method, args, reply="()"):
        return self.bus.call_sync(
            BUS, path, iface, method, args, GLib.VariantType(reply),
            Gio.DBusCallFlags.NONE, -1, None,
        ).unpack()

    def label(self):
        m, s = divmod(self.left, 60)
        return ("⏸ " if self.running else "▶ ") + f"{m:02d}:{s:02d}"

    def items(self):
        v = GLib.Variant
        return [
            {
                "id": v("s", "timer"),
                "type": v("s", "button"),
                "label": v("s", self.label()),
                "classes": v("as", ["running"] if self.running else []),
                # Users can write format = "{minutes}m" against these.
                "values": v("a{sv}", {"minutes": v("i", self.left // 60), "seconds": v("i", self.left)}),
                "gestures": v("as", ["hold"]),
            }
        ]

    def on_agent(self, *_):
        opts = {
            "scope": GLib.Variant("s", "status"),
            "provider": GLib.Variant("s", "org.example.Pomodoro"),
            "title": GLib.Variant("s", "Pomodoro"),
        }
        (self.path,) = self.call(ROOT, TOUCHBAR, "CreateContext", GLib.Variant("(a{sv})", (opts,)), "(o)")
        self.call(self.path, CONTEXT, "SetItems", GLib.Variant("(aa{sv})", (self.items(),)), "(as)")

    def on_agent_gone(self, *_):
        self.path = None

    def on_event(self, _bus, _sender, path, _iface, _signal, params):
        item, kind, _data = params.unpack()
        if path != self.path or item != "timer":
            return
        if kind == "tap":
            self.running = not self.running
        elif kind == "hold":  # long-press resets
            self.running, self.left = False, WORK
        self.push()

    def tick(self):
        if self.running and self.left > 0:
            self.left -= 1
            if self.left == 0:
                self.running = False
            self.push()
        return True

    def push(self):
        if self.path is None:
            return
        upd = {"timer": {k: v for k, v in self.items()[0].items() if k not in ("id", "type")}}
        self.call(self.path, CONTEXT, "Update", GLib.Variant("(a{sa{sv}})", (upd,)), "(as)")


if __name__ == "__main__":
    Pomodoro()
    GLib.MainLoop().run()
