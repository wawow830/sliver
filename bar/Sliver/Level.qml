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
            const v = Math.max(0, Math.min(1, level.from + (point.position.x - point.pressPosition.x) / (panel.width - panel.height)))
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

        Rectangle {
            z: -1
            anchors.fill: parent
            anchors.margins: -4
            radius: height / 2
            color: "black"
        }

        Rectangle {
            id: fill
            width: parent.height + level.shown * (parent.width - parent.height)
            height: parent.height
            radius: height / 2
            color: Theme.colors.primary

            Text {
                anchors.verticalCenter: parent.verticalCenter
                x: (parent.height - width) / 2
                text: level.icon
                color: Theme.colors.on_primary
                font.family: Theme.icons
                font.pixelSize: 30
                font.variableAxes: { "FILL": 1 }
            }

            Rectangle {
                anchors.verticalCenter: parent.verticalCenter
                anchors.right: parent.right
                anchors.rightMargin: 14
                visible: parent.width > 2 * parent.height
                width: 4
                height: 24
                radius: 2
                color: Qt.alpha(Theme.colors.on_primary, 0.5)
            }
        }
    }
}
