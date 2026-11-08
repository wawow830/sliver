import QtQuick
import QtQuick.Layouts

Pill {
    property list<int> keys
    property real grow: pressed ? 1.25 : 1

    fill: Theme.colors.surface_container_high
    Layout.fillWidth: true
    Layout.preferredWidth: implicitWidth * grow
    Behavior on grow { SpringAnimation { spring: 5; damping: 0.35 } }

    onPressedChanged: {
        const order = pressed ? Array.from(keys) : Array.from(keys).reverse()
        for (const key of order)
            Keyboard.key(key, pressed)
    }
}
