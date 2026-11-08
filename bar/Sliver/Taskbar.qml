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
    readonly property var shown: overflows ? apps.slice(0, slots - 1) : apps
    readonly property int focused: overflow.includes(Desktop.focused) ? slots - 1 : shown.indexOf(Desktop.focused)
    readonly property int slot: 88

    implicitWidth: slots * slot + (slots - 1) * 8

    component App: Pill {
        id: app
        property string app
        readonly property DesktopEntry entry: Desktop.entry(app)
        readonly property var windows: Desktop.windows.filter(w => Desktop.app(w) == app.app)
        readonly property string name: entry?.name || app.app.split(".").pop()
        readonly property string art: Desktop.icon(app.app)
        property bool closing
        property bool launching
        property real pulse

        anchors.verticalCenter: parent.verticalCenter
        implicitWidth: taskbar.slot
        image: closing ? "" : art
        icon: closing ? "close" : ""
        text: closing || art != "" ? "" : name.charAt(0).toUpperCase()
        fill: closing ? Theme.colors.error : "transparent"
        ink: closing ? Theme.colors.surface_container : Theme.colors.on_surface
        opacity: windows.length || closing ? 1 : launching ? pulse : 0.4
        onWindowsChanged: launching = false

        onTapped: {
            if (closing) {
                closing = false
                for (const w of windows)
                    Desktop.close(w)
            } else if (!windows.length) {
                launching = entry != null
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

        Timer {
            running: app.launching
            interval: 10000
            onTriggered: app.launching = false
        }

        SequentialAnimation on pulse {
            running: app.launching
            loops: Animation.Infinite
            NumberAnimation { from: 0.4; to: 1; duration: 450; easing.type: Easing.InOutSine }
            NumberAnimation { to: 0.4; duration: 450; easing.type: Easing.InOutSine }
        }
    }

    Rectangle {
        property int at

        anchors.verticalCenter: parent.verticalCenter
        x: at * (width + 8)
        width: taskbar.slot
        height: 54
        radius: height / 2
        color: Theme.colors.surface_container_high
        opacity: taskbar.focused < 0 ? 0 : 1
        Behavior on x { SpringAnimation { spring: 4; damping: 0.4 } }
        Behavior on opacity { NumberAnimation { duration: 200 } }

        Binding on at {
            when: taskbar.focused >= 0
            value: taskbar.focused
            restoreMode: Binding.RestoreNone
        }
    }

    Row {
        anchors.fill: parent
        spacing: 8

        Repeater {
            model: taskbar.shown

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
            ink: Theme.colors.on_surface_variant
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
