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
    };
    w.captionChanged.connect(again);
    w.fullScreenChanged.connect(again);
    if (w.outputChanged)
        w.outputChanged.connect(again);
}

workspace.windowActivated.connect(function (w) {
    watch(w);
    report(w);
});
workspace.windowAdded.connect(watch);
workspace.windowRemoved.connect(function (w) {
    if (current === w)
        report(workspace.activeWindow);
});
workspace.desktopsChanged.connect(reportWorkspaces);
workspace.currentDesktopChanged.connect(function () {
    reportWorkspaces();
    // The window set on the new desktop decides what is focused next; report what KWin says.
    report(workspace.activeWindow);
});

workspace.windowList().forEach(watch);
report(workspace.activeWindow);
reportWorkspaces();
