// Sliver focus: a KWin script (Plasma 6) that feeds the active window and virtual desktop to the
// Sliver session agent over D-Bus (io.github.wawow830.Sliver.Desktop1, docs/api.md section 5).
//
// KWin gives clients no way to ask "what is focused?", so the compositor side has to push.
// Nothing here talks to anything but the agent, and the agent is optional: if it is not running
// the calls fail quietly and the script keeps going. The first caller of SetFocus becomes the
// agent's focus source, so run only one of these (the package id is unique per session).

"use strict";

const SERVICE = "io.github.wawow830.Sliver";
const PATH = "/io/github/wawow830/Sliver";
const IFACE = SERVICE + ".Desktop1";

function call(method, arg) {
    // The trailing callback swallows "service not available" instead of logging per call.
    callDBus(SERVICE, PATH, IFACE, method, arg, function () {});
}

// Windows that are chrome rather than something the user is "in".
function isPlain(w) {
    return w && !w.desktopWindow && !w.dock && !w.specialWindow && !w.onScreenDisplay &&
        !w.notification && !w.criticalNotification && !w.appletPopup;
}

function desktopName() {
    const d = workspace.currentDesktop;
    return d ? String(d.name) : "";
}

function describe(w) {
    const d = {
        "app-id": String(w.desktopFileName || w.resourceClass || ""),
        "title": String(w.caption || ""),
        "window": "kwin:" + String(w.internalId),
        "workspace": desktopName(),
        "fullscreen": w.fullScreen ? 2 : 0,
        "floating": !!(w.keepAbove || w.transient),
    };
    if (w.pid > 0)
        d["pid"] = w.pid;
    if (w.output)
        d["monitor"] = String(w.output.name);
    return d;
}

function reportWorkspaces() {
    const active = workspace.currentDesktop;
    // KWin's callDBus sends JS arrays as `av`, so this uses SetWorkspacesV (elements are a{sv}).
    call("SetWorkspacesV", workspace.desktops.map(function (d, i) {
        return {
            "id": String(i + 1),
            "name": String(d.name),
            "active": !!active && d.id === active.id,
            "urgent": false,
        };
    }));
}

// The whole window list for the taskbar (Desktop1.SetWindowsV: elements are a{sv}, like
// SetFocus plus workspace (the SetWorkspaces id), focused and urgent). Only sent when it changed.
let lastWindows = "";

function desktopNumber(w) {
    // A window on every desktop (empty list) counts as being on the one you are looking at.
    const ds = w.desktops && w.desktops.length ? w.desktops : [workspace.currentDesktop];
    for (let i = 0; i < workspace.desktops.length; i++) {
        if (ds[0] && workspace.desktops[i].id === ds[0].id)
            return String(i + 1);
    }
    return "";
}

function reportWindows() {
    const active = workspace.activeWindow;
    const list = workspace.windowList().filter(isPlain).map(function (w) {
        const d = {
            "app-id": String(w.desktopFileName || w.resourceClass || ""),
            "title": String(w.caption || ""),
            "window": "kwin:" + String(w.internalId),
            "workspace": desktopNumber(w),
            "focused": w === active,
            "urgent": !!w.demandsAttention,
        };
        if (w.pid > 0)
            d["pid"] = w.pid;
        return d;
    });
    const key = JSON.stringify(list);
    if (key === lastWindows)
        return;
    lastWindows = key;
    call("SetWindowsV", list);
}

let current = null; // the window we last reported

function report(w) {
    current = isPlain(w) ? w : null;
    call("SetFocus", current ? describe(current) : {});
}

function watch(w) {
    if (!w || w.__sliverWatched)
        return;
    w.__sliverWatched = true;
    const again = function () {
        if (current === w)
            report(w);
        reportWindows();
    };
    w.captionChanged.connect(again);
    w.fullScreenChanged.connect(again);
    if (w.outputChanged)
        w.outputChanged.connect(again);
    // Things only the window list shows.
    if (w.desktopsChanged)
        w.desktopsChanged.connect(reportWindows);
    if (w.demandsAttentionChanged)
        w.demandsAttentionChanged.connect(reportWindows);
}

workspace.windowActivated.connect(function (w) {
    watch(w);
    report(w);
    reportWindows();
});
workspace.windowAdded.connect(function (w) {
    watch(w);
    reportWindows();
});
workspace.windowRemoved.connect(function (w) {
    if (current === w)
        report(workspace.activeWindow);
    reportWindows();
});
workspace.desktopsChanged.connect(function () {
    reportWorkspaces();
    reportWindows();
});
workspace.currentDesktopChanged.connect(function () {
    reportWorkspaces();
    reportWindows();
    // The window set on the new desktop decides what is focused next; report what KWin says.
    report(workspace.activeWindow);
});

workspace.windowList().forEach(watch);
report(workspace.activeWindow);
reportWorkspaces();
reportWindows();
