import QtQuick
import Quickshell
import Quickshell.Widgets

Rectangle {
    id: pill
    property string text
    property string icon
    property list<string> images
    property color fill: "transparent"
    property color ink: Theme.colors.on_surface
    property bool pressed: tap.pressed
    signal tapped

    implicitWidth: Math.max(Theme.slot, row.implicitWidth + 36)
    implicitHeight: Theme.size
    radius: pressed ? Math.min(12, height / 2) : height / 2
    color: !pressed ? fill : fill.a > 0 ? Qt.tint(fill, Qt.alpha(ink, 0.16)) : Qt.alpha(ink, 0.16)
    opacity: enabled ? 1 : 0.4
    Behavior on radius { NumberAnimation { duration: 150 } }
    Behavior on color { ColorAnimation { duration: 150 } }

    Row {
        id: row
        anchors.centerIn: parent
        spacing: 10

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

        Row {
            anchors.verticalCenter: parent.verticalCenter
            visible: pill.images.length > 0
            spacing: 6

            Repeater {
                model: pill.images

                IconImage {
                    required property string modelData
                    source: modelData
                    implicitSize: 40
                }
            }
        }
    }

    TapHandler {
        id: tap
        onTapped: pill.tapped()
    }
}
