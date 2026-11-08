import QtQuick

Rectangle {
    id: level
    property string icon
    property real value
    property bool muted
    property real shown: value
    property real from
    readonly property bool held: drag.active
    signal moved(real value)
    signal tapped

    implicitWidth: 3 * Theme.slot
    implicitHeight: Theme.size
    radius: height / 2
    color: Theme.colors.surface_container_high
    onValueChanged: {
        if (!held)
            shown = value
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
            font: Qt.font({ family: Theme.icons, pixelSize: Theme.glyph.pixelSize, variableAxes: { "FILL": level.shown } })
            Behavior on color { ColorAnimation { duration: 150 } }
        }

        Item {
            width: parent.height
            height: parent.height

            TapHandler {
                onTapped: level.tapped()
            }
        }
    }

    Text {
        anchors.verticalCenter: parent.verticalCenter
        anchors.right: parent.right
        anchors.rightMargin: 20
        visible: level.held
        text: Math.round(100 * level.shown)
        color: fill.x + fill.width > x ? Theme.colors.on_primary : Theme.colors.on_surface_variant
        font: Theme.label
    }

    DragHandler {
        id: drag
        target: null
        yAxis.enabled: false
        dragThreshold: 4
        onActiveChanged: level.from = level.shown
        onActiveTranslationChanged: {
            if (!active)
                return
            level.shown = Math.max(0, Math.min(1, level.from + activeTranslation.x / (fill.reach - fill.height)))
            level.moved(level.shown)
        }
    }
}
