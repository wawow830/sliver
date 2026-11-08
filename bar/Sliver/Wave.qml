import QtQuick
import QtQuick.Shapes

Item {
    id: wave
    property real progress
    property bool playing
    property color color
    property color track
    property real phase
    readonly property real amplitude: playing ? 2 + 3 * Spectrum.bass : 0
    readonly property real at: progress * width

    implicitHeight: 24

    NumberAnimation on phase {
        running: wave.playing
        from: 0
        to: 2 * Math.PI
        duration: 1400
        loops: Animation.Infinite
    }

    Shape {
        anchors.fill: parent

        ShapePath {
            strokeColor: wave.color
            strokeWidth: 4
            fillColor: "transparent"
            capStyle: ShapePath.RoundCap
            joinStyle: ShapePath.RoundJoin

            PathPolyline {
                path: {
                    const points = []
                    for (let x = 2; x <= wave.at - 8; x += 2)
                        points.push(Qt.point(x, wave.height / 2 + wave.amplitude * Math.sin(x / 22 * 2 * Math.PI - wave.phase)))
                    return points
                }
            }
        }
    }

    Rectangle {
        x: wave.at - 2
        width: 4
        height: parent.height
        radius: 2
        color: wave.color
    }

    Rectangle {
        anchors.verticalCenter: parent.verticalCenter
        x: wave.at + 8
        width: Math.max(0, wave.width - x)
        height: 4
        radius: 2
        color: wave.track
    }
}
