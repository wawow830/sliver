import QtQuick

Rectangle {
    id: level
    property real value
    property string icon
    property color fill: Theme.colors.surface_container_high
    readonly property bool pressed: finger.pressed
    property real dragged
    readonly property real shown: Math.max(0, Math.min(1, pressed ? dragged : value))
    signal moved(real value)

    implicitWidth: 300
    implicitHeight: 54
    radius: height / 2
    color: fill

    Text {
        id: glyph
        anchors.left: parent.left
        anchors.leftMargin: level.icon == "" ? 0 : 20
        anchors.verticalCenter: parent.verticalCenter
        width: level.icon == "" ? 0 : implicitWidth
        text: level.icon
        color: Theme.colors.on_surface
        font.family: Theme.icons
        font.pixelSize: 32
    }

    Item {
        id: track
        anchors.left: glyph.right
        anchors.leftMargin: level.icon == "" ? 22 : 14
        anchors.right: parent.right
        anchors.rightMargin: 22
        anchors.verticalCenter: parent.verticalCenter
        height: 38

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
            x: level.shown * (track.width - width)
            width: 40
            height: 38
            radius: 19
            color: Theme.colors.on_surface
        }
    }

    MouseArea {
        id: finger
        anchors.fill: parent
        preventStealing: true
        function follow(x) {
            level.dragged = Math.max(0, Math.min(1, (x - track.x - grip.width / 2) / (track.width - grip.width)))
            level.moved(level.dragged)
        }
        onPressed: mouse => follow(mouse.x)
        onPositionChanged: mouse => follow(mouse.x)
    }
}
