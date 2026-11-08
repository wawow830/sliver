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
    property real press: pressed ? 1 : 0
    property real stretch: 1
    signal tapped

    implicitWidth: Math.max(Theme.slot, row.implicitWidth + 36)
    implicitHeight: Theme.size
    radius: Math.max(0, height / 2 - press * (height / 2 - Math.min(12, height / 2)))
    property color shade: fill
    color: shade.a > 0 ? Qt.tint(shade, Qt.alpha(ink, 0.16 * Math.max(0, press))) : Qt.alpha(ink, 0.16 * Math.max(0, press))
    opacity: enabled ? 1 : 0.4
    Behavior on press { SpringAnimation { spring: 5; damping: 0.35; epsilon: 0.01 } }
    Behavior on shade { ColorAnimation { duration: 150 } }

    Row {
        id: row
        anchors.centerIn: parent
        spacing: 10

        Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: pill.icon != ""
            text: pill.icon
            color: pill.ink
            font: Qt.font({ family: Theme.icons, pixelSize: Theme.glyph.pixelSize, variableAxes: { "FILL": 1, "wght": 400 + 300 * pill.press } })
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: pill.text != ""
            text: pill.text
            color: pill.ink
            font: Qt.font({ family: Theme.font, pixelSize: Theme.label.pixelSize, variableAxes: { "ROND": 100, "wght": 500 + 250 * pill.press, "wdth": Math.max(25, Math.min(151, 100 * pill.stretch)) } })
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
