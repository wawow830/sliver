import QtQuick
import QtQuick.Layouts
import Quickshell.Services.Mpris

RowLayout {
    id: media
    readonly property MprisPlayer player: Mpris.players.values.find(p => p.isPlaying) ?? Mpris.players.values[0] ?? null

    function time(seconds) {
        const s = Math.floor(seconds)
        return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`
    }

    spacing: 8
    visible: player != null

    Pill {
        Layout.fillHeight: true
        implicitWidth: 120
        icon: "skip_previous"
        enabled: media.player?.canGoPrevious ?? false
        onTapped: media.player.previous()
    }

    PlayPause { Layout.fillHeight: true; player: media.player }

    Pill {
        Layout.fillHeight: true
        implicitWidth: 120
        icon: "skip_next"
        enabled: media.player?.canGoNext ?? false
        onTapped: media.player.next()
    }

    Rectangle {
        id: track
        readonly property real length: media.player?.lengthSupported ? media.player.length : 0
        readonly property real position: scrub.active ? scrub.at * length : media.player?.position ?? 0

        Layout.fillWidth: true
        Layout.fillHeight: true
        radius: height / 2
        color: Theme.colors.surface_container_high

        Rectangle {
            visible: track.length > 0
            width: Math.max(height, Math.min(1, track.position / track.length) * parent.width)
            height: parent.height
            radius: height / 2
            color: Theme.colors.primary_container
        }

        Text {
            anchors.left: parent.left
            anchors.leftMargin: 24
            anchors.right: clock.left
            anchors.rightMargin: 16
            anchors.verticalCenter: parent.verticalCenter
            text: [media.player?.trackTitle, media.player?.trackArtist].filter(t => t).join(" · ") || (media.player?.identity ?? "")
            elide: Text.ElideRight
            color: Theme.colors.on_surface
            font.family: Theme.font
            font.pixelSize: 22
            font.weight: Font.Medium
        }

        Text {
            id: clock
            anchors.right: parent.right
            anchors.rightMargin: 24
            anchors.verticalCenter: parent.verticalCenter
            text: track.length > 0 ? `${media.time(track.position)} / ${media.time(track.length)}` : media.time(track.position)
            color: Theme.colors.on_surface_variant
            font.family: Theme.font
            font.pixelSize: 22
            font.features: { "tnum": 1 }
        }

        PointHandler {
            id: scrub
            property real at
            enabled: (media.player?.canSeek ?? false) && track.length > 0
            onPointChanged: {
                if (active)
                    at = Math.max(0, Math.min(1, point.position.x / track.width))
            }
            onActiveChanged: {
                if (!active)
                    media.player.position = at * track.length
            }
        }
    }

    Timer {
        running: media.player?.isPlaying ?? false
        interval: 1000
        repeat: true
        onTriggered: media.player.positionChanged()
    }
}
