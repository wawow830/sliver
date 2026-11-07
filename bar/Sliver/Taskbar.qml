import QtQuick
import Quickshell

Item {
    id: taskbar
    property list<string> pinned
    property int slots: 5

    readonly property var apps: {
        const apps = Array.from(pinned)
        for (const w of Desktop.windows)
            if (!apps.includes(Desktop.app(w)))
                apps.push(Desktop.app(w))
        return apps
    }
    readonly property bool overflows: apps.length > slots
    readonly property var overflow: overflows ? apps.slice(slots - 1) : []

    implicitWidth: slots * 104 + (slots - 1) * 8

    component App: Pill {
        id: app
        property string app
        readonly property DesktopEntry entry: Desktop.entry(app)
        readonly property var windows: Desktop.windows.filter(w => Desktop.app(w) == app.app)
        readonly property bool focused: Desktop.focused == app.app
        readonly property string name: entry?.name || app.app.split(".").pop()
        readonly property string art: Desktop.icon(app.app)
        property bool closing

        anchors.verticalCenter: parent.verticalCenter
        implicitWidth: 104
        image: closing ? "" : art
        icon: closing ? "close" : ""
        text: closing || art != "" ? "" : name.charAt(0).toUpperCase()
        fill: closing ? Theme.colors.error : windows.length ? Theme.colors.surface_container_high : "transparent"
        ink: closing ? Theme.colors.surface_container : Theme.colors.on_surface
        border.width: closing ? 0 : focused ? 3 : windows.length ? 0 : 2
        border.color: focused ? Theme.colors.primary : Theme.colors.outline
        opacity: windows.length || closing ? 1 : 0.55

        onTapped: {
            if (closing) {
                closing = false
                for (const w of windows)
                    Desktop.close(w)
            } else if (!windows.length) {
                entry?.execute()
            } else {
                const next = windows.findIndex(w => w.activated) + 1
                Desktop.focus(windows[next % windows.length])
            }
        }
        onLongPressed: closing = windows.length > 0

        Timer {
            running: app.closing
            interval: 3000
            onTriggered: app.closing = false
        }
    }

    Row {
        anchors.fill: parent
        spacing: 8

        Repeater {
            model: taskbar.overflows ? taskbar.apps.slice(0, taskbar.slots - 1) : taskbar.apps

            App {
                required property string modelData
                app: modelData
            }
        }

        App {
            readonly property int at: taskbar.overflow.indexOf(Desktop.focused)

            visible: taskbar.overflows
            app: at < 0 ? "" : Desktop.focused
            image: at < 0 ? "" : art
            text: at < 0 ? `+${taskbar.overflow.length}` : ""
            fill: Theme.colors.surface_container_high
            border.width: at < 0 ? 0 : 3
            opacity: 1
            onTapped: {
                const next = taskbar.overflow[(at + 1) % taskbar.overflow.length]
                const window = Desktop.windows.find(w => Desktop.app(w) == next)
                if (window)
                    Desktop.focus(window)
            }
            onLongPressed: {}
        }
    }
}
