pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Hyprland

Singleton {
    readonly property var windows: Hyprland.toplevels.values.filter(w => app(w))
    readonly property string focused: app(Hyprland.activeToplevel ?? windows.find(w => w.lastIpcObject.focusHistoryID === 0))

    function app(window) {
        return window?.lastIpcObject?.class ?? ""
    }

    readonly property var entries: DesktopEntries.applications.values

    function entry(app) {
        return entries.length ? DesktopEntries.heuristicLookup(app) : null
    }

    function icon(app) {
        const icon = entry(app)?.icon || app
        return icon.startsWith("/") ? "file://" + icon : Quickshell.iconPath(icon, true)
    }

    function selector(window) {
        return "address:0x" + window.address
    }

    function focus(window) {
        Hyprland.dispatch(Hyprland.usingLua ? `hl.dsp.focus({ window = "${selector(window)}" })` : `focuswindow ${selector(window)}`)
    }

    function close(window) {
        Hyprland.dispatch(Hyprland.usingLua ? `hl.dsp.window.close({ window = "${selector(window)}" })` : `closewindow ${selector(window)}`)
    }

    function workspace(step) {
        const to = (step < 0 ? "r" : "r+") + step
        Hyprland.dispatch(Hyprland.usingLua ? `hl.dsp.focus({ workspace = "${to}" })` : `workspace ${to}`)
    }

    Connections {
        target: Hyprland
        function onRawEvent(event) {
            if (event.name == "openwindow")
                Hyprland.refreshToplevels()
        }
    }
}
