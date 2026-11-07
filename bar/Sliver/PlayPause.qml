import QtQuick
import Quickshell.Services.Mpris

Pill {
    id: play
    property MprisPlayer player: Mpris.players.values.find(p => p.isPlaying) ?? Mpris.players.values[0] ?? null
    readonly property real beat: player?.isPlaying ? Spectrum.bass : 0

    visible: player != null
    implicitWidth: 136
    icon: player?.isPlaying ? "pause" : "play_arrow"
    fill: Theme.colors.primary
    ink: Theme.colors.on_primary
    onTapped: player.togglePlaying()

    Rectangle {
        z: -1
        anchors.centerIn: parent
        width: parent.width + 6 * play.beat
        height: parent.height + 6 * play.beat
        radius: height / 2
        color: "transparent"
        border.width: 3 * play.beat
        border.color: Qt.alpha(Theme.colors.primary, 0.6)
        visible: play.beat > 0.02
    }
}
