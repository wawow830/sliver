import QtQuick

Pill {
    property list<int> keys

    fill: Theme.colors.surface_container_high
    radius: pressed ? 8 : 14

    onPressedChanged: {
        const order = pressed ? Array.from(keys) : Array.from(keys).reverse()
        for (const key of order)
            Keyboard.key(key, pressed)
    }
}
