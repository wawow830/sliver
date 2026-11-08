pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Hyprland

Singleton {
    readonly property var windows: Hyprland.toplevels.values.filter(w => app(w))
    readonly property string focused: app(Hyprland.activeToplevel ?? windows.find(w => w.lastIpcObject.focusHistoryID === 0))
    readonly property var workspaces: Hyprland.workspaces.values.filter(w => w.id > 0).sort((a, b) => a.id - b.id)

    function app(window) {
        return window?.lastIpcObject?.class ?? ""
    }

    readonly property var entries: DesktopEntries.applications.values

    function icon(app) {
        const icon = (entries.length ? DesktopEntries.heuristicLookup(app)?.icon : "") || app
        return icon.startsWith("/") ? "file://" + icon : Quickshell.iconPath(icon, true)
    }

    function workspace(to) {
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
