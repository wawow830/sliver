import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Services.Mpris

FloatingWindow {
    id: bar
    property list<string> terminals: ["kitty", "foot", "Alacritty", "org.wezfurlong.wezterm", "com.mitchellh.ghostty"]
    property list<string> browsers: ["zen", "firefox", "chromium", "google-chrome"]

    readonly property Component context: terminals.includes(Desktop.focused) ? fkeys
        : browsers.includes(Desktop.focused) ? browser
        : Mpris.players.values.length ? media
        : null

    color: "black"

    component Context: Loader {
        property Component component
        readonly property bool current: bar.context == component
        anchors.fill: parent
        sourceComponent: component
        enabled: current
        visible: opacity > 0
        opacity: current ? 1 : 0
        transform: Translate { x: current ? 0 : 24; Behavior on x { NumberAnimation { duration: 260; easing.type: Easing.OutCubic } } }
        Behavior on opacity { NumberAnimation { duration: 200 } }
    }

    RowLayout {
        anchors.fill: parent
        anchors.margins: (bar.height - Theme.size) / 2
        spacing: 2 * Theme.gap

        Workspaces { Layout.fillHeight: true }

        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true

            Context { component: fkeys }
            Context { component: browser }
            Context { component: media }
        }

        Tray {
            Layout.fillHeight: true
            Brightness { Layout.fillHeight: true; opensLeft: true }
            Mic { Layout.fillHeight: true }
            Volume { Layout.fillHeight: true; opensLeft: true }
        }
    }

    DragHandler {
        target: null
        minimumPointCount: 2
        maximumPointCount: 2
        yAxis.enabled: false
        onActiveChanged: {
            if (!active && Math.abs(activeTranslation.x) > 120)
                Desktop.workspace(activeTranslation.x < 0 ? "r+1" : "r-1")
        }
    }

    Component { id: fkeys; FKeys {} }

    Component {
        id: browser
        RowLayout {
            spacing: Theme.gap
            Key { Layout.fillHeight: true; icon: "arrow_back"; keys: [158] }
            Key { Layout.fillHeight: true; icon: "arrow_forward"; keys: [159] }
            Key { Layout.fillHeight: true; icon: "refresh"; keys: [173] }
            Key { Layout.fillHeight: true; icon: "add"; keys: [29, 20] }
            Media { Layout.fillWidth: true; Layout.fillHeight: true; Layout.leftMargin: Theme.gap }
        }
    }

    Component { id: media; Media {} }
}
