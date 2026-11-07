import QtQuick
import Quickshell
import Sliver

FloatingWindow {
    color: "black"

    VirtualKeyboard { id: keyboard }

    Row {
        anchors.fill: parent
        anchors.margins: 4
        spacing: 4

        Repeater {
            // Labels and evdev keycodes.
            model: [
                ["esc", 1], ["dim", 224], ["bright", 225], ["mute", 113], ["vol −", 114],
                ["vol +", 115], ["prev", 165], ["play", 164], ["next", 163],
            ]

            Rectangle {
                required property var modelData
                width: (parent.width - 8 * parent.spacing) / 9
                height: parent.height
                radius: 6
                color: tap.pressed ? "#555" : "#2a2a2a"

                Text {
                    anchors.centerIn: parent
                    text: parent.modelData[0]
                    color: "white"
                    font.pixelSize: 20
                }

                TapHandler {
                    id: tap
                    onPressedChanged: keyboard.key(parent.modelData[1], pressed)
                }
            }
        }
    }
}
