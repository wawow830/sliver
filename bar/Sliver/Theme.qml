pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io

Singleton {
    readonly property alias colors: colors
    readonly property string font: "Google Sans Flex"
    readonly property string icons: "Material Symbols Rounded"

    FileView {
        path: (Quickshell.env("XDG_CONFIG_HOME") || Quickshell.env("HOME") + "/.config") + "/sliver/colors.json"
        watchChanges: true
        onFileChanged: reload()

        JsonAdapter {
            id: colors
            property color primary: "#bfc1ff"
            property color on_primary: "#282b60"
            property color primary_container: "#3f4178"
            property color on_primary_container: "#e1e0ff"
            property color surface_container: "#1f1f25"
            property color surface_container_high: "#2a292f"
            property color surface_bright: "#39383f"
            property color on_surface: "#e4e1e9"
            property color on_surface_variant: "#c7c5d0"
            property color outline: "#918f9a"
            property color error: "#ffb4ab"
        }
    }
}
