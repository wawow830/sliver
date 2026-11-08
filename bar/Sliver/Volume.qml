import QtQuick
import Quickshell.Services.Pipewire

Level {
    readonly property PwNode sink: Pipewire.defaultAudioSink
    readonly property bool muted: sink?.audio.muted ?? false

    icon: muted ? "volume_off" : value < 0.34 ? "volume_mute" : value < 0.67 ? "volume_down" : "volume_up"
    fill: muted ? Theme.colors.error_container : "transparent"
    ink: muted ? Theme.colors.on_error_container : Theme.colors.on_surface
    value: sink?.audio.volume ?? 0
    onMoved: v => {
        if (sink) {
            sink.audio.muted = false
            sink.audio.volume = v
        }
    }
    onLongPressed: {
        if (sink)
            sink.audio.muted = !muted
    }

    PwObjectTracker { objects: [sink] }
}
