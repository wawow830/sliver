import QtQuick
import QtQuick.Layouts

Rectangle {
    default property alias content: row.data

    implicitWidth: row.implicitWidth + 2 * Theme.inset
    implicitHeight: Theme.size
    radius: height / 2
    color: Theme.colors.surface_container_high

    RowLayout {
        id: row
        anchors.fill: parent
        anchors.margins: Theme.inset
        spacing: 0
    }
}
