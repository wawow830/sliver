import QtQuick

Canvas {
    id: cover
    property string source
    property bool spinning
    property string drawn
    property color vivid: "transparent"

    function load() {
        if (available && source != "")
            loadImage(source, Qt.size(width, height))
    }

    onSourceChanged: {
        if (drawn != "")
            unloadImage(drawn)
        drawn = ""
        vivid = "transparent"
        load()
        requestPaint()
    }
    onAvailableChanged: load()
    onImageLoaded: requestPaint()
    onPaint: {
        const context = getContext("2d")
        context.reset()
        if (!isImageLoaded(source))
            return
        context.fillStyle = context.createPattern(source, "no-repeat")
        context.beginPath()
        context.arc(width / 2, height / 2, width / 2, 0, 2 * Math.PI)
        context.fill()
        drawn = source

        const data = context.getImageData(0, 0, width, height).data
        const cell = Math.ceil(width / 6)
        let best = 0
        for (let cy = 0; cy < height; cy += cell)
            for (let cx = 0; cx < width; cx += cell) {
                const sum = [0, 0, 0]
                let n = 0
                for (let y = cy; y < Math.min(height, cy + cell); y++)
                    for (let x = cx; x < Math.min(width, cx + cell); x++) {
                        const i = (y * width + x) * 4
                        if (data[i + 3] < 255)
                            continue
                        for (let c = 0; c < 3; c++)
                            sum[c] += data[i + c]
                        n++
                    }
                if (!n)
                    continue
                const color = Qt.rgba(sum[0] / n / 255, sum[1] / n / 255, sum[2] / n / 255, 1)
                const chroma = color.hsvSaturation * color.hsvValue
                if (chroma > best) {
                    best = chroma
                    vivid = color
                }
            }
        if (best < 0.12)
            vivid = "transparent"
    }

    RotationAnimator on rotation {
        running: cover.spinning
        from: 0
        to: 360
        duration: 8000
        loops: Animation.Infinite
    }
}
