import QtQuick

Item {
    id: seek
    property real progress
    property color color
    property color track
    readonly property int count: Math.max(2, Math.floor((width + 6) / 10))

    implicitHeight: 36

    Repeater {
        model: seek.count

        Rectangle {
            required property int index
            readonly property real level: Spectrum.levels[Math.floor(index * Spectrum.levels.length / seek.count)] ?? 0

            x: index * (seek.width - width) / (seek.count - 1)
            anchors.verticalCenter: parent.verticalCenter
            width: 4
            height: width + Math.min(1, level) * (seek.height - width)
            radius: width / 2
            color: x + width / 2 <= seek.progress * seek.width ? seek.color : seek.track
        }
    }
}
