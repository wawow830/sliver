import QtQuick
import Quickshell

Row {
    id: taskbar
    property list<string> pinned
    property bool named

    readonly property var apps: {
        const apps = Array.from(pinned)
        for (const w of Desktop.windows)
            if (!apps.includes(Desktop.app(w)))
                apps.push(Desktop.app(w))
        return apps
    }

    spacing: 8

    Repeater {
        model: taskbar.apps

        Pill {
            id: app
            required property string modelData
            readonly property DesktopEntry entry: Desktop.entry(modelData)
            readonly property var windows: Desktop.windows.filter(w => Desktop.app(w) == modelData)
            readonly property bool focused: Desktop.focused == modelData
            readonly property string name: entry?.name ?? modelData
            property bool closing

            anchors.verticalCenter: parent.verticalCenter
            implicitWidth: text == "" ? 104 : content + 46
            Behavior on implicitWidth { NumberAnimation { duration: 260; easing.type: Easing.OutCubic } }

            image: closing ? "" : Desktop.icon(modelData)
            icon: closing ? "close" : ""
            text: closing ? `Close ${name}` : taskbar.named && focused ? name : ""
            fill: closing ? Theme.colors.error : taskbar.named && focused ? Theme.colors.primary_container : windows.length ? Theme.colors.surface_container_high : "transparent"
            ink: closing ? Theme.colors.surface_container : taskbar.named && focused ? Theme.colors.on_primary_container : Theme.colors.on_surface
            border.width: closing || (taskbar.named && focused) ? 0 : focused ? 3 : windows.length ? 0 : 2
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
    }
}
