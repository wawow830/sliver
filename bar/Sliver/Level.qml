import QtQuick

Pill {
    id: level
    property real value
    property bool opensLeft
    property real from
    property real dragged
    readonly property real shown: Math.max(0, Math.min(1, pressed ? dragged : value))
    signal moved(real value)

    pressed: finger.active
    z: pressed || panel.visible ? 1 : 0
    radius: height / 2
    scale: 1

    PointHandler {
        id: finger
        onActiveChanged: {
            level.from = level.value
            level.dragged = level.value
        }
        onPointChanged: {
            if (!active)
                return
            const v = Math.max(0, Math.min(1, level.from + (point.position.x - point.pressPosition.x) / (fill.reach - fill.height)))
            if (v != level.dragged) {
                level.dragged = v
                level.moved(v)
            }
        }
    }

    Rectangle {
        id: panel
        readonly property real outset: (Theme.size - level.height) / 2
        visible: opacity > 0
        opacity: level.pressed ? 1 : 0
        x: level.opensLeft ? level.width + outset - width : -outset
        y: -outset
        width: level.pressed ? 720 : level.width + 2 * outset
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
            id: fill
            readonly property real reach: panel.width - 2 * Theme.inset
            x: Theme.inset
            y: Theme.inset
            height: panel.height - 2 * Theme.inset
            width: height + level.shown * (reach - height)
            radius: height / 2
            color: Theme.colors.primary

            Text {
                anchors.verticalCenter: parent.verticalCenter
                x: (parent.height - width) / 2
                text: level.icon
                color: Theme.colors.on_primary
                font: Theme.glyph
            }
        }
    }
}
