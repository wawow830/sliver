#!/usr/bin/env -S gjs -m
// A counter on the Touch Bar from a GTK/GNOME app written in GJS, in plain D-Bus (Gio). It is
// examples/python/pomodoro.py's twin: Gio is the same API in every language.
// docs/integrating-apps.md explains it step by step.
//
//     gjs -m examples/gjs/counter.js

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

const BUS = 'io.github.wawow830.Sliver';
const ROOT = '/io/github/wawow830/Sliver';
const TOUCHBAR = `${BUS}.TouchBar1`;
const CONTEXT = `${BUS}.Context1`;

const bus = Gio.bus_get_sync(Gio.BusType.SESSION, null);
let count = 0;
let path = null;

const call = (objPath, iface, method, args, reply) =>
    bus.call_sync(BUS, objPath, iface, method, args, new GLib.VariantType(reply),
                  Gio.DBusCallFlags.NONE, -1, null).deepUnpack();

const v = (type, value) => new GLib.Variant(type, value);

function item() {
    return {
        id: v('s', 'count'),
        type: v('s', 'button'),
        label: v('s', `Clicked ${count}`),
        gestures: v('as', ['hold']),
        values: v('a{sv}', {count: v('i', count)}),
    };
}

function publish() {
    const options = {
        scope: v('s', 'status'),
        provider: v('s', 'org.example.GjsCounter'),
        title: v('s', 'GJS counter'),
    };
    [path] = call(ROOT, TOUCHBAR, 'CreateContext', new GLib.Variant('(a{sv})', [options]), '(o)');
    call(path, CONTEXT, 'SetItems', new GLib.Variant('(aa{sv})', [[item()]]), '(as)');
}

function onEvent(_bus, _sender, objPath, _iface, _signal, params) {
    const [id, kind] = params.deepUnpack();
    if (objPath !== path || id !== 'count')
        return;
    if (kind === 'tap')
        count += 1;
    else if (kind === 'hold')
        count = 0;
    else
        return;
    const {label, values} = item();
    call(path, CONTEXT, 'Update',
         new GLib.Variant('(a{sa{sv}})', [{count: {label, values}}]), '(as)');
}

// Events are unicast to us, so subscribing to the name sees only our own.
bus.signal_subscribe(BUS, CONTEXT, 'Event', null, null, Gio.DBusSignalFlags.NONE, onEvent);

// Publish when the agent appears (now, later, or after a restart); forget the context when it goes.
Gio.bus_watch_name_on_connection(bus, BUS, Gio.BusNameWatcherFlags.NONE,
    () => publish(), () => { path = null; });

new GLib.MainLoop(null, false).run();
