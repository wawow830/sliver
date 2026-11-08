import QtQuick

Rectangle {
    id: level
    property string icon
    property real value
    property bool muted
    property real from
    readonly property bool held: drag.active
    readonly property real shown: Math.max(0, Math.min(1, held ? from + drag.activeTranslation.x / (2 * width) : value))
    signal moved(real value)
    signal tapped

    implicitWidth: 2 * Theme.slot
    implicitHeight: Theme.size
    radius: height / 2
    color: Theme.colors.surface_container_high
    onShownChanged: {
        if (held)
            moved(shown)
    }

    Binding on from {
        when: !level.held
        value: level.value
        restoreMode: Binding.RestoreNone
    }

    Rectangle {
        id: fill
        readonly property real reach: level.width - 2 * Theme.inset
        x: Theme.inset
        y: Theme.inset
        height: level.height - 2 * Theme.inset
        width: height + level.shown * (reach - height)
        radius: height / 2
        color: level.muted ? Theme.colors.error_container : level.held ? Theme.colors.primary : Theme.colors.primary_container
        Behavior on width { enabled: !level.held; NumberAnimation { duration: 200; easing.type: Easing.OutCubic } }
        Behavior on color { ColorAnimation { duration: 150 } }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            x: (parent.height - width) / 2
            text: level.icon
            color: level.muted ? Theme.colors.on_error_container : level.held ? Theme.colors.on_primary : Theme.colors.on_primary_container
            font: Theme.glyph
            Behavior on color { ColorAnimation { duration: 150 } }
        }
    }

    DragHandler {
        id: drag
        target: null
        yAxis.enabled: false
    }

    TapHandler {
        onTapped: level.tapped()
    }
}
