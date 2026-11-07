import QtQuick

Pill {
    property list<int> keys

    onPressedChanged: {
        const order = pressed ? Array.from(keys) : Array.from(keys).reverse()
        for (const key of order)
            Keyboard.key(key, pressed)
    }
}
