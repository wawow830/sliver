import QtQuick
import Quickshell
import Quickshell.Widgets

Rectangle {
    id: pill
    property string text
    property string icon
    property string image
    property color fill: "transparent"
    property color ink: Theme.colors.on_surface
    property bool pressed: tap.pressed
    readonly property real content: row.implicitWidth
    signal tapped
    signal longPressed

    implicitWidth: Math.max(96, row.implicitWidth + 36)
    implicitHeight: 54
    radius: pressed ? 12 : height / 2
    color: pressed ? Qt.tint(fill.a > 0 ? fill : Theme.colors.surface_container_high, Qt.alpha(ink, 0.2)) : fill
    opacity: enabled ? 1 : 0.4
    scale: pressed ? 0.95 : 1
    Behavior on scale { SpringAnimation { spring: 6; damping: 0.35 } }
    Behavior on radius { NumberAnimation { duration: 150 } }
    Behavior on color { ColorAnimation { duration: 150 } }

    Row {
        id: row
        anchors.centerIn: parent
        spacing: 10

        IconImage {
            anchors.verticalCenter: parent.verticalCenter
            visible: pill.image != ""
            source: pill.image
            implicitSize: 40
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: pill.icon != ""
            text: pill.icon
            color: pill.ink
            font.family: Theme.icons
            font.pixelSize: 32
            font.variableAxes: { "FILL": 1 }
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: pill.text != ""
            text: pill.text
            color: pill.ink
            font.family: Theme.font
            font.pixelSize: 24
            font.weight: Font.Medium
        }
    }

    TapHandler {
        id: tap
        onTapped: pill.tapped()
        onLongPressed: pill.longPressed()
    }
}
