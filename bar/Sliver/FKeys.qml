import QtQuick
import QtQuick.Layouts

RowLayout {
    spacing: Theme.gap

    Repeater {
        model: [59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 87, 88]

        Key {
            required property int modelData
            required property int index
            Layout.fillHeight: true
            keys: [modelData]
            text: `F${index + 1}`
        }
    }
}
