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

    PointHandler {
        id: finger
        onActiveChanged: {
            level.from = level.value
            level.dragged = level.value
        }
        onPointChanged: {
            if (!active)
                return
            const v = Math.max(0, Math.min(1, level.from + (point.position.x - point.pressPosition.x) / track.width))
            if (v != level.dragged) {
                level.dragged = v
                level.moved(v)
            }
        }
    }

    Rectangle {
        id: panel
        visible: opacity > 0
        opacity: level.pressed ? 1 : 0
        x: level.opensLeft ? level.width - width : 0
        width: level.pressed ? 720 : level.width
        height: level.height
        radius: height / 2
        color: Theme.colors.surface_container_high
        Behavior on width { NumberAnimation { duration: 220; easing.type: Easing.OutCubic } }
        Behavior on opacity { NumberAnimation { duration: 160 } }

        Text {
            id: glyph
            anchors.verticalCenter: parent.verticalCenter
            x: level.opensLeft ? parent.width - width - 24 : 24
            text: level.icon
            color: Theme.colors.on_surface
            font.family: Theme.icons
            font.pixelSize: 32
        }

        Item {
            id: track
            anchors.top: parent.top
            anchors.bottom: parent.bottom
            x: level.opensLeft ? 22 : glyph.x + glyph.width + 16
            width: parent.width - glyph.width - 22 - 24 - 16

            Rectangle {
                anchors.verticalCenter: parent.verticalCenter
                width: parent.width
                height: 20
                radius: 10
                color: Qt.alpha(Theme.colors.on_surface, 0.16)
            }

            Rectangle {
                anchors.verticalCenter: parent.verticalCenter
                width: grip.x + grip.width / 2
                height: 20
                radius: 10
                color: Theme.colors.primary
            }

            Rectangle {
                id: grip
                anchors.verticalCenter: parent.verticalCenter
                x: level.shown * (track.width - width)
                width: 40
                height: 38
                radius: 19
                color: Theme.colors.on_surface
            }
        }
    }
}
