import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Services.Mpris

FloatingWindow {
    id: bar
    property list<string> pinned: []
    property int slots: 5
    property list<string> terminals: ["kitty", "foot", "Alacritty", "org.wezfurlong.wezterm", "com.mitchellh.ghostty"]
    property list<string> browsers: ["zen", "firefox", "chromium", "google-chrome"]

    readonly property Component context: terminals.includes(Desktop.focused) ? fkeys
        : browsers.includes(Desktop.focused) ? browser
        : Mpris.players.values.length ? media
        : null

    color: "black"

    RowLayout {
        anchors.fill: parent
        anchors.topMargin: 3
        anchors.bottomMargin: 3
        anchors.leftMargin: 8
        anchors.rightMargin: 8
        spacing: 30

        Brightness { Layout.fillHeight: true }

        Taskbar { Layout.fillHeight: true; pinned: bar.pinned; slots: bar.slots }

        Loader {
            Layout.fillWidth: true
            Layout.fillHeight: true
            sourceComponent: bar.context
        }

        RowLayout {
            Layout.fillHeight: true
            spacing: 8
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
                Desktop.workspace(activeTranslation.x < 0 ? 1 : -1)
        }
    }

    Component { id: fkeys; FKeys {} }

    Component {
        id: browser
        RowLayout {
            spacing: 8
            Key { Layout.fillHeight: true; implicitWidth: 120; icon: "arrow_back"; keys: [158] }
            Key { Layout.fillHeight: true; implicitWidth: 120; icon: "arrow_forward"; keys: [159] }
            Key { Layout.fillHeight: true; implicitWidth: 120; icon: "refresh"; keys: [173] }
            Key { Layout.fillHeight: true; implicitWidth: 120; icon: "add"; keys: [29, 20] }
            Item { Layout.fillWidth: true }
            PlayPause { Layout.fillHeight: true }
        }
    }

    Component { id: media; Media {} }
}
