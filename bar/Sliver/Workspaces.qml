import QtQuick
import Quickshell.Hyprland

Rectangle {
    implicitWidth: row.width + 2 * Theme.inset
    implicitHeight: Theme.size
    radius: height / 2
    color: Theme.colors.surface_container_high

    Rectangle {
        id: marker
        readonly property Item at: Array.from(row.children).find(c => c.current) ?? null
        readonly property bool forward: (at?.x ?? head) > head
        property real head
        property real tail

        x: row.x + head
        y: row.y
        width: tail - head
        height: row.height
        radius: height / 2
        color: Theme.colors.primary_container
        opacity: at ? 1 : 0
        Behavior on head { SpringAnimation { spring: marker.forward ? 2.5 : 5; damping: 0.4; epsilon: 0.25 } }
        Behavior on tail { SpringAnimation { spring: marker.forward ? 5 : 2.5; damping: 0.4; epsilon: 0.25 } }
        Behavior on opacity { NumberAnimation { duration: 200 } }

        Binding on head {
            when: marker.at != null
            value: marker.at?.x ?? marker.head
            restoreMode: Binding.RestoreNone
        }

        Binding on tail {
            when: marker.at != null
            value: marker.at ? marker.at.x + marker.at.width : marker.tail
            restoreMode: Binding.RestoreNone
        }
    }

    Row {
        id: row
        x: Theme.inset
        y: Theme.inset
        height: parent.height - 2 * Theme.inset

        Repeater {
            model: Desktop.workspaces

            Pill {
                required property HyprlandWorkspace modelData
                readonly property bool current: modelData.focused

                height: row.height
                text: modelData.name
                images: Array.from(new Set(modelData.toplevels.values.map(Desktop.app).filter(a => a))).slice(0, 3).map(Desktop.icon)
                ink: current ? Theme.colors.on_primary_container : Theme.colors.on_surface_variant
                onTapped: Desktop.workspace(modelData.id)
            }
        }
    }
}
