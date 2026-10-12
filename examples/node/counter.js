#!/usr/bin/env node
// A counter on the Touch Bar from Node.js or an Electron main process, in plain D-Bus
// (dbus-next, no sliver libraries). docs/integrating-apps.md explains it step by step.
//
//     npm install dbus-next
//     node examples/node/counter.js
//
// It publishes one `button` in the user's "Status items", counts taps, resets on a long press,
// and publishes again if sliver-agent restarts.

const dbus = require('dbus-next');
const { Variant } = dbus;

const BUS = 'io.github.wawow830.Sliver';
const ROOT = '/io/github/wawow830/Sliver';

const bus = dbus.sessionBus();
let count = 0;
let context = null; // the Context1 interface while we have one

const items = () => [{
  id: new Variant('s', 'count'),
  type: new Variant('s', 'button'),
  label: new Variant('s', `Clicked ${count}`),
  gestures: new Variant('as', ['hold']),
  // For the user's `format = "{count} clicks"` (docs/api.md §3.1)
  values: new Variant('a{sv}', { count: new Variant('i', count) }),
}];

async function publish() {
  const root = await bus.getProxyObject(BUS, ROOT);
  const touchbar = root.getInterface(`${BUS}.TouchBar1`);
  const path = await touchbar.CreateContext({
    scope: new Variant('s', 'status'),
    provider: new Variant('s', 'org.example.NodeCounter'),
    title: new Variant('s', 'Node counter'),
  });
  const obj = await bus.getProxyObject(BUS, path);
  context = obj.getInterface(`${BUS}.Context1`);
  // Events are sent to the owner of the context only, so this sees nothing but ours.
  context.on('Event', (item, kind) => {
    if (item !== 'count') return;
    if (kind === 'tap') count += 1;
    else if (kind === 'hold') count = 0;
    else return;
    // Update takes {item id: {changed props}}
    context.Update({
      count: {
        label: new Variant('s', `Clicked ${count}`),
        values: new Variant('a{sv}', { count: new Variant('i', count) }),
      },
    }).catch(() => {});
  });
  await context.SetItems(items());
}

async function main() {
  // The agent may start later or restart: publish whenever its name gets an owner.
  const dbusObj = await bus.getProxyObject('org.freedesktop.DBus', '/org/freedesktop/DBus');
  dbusObj.getInterface('org.freedesktop.DBus').on('NameOwnerChanged', (name, _old, now) => {
    if (name !== BUS) return;
    context = null;
    if (now) publish().catch(() => {});
  });
  await publish().catch(() => {}); // not running yet: the watcher above will do it
}

main();
