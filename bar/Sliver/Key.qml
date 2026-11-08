import QtQuick
import QtQuick.Layouts

Pill {
    property list<int> keys

    fill: Theme.colors.surface_container_high
    stretch: width / Theme.slot
    Layout.fillWidth: true
    Layout.preferredWidth: Theme.slot * (1 + 0.25 * press)

    onPressedChanged: {
        const order = pressed ? Array.from(keys) : Array.from(keys).reverse()
        for (const key of order)
            Keyboard.key(key, pressed)
    }
}
