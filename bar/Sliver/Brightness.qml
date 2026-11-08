import QtQuick
import Quickshell.Io

Level {
    id: brightness
    property string device: "apple-panel-bl"
    readonly property string sys: `/sys/class/backlight/${device}/`
    property int wanted: -1

    icon: "light_mode"
    value: Number(now.text()) / Number(max.text())
    onMoved: v => {
        wanted = Math.round(v * Number(max.text()))
        set.send()
    }

    FileView { id: max; path: brightness.sys + "max_brightness" }
    FileView { id: now; path: brightness.sys + "brightness" }

    Timer {
        interval: 1000
        repeat: true
        running: !set.running
        onTriggered: now.reload()
    }

    Process {
        id: set
        property int sent: -1
        function send() {
            if (running || brightness.wanted == sent)
                return
            sent = brightness.wanted
            command = ["brightnessctl", "-q", "-d", brightness.device, "set", String(sent)]
            running = true
        }
        onExited: {
            now.reload()
            send()
        }
    }
}
