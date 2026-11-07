import QtQuick
import Quickshell.Services.Pipewire

Pill {
    readonly property PwNode source: Pipewire.defaultAudioSource
    readonly property bool muted: source?.audio.muted ?? false

    implicitWidth: 88
    icon: muted ? "mic_off" : "mic"
    fill: muted ? Theme.colors.error : Theme.colors.surface_container_high
    ink: muted ? Theme.colors.surface_container : Theme.colors.on_surface
    onTapped: {
        if (source)
            source.audio.muted = !muted
    }

    PwObjectTracker { objects: [source] }
}
