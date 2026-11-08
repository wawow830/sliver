import QtQuick

Item {
    id: seek
    property real progress
    property color color
    property color track
    readonly property real at: progress * width
    readonly property real line: height - 15
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

            x: index * (seek.width - width) / (seek.count - 1)
            y: seek.line - 4 - height
            width: 6
            height: Math.min(1, level) * (seek.line - 8)
            radius: 3
            color: Qt.alpha(seek.color, 0.5)
        }
    }

    Rectangle {
        y: seek.line
        width: Math.max(0, seek.at - 6)
        height: 4
        radius: 2
        color: seek.color
    }

    Rectangle {
        x: seek.at - 2
        y: seek.line - 6
        width: 4
        height: 16
        radius: 2
        color: seek.color
    }

    Rectangle {
        x: seek.at + 6
        y: seek.line
        width: Math.max(0, seek.width - x)
        height: 4
        radius: 2
        color: seek.track
    }
}
