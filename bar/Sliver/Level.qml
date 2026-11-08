import QtQuick

Pill {
    id: level
    property real value
    property bool opensLeft
    property bool open
    property real dragged
    readonly property real shown: Math.max(0, Math.min(1, finger.pressed ? dragged : value))
    signal moved(real value)

    z: panel.visible ? 1 : 0
    radius: height / 2
    scale: 1

    Connections {
        target: level
        function onTapped() { level.open = true }
    }

    Timer {
        running: level.open && !finger.pressed
        interval: 2500
        onTriggered: level.open = false
    }

    Rectangle {
        id: panel
        readonly property real outset: (Theme.size - level.height) / 2
        visible: opacity > 0
        opacity: level.open ? 1 : 0
        x: level.opensLeft ? level.width + outset - width : -outset
        y: -outset
        width: level.open ? 720 : level.width + 2 * outset
        height: Theme.size
        radius: height / 2
        color: Theme.colors.surface_container_high
        Behavior on width { NumberAnimation { duration: 220; easing.type: Easing.OutCubic } }
        Behavior on opacity { NumberAnimation { duration: 160 } }

        Rectangle {
            z: -1
            anchors.fill: parent
            anchors.margins: -3
            radius: height / 2
            color: "black"
        }

        Rectangle {
            id: bar
            readonly property real reach: panel.width - 2 * Theme.inset
            x: Theme.inset
            y: Theme.inset
            height: panel.height - 2 * Theme.inset
            width: height + level.shown * (reach - height)
            radius: height / 2
            color: level.fill.a > 0 ? level.fill : Theme.colors.primary

            Text {
                anchors.verticalCenter: parent.verticalCenter
                x: (parent.height - width) / 2
                text: level.icon
                color: level.fill.a > 0 ? level.ink : Theme.colors.on_primary
                font: Theme.glyph
            }
        }

        MouseArea {
            id: finger
            anchors.fill: parent
            enabled: level.open
            onPressed: mouse => {
                level.dragged = level.value
                slide(mouse.x)
            }
            onPositionChanged: mouse => slide(mouse.x)

            function slide(x) {
                const v = Math.max(0, Math.min(1, (x - bar.x - bar.height / 2) / (bar.reach - bar.height)))
                if (v != level.dragged) {
                    level.dragged = v
                    level.moved(v)
                }
            }
        }
    }
}
