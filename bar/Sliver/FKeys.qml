import QtQuick
import QtQuick.Layouts

RowLayout {
    spacing: 14

    Repeater {
        model: [[59, 60, 61, 62], [63, 64, 65, 66], [67, 68, 87, 88]]

        RowLayout {
            id: group
            required property var modelData
            required property int index
            Layout.fillWidth: true
            spacing: 4

            Repeater {
                model: group.modelData

                Key {
                    required property int modelData
                    required property int index
                    Layout.fillWidth: true
                    Layout.preferredWidth: 1
                    radius: pressed ? 8 : 14
                    keys: [modelData]
                    text: `F${group.index * 4 + index + 1}`
                }
            }
        }
    }
}
