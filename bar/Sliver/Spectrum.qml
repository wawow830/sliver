pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Services.Mpris

Singleton {
    readonly property int bands: 96
    property var levels: []

    Process {
        running: Mpris.players.values.some(p => p.isPlaying)
        command: ["bash", "-c", "exec cava -p <(printf '%s' \"$1\")", "cava", `
[general]
bars = ${bands}
framerate = 60
sleep_timer = 1
[input]
method = pulse
[output]
method = raw
raw_target = /dev/stdout
data_format = ascii
ascii_max_range = 1000
bar_delimiter = 59
frame_delimiter = 10
channels = mono
[smoothing]
noise_reduction = 0.6
`]
        stdout: SplitParser {
            onRead: line => levels = line.split(";").filter(s => s != "").map(n => Number(n) / 1000)
        }
        onRunningChanged: {
            if (!running)
                levels = []
        }
    }
}
