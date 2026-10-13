#!/usr/bin/env python3
"""A portal *backend* for the screenshot test: org.freedesktop.impl.portal.Screenshot and Access.

The real xdg-desktop-portal (the frontend, which apps and Sliver talk to) runs in front of it, so
the test covers everything except the compositor-specific part: grabbing pixels. Every call is
appended to the log file (argv[1]) as one JSON line; Screenshot answers with a file:// uri."""
import json
import sys

from gi.repository import Gio, GLib

LOG = sys.argv[1]
NAME = "org.freedesktop.impl.portal.desktop.sliver_test"
PATH = "/org/freedesktop/portal/desktop"

XML = """<node>
  <interface name="org.freedesktop.impl.portal.Screenshot">
    <method name="Screenshot">
      <arg type="o" name="handle" direction="in"/>
      <arg type="s" name="app_id" direction="in"/>
      <arg type="s" name="parent_window" direction="in"/>
      <arg type="a{sv}" name="options" direction="in"/>
      <arg type="u" name="response" direction="out"/>
      <arg type="a{sv}" name="results" direction="out"/>
    </method>
  </interface>
  <interface name="org.freedesktop.impl.portal.Access">
    <method name="AccessDialog">
      <arg type="o" name="handle" direction="in"/>
      <arg type="s" name="app_id" direction="in"/>
      <arg type="s" name="parent_window" direction="in"/>
      <arg type="s" name="title" direction="in"/>
      <arg type="s" name="subtitle" direction="in"/>
      <arg type="s" name="body" direction="in"/>
      <arg type="a{sv}" name="options" direction="in"/>
      <arg type="u" name="response" direction="out"/>
      <arg type="a{sv}" name="results" direction="out"/>
    </method>
  </interface>
</node>"""


def log(**kw):
    with open(LOG, "a") as f:
        f.write(json.dumps(kw) + "\n")


def on_call(conn, sender, path, iface, method, params, invocation):
    args = params.unpack()
    if method == "Screenshot":
        handle, app_id, parent, options = args
        log(method="Screenshot", parent=parent, options=options)
        invocation.return_value(
            GLib.Variant("(ua{sv})", (0, {"uri": GLib.Variant("s", "file:///tmp/sliver-test-shot.png")}))
        )
    else:
        log(method=method)
        invocation.return_value(GLib.Variant("(ua{sv})", (0, {})))


def on_bus(conn, name):
    for info in Gio.DBusNodeInfo.new_for_xml(XML).interfaces:
        conn.register_object(PATH, info, on_call, None, None)


loop = GLib.MainLoop()
Gio.bus_own_name(Gio.BusType.SESSION, NAME, Gio.BusNameOwnerFlags.NONE, on_bus, None, lambda *_: loop.quit())
loop.run()
