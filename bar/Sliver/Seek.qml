import QtQuick

Item {
    id: seek
    property real progress
    property color color
    property color track
    readonly property real at: progress * width
    readonly property int count: Math.max(2, Math.floor((width + 4) / 10))

    Repeater {
        model: seek.count

        Rectangle {
            required property int index
            readonly property real level: {
                const levels = Spectrum.levels
                const at = index / (seek.count - 1) * (levels.length - 1)
                const i = Math.floor(at)
                return levels.length ? levels[i] + (at - i) * ((levels[i + 1] ?? levels[i]) - levels[i]) : 0
            }

            visible: level > 0
            x: index * (seek.width - width) / (seek.count - 1)
            anchors.verticalCenter: parent.verticalCenter
            width: 6
            height: width + Math.min(1, level) * (seek.height - width)
            radius: width / 2
            color: Qt.alpha(seek.color, 0.45)
        }
    }

    Rectangle {
        anchors.verticalCenter: parent.verticalCenter
        width: Math.max(0, seek.at - 6)
        height: 4
        radius: 2
        color: seek.color
    }

    Rectangle {
        anchors.verticalCenter: parent.verticalCenter
        x: seek.at - 2
        width: 4
        height: 20
        radius: 2
        color: seek.color
    }

    Rectangle {
        anchors.verticalCenter: parent.verticalCenter
        x: seek.at + 6
        width: Math.max(0, seek.width - x)
        height: 4
        radius: 2
        color: seek.track
    }
}
