#!/usr/bin/env python3
"""A stand-in for sliver-agent's Desktop1 service: logs every call as one JSON line.

Usage: mock-desktop1.py LOGFILE   (runs on the session bus until killed)
"""
import json
import sys

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

XML = """
<node>
  <interface name="io.github.wawow830.Sliver.Desktop1">
    <method name="SetFocus"><arg name="window" type="a{sv}" direction="in"/></method>
    <method name="SetWorkspaces"><arg name="workspaces" type="aa{sv}" direction="in"/></method>
  </interface>
</node>
"""

log = open(sys.argv[1], "a", buffering=1)


def plain(v):
    """GVariant -> JSON-able, keeping the D-Bus type of leaves visible for the test."""
    t = v.get_type_string()
    if t == "v":
        return plain(v.get_variant())
    if t.startswith("a{"):
        return {k: plain(x) for k, x in ((e.get_child_value(0).get_string(), e.get_child_value(1)) for e in [v.get_child_value(i) for i in range(v.n_children())])}
    if t.startswith("a"):
        return [plain(v.get_child_value(i)) for i in range(v.n_children())]
    return {"type": t, "value": v.unpack()}


def on_call(conn, sender, path, iface, method, params, invocation):
    arg = params.get_child_value(0)
    log.write(json.dumps({"method": method, "arg": plain(arg), "sig": params.get_type_string()}) + "\n")
    invocation.return_value(None)


def on_bus(conn, name):
    node = Gio.DBusNodeInfo.new_for_xml(XML)
    conn.register_object("/io/github/wawow830/Sliver", node.interfaces[0], on_call, None, None)


Gio.bus_own_name(Gio.BusType.SESSION, "io.github.wawow830.Sliver", Gio.BusNameOwnerFlags.NONE, on_bus, None, None)
GLib.MainLoop().run()
