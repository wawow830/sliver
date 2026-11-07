import QtQuick
import QtQuick.Layouts
import Quickshell.Services.Mpris

RowLayout {
    id: media
    readonly property MprisPlayer player: Mpris.players.values.find(p => p.isPlaying) ?? Mpris.players.values[0] ?? null

    function time(seconds) {
        const s = Math.floor(seconds)
        return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`
    }

    spacing: 8
    visible: player != null

    Pill {
        Layout.fillHeight: true
        implicitWidth: 104
        icon: "skip_previous"
        enabled: media.player?.canGoPrevious ?? false
        onTapped: media.player.previous()
    }

    PlayPause { Layout.fillHeight: true; player: media.player }

    Pill {
        Layout.fillHeight: true
        implicitWidth: 104
        icon: "skip_next"
        enabled: media.player?.canGoNext ?? false
        onTapped: media.player.next()
    }

    Rectangle {
        id: track
        readonly property real length: media.player?.lengthSupported ? media.player.length : 0
        readonly property real position: scrub.active ? scrub.at * length : media.player?.position ?? 0
        readonly property real progress: length > 0 ? Math.min(1, position / length) : 0

        Layout.fillWidth: true
        Layout.fillHeight: true
        radius: height / 2
        color: Theme.colors.surface_container_high

        Item {
            id: art
            x: 0
            width: cover.status == Image.Ready ? 54 : 0
            height: 54
            visible: width > 0

            Image {
                id: cover
                anchors.centerIn: parent
                width: 38
                height: 38
                source: media.player?.trackArtUrl ?? ""
                sourceSize: Qt.size(76, 76)
                fillMode: Image.PreserveAspectCrop
                smooth: true

                RotationAnimator on rotation {
                    running: media.player?.isPlaying ?? false
                    from: 0
                    to: 360
                    duration: 8000
                    loops: Animation.Infinite
                }
            }

            Rectangle {
                anchors.fill: parent
                radius: 27
                color: "transparent"
                border.width: 8
                border.color: track.color
            }
        }

        Column {
            id: label
            anchors.left: art.right
            anchors.leftMargin: art.visible ? 6 : 22
            anchors.verticalCenter: parent.verticalCenter
            width: Math.min(Math.max(title.implicitWidth, artist.implicitWidth), track.width * 0.35)
            spacing: -2

            Text {
                id: title
                width: parent.width
                text: media.player?.trackTitle || media.player?.identity || ""
                elide: Text.ElideRight
                color: Theme.colors.on_surface
                font.family: Theme.font
                font.pixelSize: 19
                font.weight: Font.DemiBold
            }

            Text {
                id: artist
                width: parent.width
                visible: text != ""
                text: media.player?.trackArtist ?? ""
                elide: Text.ElideRight
                color: Theme.colors.on_surface_variant
                font.family: Theme.font
                font.pixelSize: 16
            }
        }

        Row {
            id: bands
            anchors.left: label.right
            anchors.leftMargin: 18
            anchors.right: clock.left
            anchors.rightMargin: 14
            anchors.verticalCenter: parent.verticalCenter
            height: 40
            readonly property real pitch: width / Spectrum.bands

            Repeater {
                model: Spectrum.bands

                Item {
                    required property int index
                    width: bands.pitch
                    height: bands.height

                    Rectangle {
                        anchors.centerIn: parent
                        width: Math.max(3, bands.pitch * 0.55)
                        height: Math.max(width, bands.height * (Spectrum.levels[index] ?? 0))
                        radius: width / 2
                        color: (index + 0.5) / Spectrum.bands <= track.progress ? Theme.colors.primary : Qt.alpha(Theme.colors.on_surface, 0.28)
                    }
                }
            }
        }

        Text {
            id: clock
            anchors.right: parent.right
            anchors.rightMargin: 22
            anchors.verticalCenter: parent.verticalCenter
            text: track.length > 0 ? `${media.time(track.position)} / ${media.time(track.length)}` : media.time(track.position)
            color: Theme.colors.on_surface_variant
            font.family: Theme.font
            font.pixelSize: 18
            font.features: { "tnum": 1 }
        }

        PointHandler {
            id: scrub
            property real at
            enabled: (media.player?.canSeek ?? false) && track.length > 0
            onPointChanged: {
                if (active)
                    at = Math.max(0, Math.min(1, (point.position.x - bands.x) / bands.width))
            }
            onActiveChanged: {
                if (!active)
                    media.player.position = at * track.length
            }
        }
    }

    Timer {
        running: media.player?.isPlaying ?? false
        interval: 1000
        repeat: true
        onTriggered: media.player.positionChanged()
    }
}
