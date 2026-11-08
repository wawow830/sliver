import QtQuick
import Quickshell.Services.Pipewire

Level {
    readonly property PwNode sink: Pipewire.defaultAudioSink

    muted: sink?.audio.muted ?? false
    icon: muted ? "volume_off" : value < 0.34 ? "volume_mute" : value < 0.67 ? "volume_down" : "volume_up"
    value: sink?.audio.volume ?? 0
    onMoved: v => {
        if (sink) {
            sink.audio.muted = false
            sink.audio.volume = v
        }
    }
    onTapped: {
        if (sink)
            sink.audio.muted = !muted
    }

    PwObjectTracker { objects: [sink] }
}
