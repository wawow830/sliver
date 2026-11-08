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
    signal tapped
    signal longPressed

    implicitWidth: Math.max(64, row.implicitWidth + 36)
    implicitHeight: Theme.size
    radius: pressed ? Math.min(12, height / 2) : height / 2
    color: !pressed ? fill : fill.a > 0 ? Qt.tint(fill, Qt.alpha(ink, 0.16)) : Qt.alpha(ink, 0.16)
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
            implicitSize: 36
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: pill.icon != ""
            text: pill.icon
            color: pill.ink
            font: Theme.glyph
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: pill.text != ""
            text: pill.text
            color: pill.ink
            font: Theme.label
        }
    }

    TapHandler {
        id: tap
        onTapped: pill.tapped()
        onLongPressed: pill.longPressed()
    }
}
