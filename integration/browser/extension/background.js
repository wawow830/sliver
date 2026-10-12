// Sliver Touch Bar: tabs and navigation for the focused browser window.
//
// Talks to the native host `io.github.wawow830.sliver_browser` (integration/browser/host), which
// publishes them on the Touch Bar over Sliver's D-Bus API. Works in Firefox and Chromium (MV3).
"use strict";

const api = typeof browser !== "undefined" ? browser : chrome;
const HOST = "io.github.wawow830.sliver_browser";
const MAX_TABS = 256;

let port = null;
let focusedWindow = null;
let pending = null;

function connect() {
  port = api.runtime.connectNative(HOST);
  port.onMessage.addListener(onCommand);
  port.onDisconnect.addListener(() => {
    port = null;
    // The host exits when sliver isn't installed or crashed; try again later, not in a loop.
    setTimeout(connect, 30000);
  });
  report();
}

// Coalesce bursts of tab events (a session restore fires hundreds) into one message.
function report() {
  if (pending !== null) return;
  pending = setTimeout(async () => {
    pending = null;
    if (!port) return;
    const query = focusedWindow !== null ? { windowId: focusedWindow } : { lastFocusedWindow: true };
    const tabs = await api.tabs.query(query);
    port.postMessage({
      type: "tabs",
      tabs: tabs.slice(0, MAX_TABS).map((t) => ({
        id: t.id,
        title: t.title || t.url || "",
        active: !!t.active,
      })),
    });
  }, 50);
}

async function activeTab() {
  const query = focusedWindow !== null ? { windowId: focusedWindow, active: true } : { lastFocusedWindow: true, active: true };
  const [tab] = await api.tabs.query(query);
  return tab;
}

async function onCommand(msg) {
  const tab = await activeTab();
  switch (msg.cmd) {
    case "activate":
      await api.tabs.update(msg.tab, { active: true });
      break;
    case "back":
      if (tab) await api.tabs.goBack(tab.id);
      break;
    case "forward":
      if (tab) await api.tabs.goForward(tab.id);
      break;
    case "reload":
      if (tab) await api.tabs.reload(tab.id);
      break;
    case "new-tab":
      await api.tabs.create({});
      break;
  }
}

api.windows.onFocusChanged.addListener((id) => {
  if (id !== api.windows.WINDOW_ID_NONE) {
    focusedWindow = id;
    report();
  }
});
for (const ev of [api.tabs.onActivated, api.tabs.onCreated, api.tabs.onRemoved, api.tabs.onMoved,
                  api.tabs.onAttached, api.tabs.onDetached]) {
  ev.addListener(report);
}
api.tabs.onUpdated.addListener((_id, change) => {
  if ("title" in change || "url" in change) report();
});

connect();
