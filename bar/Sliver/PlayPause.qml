import QtQuick
import Quickshell.Services.Mpris

Pill {
    property MprisPlayer player: Mpris.players.values.find(p => p.isPlaying) ?? Mpris.players.values[0] ?? null

    visible: player != null
    implicitWidth: 136
    icon: player?.isPlaying ? "pause" : "play_arrow"
    fill: Theme.colors.primary
    ink: Theme.colors.on_primary
    onTapped: player.togglePlaying()
}
