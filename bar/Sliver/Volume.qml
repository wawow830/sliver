import QtQuick
import Quickshell.Services.Pipewire

Level {
    readonly property PwNode sink: Pipewire.defaultAudioSink

    icon: sink?.audio.muted ? "volume_off" : "volume_up"
    value: sink?.audio.volume ?? 0
    onMoved: v => {
        if (sink)
            sink.audio.volume = v
    }

    PwObjectTracker { objects: [sink] }
}
