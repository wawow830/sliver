import QtQuick
import QtQuick.Layouts

RowLayout {
    spacing: 2 * Theme.gap

    Repeater {
        model: [[59, 60, 61, 62], [63, 64, 65, 66], [67, 68, 87, 88]]

        RowLayout {
            id: group
            required property var modelData
            required property int index
            Layout.fillWidth: true
            spacing: Theme.gap

            Repeater {
                model: group.modelData

                Key {
                    required property int modelData
                    required property int index
                    Layout.fillHeight: true
                    keys: [modelData]
                    text: `F${group.index * 4 + index + 1}`
                }
            }
        }
    }
}
