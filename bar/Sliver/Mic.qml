import QtQuick
import Quickshell.Services.Pipewire

Pill {
    readonly property PwNode source: Pipewire.defaultAudioSource
    readonly property bool muted: source?.audio.muted ?? false

    icon: muted ? "mic_off" : "mic"
    fill: muted ? Theme.colors.error_container : Theme.colors.surface_container_high
    ink: muted ? Theme.colors.on_error_container : Theme.colors.on_surface
    onTapped: {
        if (source)
            source.audio.muted = !muted
    }

    PwObjectTracker { objects: [source] }
}
