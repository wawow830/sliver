import QtQuick
import QtQuick.Layouts
import Quickshell.Services.Mpris

Rectangle {
    id: media
    readonly property MprisPlayer player: Mpris.players.values.find(p => p.isPlaying) ?? Mpris.players.values[0] ?? null
    readonly property real length: player?.lengthSupported ? player.length : 0
    readonly property real position: scrub.active ? scrub.at * length : player?.position ?? 0
    readonly property real progress: length > 0 ? Math.min(1, position / length) : 0

    readonly property color vivid: cover.vivid
    readonly property bool tinted: vivid.a > 0
    readonly property color ink: tinted ? Qt.hsla(vivid.hslHue, 0.3, 0.94, 1) : Theme.colors.on_surface
    readonly property color accent: tinted ? Qt.hsla(vivid.hslHue, Math.max(0.6, vivid.hslSaturation), 0.8, 1) : Theme.colors.primary
    readonly property color onAccent: tinted ? Qt.hsla(vivid.hslHue, 0.6, 0.15, 1) : Theme.colors.on_primary

    function time(seconds) {
        const s = Math.floor(seconds)
        return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`
    }

    visible: player != null
    radius: height / 2
    color: tinted ? Qt.hsla(vivid.hslHue, Math.max(0.35, Math.min(0.6, vivid.hslSaturation)), 0.2, 1) : Theme.colors.surface_container_high
    Behavior on color { ColorAnimation { duration: 400 } }

    RowLayout {
        anchors.fill: parent
        anchors.margins: Theme.inset
        spacing: 0

        Item {
            Layout.fillHeight: true
            Layout.preferredWidth: cover.drawn != "" ? cover.width : 0

            Cover {
                id: cover
                width: Theme.size - 2 * Theme.inset
                height: width
                source: media.player?.trackArtUrl ?? ""
                spinning: media.player?.isPlaying ?? false
            }
        }

        Column {
            Layout.leftMargin: cover.drawn != "" ? 12 : 18
            Layout.preferredWidth: Math.min(Math.max(title.implicitWidth, artist.implicitWidth), media.width * 0.25)

            Text {
                id: title
                width: parent.width
                text: media.player?.trackTitle || media.player?.identity || ""
                elide: Text.ElideRight
                color: media.ink
                font: Theme.title
            }

            Text {
                id: artist
                width: parent.width
                visible: text != ""
                text: media.player?.trackArtist ?? ""
                elide: Text.ElideRight
                color: Qt.alpha(media.ink, 0.7)
                font: Theme.meta
            }
        }

        Seek {
            id: seek
            Layout.fillWidth: true
            Layout.leftMargin: 20
            Layout.rightMargin: 16
            progress: media.progress
            color: media.accent
            track: Qt.alpha(media.ink, 0.25)

            PointHandler {
                id: scrub
                property real at
                target: null
                enabled: (media.player?.canSeek ?? false) && media.length > 0
                margin: 16
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

        Text {
            Layout.rightMargin: 8
            text: media.length > 0 ? `${media.time(media.position)} / ${media.time(media.length)}` : media.time(media.position)
            color: Qt.alpha(media.ink, 0.7)
            font: Theme.meta
        }

        Pill {
            Layout.fillHeight: true
            icon: "skip_previous"
            ink: media.ink
            enabled: media.player?.canGoPrevious ?? false
            onTapped: media.player.previous()
        }

        Pill {
            Layout.fillHeight: true
            icon: media.player?.isPlaying ? "pause" : "play_arrow"
            fill: media.accent
            ink: media.onAccent
            onTapped: media.player.togglePlaying()
        }

        Pill {
            Layout.fillHeight: true
            icon: "skip_next"
            ink: media.ink
            enabled: media.player?.canGoNext ?? false
            onTapped: media.player.next()
        }
    }

    Timer {
        running: media.player?.isPlaying ?? false
        interval: 1000
        repeat: true
        onTriggered: media.player.positionChanged()
    }
}
