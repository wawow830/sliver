import QtQuick
import QtQuick.Layouts
import Quickshell.Hyprland

Tray {
    Item {
        Layout.fillHeight: true
        implicitWidth: row.implicitWidth

        Rectangle {
            readonly property Item at: Array.from(row.children).find(c => c.current) ?? null

            x: at?.x ?? x
            width: at?.width ?? width
            height: parent.height
            radius: height / 2
            color: Theme.colors.primary_container
            opacity: at ? 1 : 0
            Behavior on x { SpringAnimation { spring: 4; damping: 0.4 } }
            Behavior on width { SpringAnimation { spring: 4; damping: 0.4 } }
            Behavior on opacity { NumberAnimation { duration: 200 } }
        }

        Row {
            id: row
            height: parent.height

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
}
