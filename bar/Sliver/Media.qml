import QtQuick
import QtQuick.Layouts
import Quickshell.Services.Mpris

Rectangle {
    id: media
    readonly property MprisPlayer player: Mpris.players.values.find(p => p.isPlaying) ?? Mpris.players.values[0] ?? null
    readonly property bool playing: player?.isPlaying ?? false
    readonly property real length: player?.lengthSupported ? player.length : 0
    readonly property real position: scrub.active ? scrub.at * length : player?.position ?? 0

    readonly property color vivid: cover.vivid
    readonly property bool tinted: vivid.a > 0
    readonly property color ink: tinted ? Qt.hsla(vivid.hslHue, 0.3, 0.94, 1) : Theme.colors.on_surface
    readonly property color accent: tinted ? Qt.hsla(vivid.hslHue, Math.max(0.6, vivid.hslSaturation), 0.8, 1) : Theme.colors.primary
    readonly property color onAccent: tinted ? Qt.hsla(vivid.hslHue, 0.6, 0.15, 1) : Theme.colors.on_primary

    visible: player != null
    radius: height / 2
    color: tinted ? Qt.hsla(vivid.hslHue, Math.max(0.35, Math.min(0.6, vivid.hslSaturation)), 0.2, 1) : Theme.colors.surface_container_high
    Behavior on color { ColorAnimation { duration: 400 } }

    RowLayout {
        anchors.fill: parent
        anchors.margins: Theme.inset
        spacing: 16

        Pill {
            readonly property bool art: cover.drawn != ""

            Layout.fillHeight: true
            implicitWidth: height
            icon: art && media.playing ? "" : media.playing ? "pause" : "play_arrow"
            fill: !art ? media.accent : media.playing ? "transparent" : Qt.alpha("black", 0.45)
            ink: art ? "white" : media.onAccent
            onTapped: media.player.togglePlaying()

            Cover {
                id: cover
                z: -1
                anchors.centerIn: parent
                width: Theme.size - 2 * Theme.inset
                height: width
                corner: parent.radius
                source: media.player?.trackArtUrl ?? ""
                spinning: media.playing
            }
        }

        Seek {
            id: seek
            Layout.fillWidth: true
            Layout.fillHeight: true
            progress: media.length > 0 ? Math.min(1, media.position / media.length) : 0
            color: media.accent
            track: Qt.alpha(media.ink, 0.25)

            PointHandler {
                id: scrub
                property real at
                target: null
                enabled: (media.player?.canSeek ?? false) && media.length > 0
                onPointChanged: {
                    if (active)
                        at = Math.max(0, Math.min(1, point.position.x / seek.width))
                }
                onActiveChanged: {
                    if (!active)
                        media.player.position = at * media.length
                }
            }
        }

        Pill {
            Layout.fillHeight: true
            implicitWidth: height
            icon: "skip_next"
            ink: media.ink
            enabled: media.player?.canGoNext ?? false
            onTapped: media.player.next()
        }
    }

    Timer {
        running: media.playing
        interval: 1000
        repeat: true
        onTriggered: media.player.positionChanged()
    }
}
