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
    z: pressed ? 1 : 0

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
        visible: level.pressed
        x: level.opensLeft ? -width - 8 : level.width + 8
        width: 640
        height: level.height
        radius: height / 2
        color: Theme.colors.surface_container_high

        Item {
            id: track
            anchors.fill: parent
            anchors.leftMargin: 22
            anchors.rightMargin: 22

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
