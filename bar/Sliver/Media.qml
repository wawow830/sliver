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
        implicitWidth: 120
        icon: "skip_previous"
        enabled: media.player?.canGoPrevious ?? false
        onTapped: media.player.previous()
    }

    PlayPause { player: media.player }

    Pill {
        implicitWidth: 120
        icon: "skip_next"
        enabled: media.player?.canGoNext ?? false
        onTapped: media.player.next()
    }

    Text {
        Layout.leftMargin: 8
        text: media.time(media.player?.position ?? 0)
        color: Theme.colors.on_surface_variant
        font.family: Theme.font
        font.pixelSize: 22
        font.features: { "tnum": 1 }
    }

    Level {
        Layout.fillWidth: true
        enabled: media.player?.canSeek ?? false
        value: media.player?.length ? media.player.position / media.player.length : 0
        onMoved: v => media.player.position = v * media.player.length
    }

    Text {
        Layout.rightMargin: 8
        text: media.time(media.player?.length ?? 0)
        color: Theme.colors.on_surface_variant
        font.family: Theme.font
        font.pixelSize: 22
        font.features: { "tnum": 1 }
    }

    Timer {
        running: media.player?.isPlaying ?? false
        interval: 1000
        repeat: true
        onTriggered: media.player.positionChanged()
    }
}
