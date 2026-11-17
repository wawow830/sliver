// A Project SEKAI style rhythm game for the Touch Bar. ./run puts it over the
// bar until you close it (the × on the right).
import QtQuick
import QtQuick.Layouts
import QtMultimedia
import Quickshell
import Sliver
import "chart.js" as Chart

FloatingWindow {
    id: root
    title: "pjsk"
    color: "black"
    implicitWidth: 2008
    implicitHeight: 60

    // Milliseconds to move the notes later against the music, if they feel early.
    property real offset: 40
    // Seconds a note takes from the far end to the line.
    property real approach: 0.9

    property string mode: "menu"           // menu, play, result
    property string difficulty: "expert"
    property bool auto: false

    readonly property var diffs: ["easy", "normal", "hard", "expert", "master"]
    readonly property var diffColors: ({ easy: "#5ee077", normal: "#5bb8ff", hard: "#ffcc3d", expert: "#ff4f6d", master: "#c25cff" })
    readonly property var judgeColors: ({ PERFECT: "#ff9ef0", GREAT: "#ff73c4", GOOD: "#62c8ff", BAD: "#6fe08c", MISS: "#9a9aa8" })
    readonly property var weights: ({ PERFECT: 1, GREAT: 0.75, GOOD: 0.45, BAD: 0.15, MISS: 0 })
    readonly property var windows: [["PERFECT", 0.045], ["GREAT", 0.09], ["GOOD", 0.125], ["BAD", 0.16]]
    readonly property real badWindow: 0.16

    readonly property real hud: 232
    readonly property real fieldWidth: width - 2 * hud
    readonly property real laneWidth: fieldWidth / 12

    // game state, kept out of bindings and repainted every frame
    property var notes: []
    property int head: 0
    property var touches: ({})
    property var flicks: []
    property var effects: []
    property real wallStart: 0

    property int combo: 0
    property int maxCombo: 0
    property real points: 0
    property real maxPoints: 1
    property var counts: ({ PERFECT: 0, GREAT: 0, GOOD: 0, BAD: 0, MISS: 0 })
    property string lastJudge: ""
    property int judgeSerial: 0
    readonly property int score: Math.round(1000000 * points / maxPoints)
    readonly property bool fullCombo: counts.GOOD + counts.BAD + counts.MISS == 0
    readonly property bool allPerfect: fullCombo && counts.GREAT == 0

    function now() { return Date.now() / 1000 }
    function songTime() { return now() - wallStart - offset / 1000 }

    function start(d) {
        difficulty = d
        notes = Chart.charts[d].map(n => ({ t: n[0], end: n[0] + n[1], l: n[2], w: n[3], type: n[4], judged: false, tail: n[4] != 3, holder: -1, held: false }))
        maxPoints = notes.reduce((s, n) => s + (n.type == 1 ? 2 : 1) * (n.type == 3 ? 2 : 1), 0)
        head = 0
        touches = {}
        flicks = []
        effects = []
        combo = 0
        maxCombo = 0
        points = 0
        counts = { PERFECT: 0, GREAT: 0, GOOD: 0, BAD: 0, MISS: 0 }
        lastJudge = ""
        wallStart = now() + 0.25
        mode = "play"
        player.stop()
        player.position = 0
        player.play()
    }

    function finish() {
        player.stop()
        mode = "result"
        field.requestPaint()
    }

    function grade(dt) {
        for (const [name, w] of windows)
            if (Math.abs(dt) <= w) return name
        return "MISS"
    }

    function judge(n, j, tail) {
        const c = Object.assign({}, counts)
        c[j]++
        counts = c
        points += weights[j] * (n.type == 1 ? 2 : 1)
        if (j == "PERFECT" || j == "GREAT") {
            combo++
            maxCombo = Math.max(maxCombo, combo)
        } else {
            combo = 0
        }
        lastJudge = j
        judgeSerial++
        if (j != "MISS") {
            effects.push({ l: n.l, w: n.w, at: now(), type: n.type, tail: !!tail, judge: j })
            const sfx = n.type == 1 ? crit : n.type == 2 && !tail ? flickSound : taps[judgeSerial % taps.length]
            sfx.volume = j == "PERFECT" || j == "GREAT" ? 0.55 : 0.3
            if (!Quickshell.env("PJSK_MUTE")) sfx.play()
        }
    }

    function hit(id, x, y) {
        const t = songTime()
        const lane = x / laneWidth
        for (let i = head; i < notes.length; i++) {
            const n = notes[i]
            if (n.t - t > badWindow) break
            if (n.judged || lane < n.l - 0.35 || lane > n.l + n.w + 0.35) continue
            const j = grade(t - n.t)
            if (j == "MISS") continue
            n.judged = true
            if (n.type == 2) {
                flicks.push({ n: n, id: id, x: x, y: y, j: j, until: t + 0.3 })
            } else {
                judge(n, j)
                if (n.type == 3) {
                    n.holder = id
                    n.held = true
                }
            }
            return
        }
    }

    function move(id, x, y) {
        for (let k = flicks.length - 1; k >= 0; k--) {
            const f = flicks[k]
            if (f.id == id && Math.hypot(x - f.x, y - f.y) >= 14) {
                flicks.splice(k, 1)
                judge(f.n, f.j)
            }
        }
        const lane = x / laneWidth
        for (let i = head; i < notes.length; i++) {
            const n = notes[i]
            if (n.holder == id && (lane < n.l - 1 || lane > n.l + n.w + 1))
                release(id)
        }
    }

    function release(id) {
        const t = songTime()
        for (let k = flicks.length - 1; k >= 0; k--) {
            if (flicks[k].id == id) {
                const f = flicks.splice(k, 1)[0]
                judge(f.n, "MISS")
            }
        }
        for (let i = head; i < notes.length && notes[i].t <= t + 1; i++) {
            const n = notes[i]
            if (n.holder != id || n.tail) continue
            n.holder = -1
            n.held = false
            n.tail = true
            judge(n, t >= n.end - 0.12 ? "PERFECT" : t >= n.end - 0.25 ? "GOOD" : "MISS", true)
        }
    }

    function tick() {
        const t = songTime()
        for (let k = flicks.length - 1; k >= 0; k--) {
            if (t > flicks[k].until) {
                const f = flicks.splice(k, 1)[0]
                judge(f.n, "MISS")
            }
        }
        for (let i = head; i < notes.length; i++) {
            const n = notes[i]
            if (n.t > t + badWindow) break
            if (auto && !n.judged && t >= n.t) {
                n.judged = true
                judge(n, "PERFECT")
                if (n.type == 3) { n.holder = -2; n.held = true }
            }
            if (!n.judged && t - n.t > badWindow) {
                n.judged = true
                judge(n, "MISS")
            }
            if (!n.tail && t >= n.end) {
                if (n.held) {
                    n.tail = true
                    n.held = false
                    n.holder = -1
                    judge(n, "PERFECT", true)
                } else if (t - n.end > badWindow) {
                    n.tail = true
                    judge(n, "MISS", true)
                }
            }
        }
        while (head < notes.length && notes[head].judged && notes[head].tail)
            head++
        const wall = now()
        effects = effects.filter(e => wall - e.at < 0.35)
        if (t > Chart.meta.length + 0.5 || (head >= notes.length && t > notes[notes.length - 1].end + 1.5))
            finish()
        field.requestPaint()
    }

    FrameAnimation {
        running: root.mode == "play"
        onTriggered: root.tick()
    }

    MediaPlayer {
        id: player
        source: Qt.resolvedUrl("song.wav")
        audioOutput: AudioOutput { volume: Quickshell.env("PJSK_MUTE") ? 0 : 0.8 }
        onPlaybackStateChanged: {
            if (playbackState == MediaPlayer.PlayingState)
                root.wallStart = root.now() - position / 1000
        }
        onPositionChanged: at => {
            if (playbackState != MediaPlayer.PlayingState) return
            // the wall clock is smooth, the player is right: lean it towards the player
            const err = at / 1000 - (root.now() - root.wallStart)
            root.wallStart -= Math.abs(err) > 0.08 ? err : err * 0.1
        }
    }

    SoundEffect { id: tap0; source: Qt.resolvedUrl("tap.wav") }
    SoundEffect { id: tap1; source: Qt.resolvedUrl("tap.wav") }
    SoundEffect { id: tap2; source: Qt.resolvedUrl("tap.wav") }
    SoundEffect { id: tap3; source: Qt.resolvedUrl("tap.wav") }
    SoundEffect { id: crit; source: Qt.resolvedUrl("crit.wav") }
    SoundEffect { id: flickSound; source: Qt.resolvedUrl("flick.wav") }
    readonly property var taps: [tap0, tap1, tap2, tap3]

    Item {
        id: scene
        anchors.fill: parent

        // ---- the field ----------------------------------------------------------
        Canvas {
            id: field
            x: root.hud
            width: root.fieldWidth
            height: root.height
            renderStrategy: Canvas.Immediate
            onWidthChanged: requestPaint()

            // the lanes meet towards the far end, as on the phone
            readonly property real judgeY: height - 11
            function squeeze(y) { return 0.62 + 0.38 * y / judgeY }
            function px(x, y) { return width / 2 + (x - width / 2) * squeeze(y) }
            function depth(dt) {
                const p = Math.max(0, 1 - dt / root.approach)
                return (0.45 * p + 0.55 * p * p) * judgeY
            }

            function quad(ctx, x0, x1, ya, yb) {
                ctx.beginPath()
                ctx.moveTo(px(x0, ya), ya)
                ctx.lineTo(px(x1, ya), ya)
                ctx.lineTo(px(x1, yb), yb)
                ctx.lineTo(px(x0, yb), yb)
                ctx.closePath()
            }

            function noteColor(type) {
                return type == 1 ? "#ffd23f" : type == 2 ? "#ff5ca8" : type == 3 ? "#4fffb0" : "#4fd8ff"
            }

            onPaint: {
                const ctx = getContext("2d")
                const lw = root.laneWidth
                const H = height
                const jy = judgeY
                ctx.clearRect(0, 0, width, H)

                // road
                quad(ctx, 0, width, 0, H)
                const road = ctx.createLinearGradient(0, 0, 0, H)
                road.addColorStop(0, "#070712")
                road.addColorStop(1, "#141428")
                ctx.fillStyle = road
                ctx.fill()

                // touched lanes light up
                if (root.mode == "play") {
                    for (const id in root.touches) {
                        const l = Math.floor(root.touches[id].x / lw)
                        quad(ctx, l * lw, (l + 1) * lw, 0, H)
                        const g = ctx.createLinearGradient(0, 0, 0, H)
                        g.addColorStop(0, "rgba(120,200,255,0)")
                        g.addColorStop(1, "rgba(120,200,255,0.28)")
                        ctx.fillStyle = g
                        ctx.fill()
                    }
                }

                ctx.lineWidth = 1
                for (let i = 0; i <= 12; i++) {
                    ctx.strokeStyle = i == 0 || i == 12 ? "rgba(255,255,255,0.45)" : i % 2 ? "rgba(255,255,255,0.07)" : "rgba(255,255,255,0.14)"
                    ctx.beginPath()
                    ctx.moveTo(px(i * lw, 0), 0)
                    ctx.lineTo(px(i * lw, H), H)
                    ctx.stroke()
                }

                // the line
                ctx.fillStyle = "rgba(255,255,255,0.12)"
                ctx.fillRect(px(0, jy), jy - 3, px(width, jy) - px(0, jy), 6)
                ctx.fillStyle = "rgba(255,255,255,0.9)"
                ctx.fillRect(px(0, jy), jy - 1, px(width, jy) - px(0, jy), 2)

                if (root.mode != "play") return
                const t = root.songTime()

                // notes, far ones first so near ones draw on top
                const shown = []
                for (let i = root.head; i < root.notes.length; i++) {
                    const n = root.notes[i]
                    if (n.t - t > root.approach) break
                    shown.push(n)
                }
                for (let k = shown.length - 1; k >= 0; k--) {
                    const n = shown[k]
                    const x0 = n.l * lw + 4
                    const x1 = (n.l + n.w) * lw - 4
                    if (n.type == 3 && !n.tail) {
                        const yh = n.held ? jy : depth(n.t - t)
                        const yt = depth(n.end - t)
                        if (n.judged && !n.held && n.holder == -1 && t > n.t + root.badWindow && yt < yh) {
                            quad(ctx, x0 + 6, x1 - 6, yt, yh)
                            ctx.fillStyle = "rgba(120,140,140,0.25)"
                            ctx.fill()
                            continue
                        }
                        if (yt < yh) {
                            quad(ctx, x0 + 6, x1 - 6, yt, yh)
                            const g = ctx.createLinearGradient(0, yt, 0, yh)
                            g.addColorStop(0, n.held ? "rgba(79,255,176,0.35)" : "rgba(79,255,176,0.18)")
                            g.addColorStop(1, n.held ? "rgba(79,255,176,0.75)" : "rgba(79,255,176,0.4)")
                            ctx.fillStyle = g
                            ctx.fill()
                        }
                        if (n.end - t < root.approach) drawNote(ctx, x0, x1, yt, 3, 0.7)
                        if (!n.judged || n.held) drawNote(ctx, x0, x1, yh, 3, 1)
                        continue
                    }
                    if (n.judged) continue
                    drawNote(ctx, x0, x1, depth(n.t - t), n.type, 1)
                }

                // hits
                const wall = root.now()
                for (const e of root.effects) {
                    const a = (wall - e.at) / 0.35
                    const c = e.type == 1 ? "255,210,63" : e.type == 2 && !e.tail ? "255,92,168" : e.type == 3 ? "79,255,176" : "79,216,255"
                    const x0 = e.l * lw, x1 = (e.l + e.w) * lw
                    quad(ctx, x0, x1, Math.max(0, jy - 46 * (0.4 + a)), jy)
                    const g = ctx.createLinearGradient(0, jy - 46, 0, jy)
                    g.addColorStop(0, `rgba(${c},0)`)
                    g.addColorStop(1, `rgba(${c},${0.55 * (1 - a)})`)
                    ctx.fillStyle = g
                    ctx.fill()
                    const spread = 10 + 30 * a
                    ctx.fillStyle = `rgba(255,255,255,${0.9 * (1 - a)})`
                    ctx.fillRect(px(x0, jy) - spread, jy - 1.5, px(x1, jy) - px(x0, jy) + 2 * spread, 3)
                    if (e.judge == "PERFECT") {
                        ctx.fillStyle = `rgba(${c},${1 - a})`
                        for (let s = 0; s < 4; s++) {
                            const sx = px(x0 + (x1 - x0) * (s + 0.5) / 4, jy) + (s - 1.5) * 14 * a
                            ctx.fillRect(sx - 2, jy - 6 - 26 * a * (1 + (s % 2) * 0.6), 4, 4)
                        }
                    }
                }
            }

            function drawNote(ctx, x0, x1, y, type, alpha) {
                const s = squeeze(y)
                const h = 4 + 6 * y / judgeY
                const l = px(x0, y), r = px(x1, y)
                ctx.globalAlpha = alpha * Math.min(1, 0.25 + y / judgeY * 2)
                ctx.fillStyle = noteColor(type)
                ctx.beginPath()
                ctx.roundedRect(l, y - h / 2, r - l, h, h / 2, h / 2)
                ctx.fill()
                ctx.fillStyle = "rgba(255,255,255,0.85)"
                ctx.fillRect(l + h, y - h * 0.15, r - l - 2 * h, h * 0.3)
                if (type == 2) {
                    const cx = (l + r) / 2, a = 7 * s
                    ctx.fillStyle = "#ff5ca8"
                    ctx.beginPath()
                    ctx.moveTo(cx - 1.6 * a, y - h / 2 - 1)
                    ctx.lineTo(cx + 1.6 * a, y - h / 2 - 1)
                    ctx.lineTo(cx, y - h / 2 - 1 - 1.4 * a)
                    ctx.closePath()
                    ctx.fill()
                }
                ctx.globalAlpha = 1
            }

            MultiPointTouchArea {
                anchors.fill: parent
                enabled: root.mode == "play" && !root.auto
                maximumTouchPoints: 10
                mouseEnabled: true
                onPressed: points => {
                    for (const p of points) {
                        root.touches[p.pointId] = { x: p.x, y: p.y }
                        root.hit(p.pointId, p.x, p.y)
                    }
                }
                onUpdated: points => {
                    for (const p of points) {
                        root.touches[p.pointId] = { x: p.x, y: p.y }
                        root.move(p.pointId, p.x, p.y)
                    }
                }
                onReleased: points => {
                    for (const p of points) {
                        root.move(p.pointId, p.x, p.y)
                        delete root.touches[p.pointId]
                        root.release(p.pointId)
                    }
                }
                onCanceled: points => {
                    for (const p of points) {
                        delete root.touches[p.pointId]
                        root.release(p.pointId)
                    }
                }
            }

            // the judgement, up where the notes are still far
            Text {
                id: judgeText
                anchors.horizontalCenter: parent.horizontalCenter
                y: 4
                visible: root.mode == "play" && root.lastJudge != ""
                text: root.lastJudge
                color: root.judgeColors[root.lastJudge] ?? "white"
                font: Qt.font({ family: Theme.font, pixelSize: 17, weight: Font.Black, letterSpacing: 3, variableAxes: { "ROND": 100, "wdth": 120 } })
                style: Text.Outline
                styleColor: "#40000000"
                scale: 1
                opacity: 0

                Connections {
                    target: root
                    function onJudgeSerialChanged() { pop.restart() }
                }
                SequentialAnimation {
                    id: pop
                    ParallelAnimation {
                        NumberAnimation { target: judgeText; property: "scale"; from: 1.35; to: 1; duration: 120; easing.type: Easing.OutBack }
                        NumberAnimation { target: judgeText; property: "opacity"; to: 1; duration: 40 }
                    }
                    PauseAnimation { duration: 300 }
                    NumberAnimation { target: judgeText; property: "opacity"; to: 0; duration: 200 }
                }
            }

            // menu: pick a difficulty
            RowLayout {
                anchors.fill: parent
                anchors.margins: 1
                spacing: Theme.gap
                visible: root.mode == "menu"

                Repeater {
                    model: root.diffs
                    Pill {
                        required property string modelData
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        fill: root.diffColors[modelData]
                        ink: "#101018"
                        text: `${modelData.toUpperCase()}  ${Chart.meta.levels[modelData]}`
                        onTapped: root.start(modelData)
                    }
                }
            }

            // results
            Rectangle {
                anchors.fill: parent
                visible: root.mode == "result"
                color: "#d0080810"

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 24
                    anchors.rightMargin: 4
                    spacing: 28

                    Text {
                        text: root.auto ? "AUTO PLAY" : root.allPerfect ? "ALL PERFECT" : root.fullCombo ? "FULL COMBO" : "CLEAR"
                        color: root.allPerfect ? "#ff9ef0" : root.fullCombo ? "#ffd23f" : "white"
                        font: Qt.font({ family: Theme.font, pixelSize: 26, weight: Font.Black, letterSpacing: 2, variableAxes: { "ROND": 100, "wdth": 125 } })
                    }
                    Text {
                        text: String(root.score).padStart(7, "0")
                        color: "white"
                        font: Qt.font({ family: Theme.font, pixelSize: 30, weight: Font.Bold, features: { "tnum": 1 }, variableAxes: { "ROND": 100 } })
                    }
                    Repeater {
                        model: ["PERFECT", "GREAT", "GOOD", "BAD", "MISS"]
                        Column {
                            required property string modelData
                            Text { text: modelData; color: root.judgeColors[modelData]; font: Qt.font({ family: Theme.font, pixelSize: 13, weight: Font.Black, letterSpacing: 1, variableAxes: { "ROND": 100 } }) }
                            Text { text: root.counts[modelData]; color: "white"; font: Theme.label }
                        }
                    }
                    Column {
                        Text { text: "MAX COMBO"; color: Theme.colors.on_surface_variant; font: Qt.font({ family: Theme.font, pixelSize: 13, weight: Font.Black, letterSpacing: 1, variableAxes: { "ROND": 100 } }) }
                        Text { text: root.maxCombo; color: "white"; font: Theme.label }
                    }
                    Item { Layout.fillWidth: true }
                    Pill { Layout.fillHeight: true; Layout.topMargin: -2; Layout.bottomMargin: -2; icon: "replay"; text: "Retry"; fill: root.diffColors[root.difficulty]; ink: "#101018"; onTapped: root.start(root.difficulty) }
                    Pill { Layout.fillHeight: true; Layout.topMargin: -2; Layout.bottomMargin: -2; icon: "menu"; text: "Songs"; fill: Theme.colors.surface_container_high; onTapped: root.mode = "menu" }
                }
            }
        }

        // ---- left: song, then the combo ------------------------------------------
        Item {
            width: root.hud
            height: root.height

            Column {
                anchors.verticalCenter: parent.verticalCenter
                x: 16
                visible: root.mode != "play"
                spacing: 0
                Text { text: Chart.meta.title; color: "white"; font: Qt.font({ family: Theme.font, pixelSize: 21, weight: Font.Bold, variableAxes: { "ROND": 100 } }) }
                Text {
                    text: `${Chart.meta.bpm} BPM · ${Math.floor(Chart.meta.length / 60)}:${String(Math.round(Chart.meta.length % 60)).padStart(2, "0")}`
                    color: Theme.colors.on_surface_variant
                    font: Theme.meta
                }
            }

            Column {
                anchors.centerIn: parent
                visible: root.mode == "play" && root.combo > 1
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: root.combo
                    color: root.allPerfect ? "#ff9ef0" : root.fullCombo ? "#ffd23f" : "white"
                    font: Qt.font({ family: Theme.font, pixelSize: 32, weight: Font.Black, features: { "tnum": 1 }, variableAxes: { "ROND": 100, "wdth": 115 } })
                }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    topPadding: -6
                    text: root.allPerfect ? "AP COMBO" : "COMBO"
                    color: Theme.colors.on_surface_variant
                    font: Qt.font({ family: Theme.font, pixelSize: 11, weight: Font.Black, letterSpacing: 3, variableAxes: { "ROND": 100 } })
                }
            }
        }

        // ---- right: score and the way out ------------------------------------------
        RowLayout {
            x: root.width - root.hud + Theme.gap
            width: root.hud - Theme.gap - 1
            height: root.height
            spacing: Theme.gap

            Column {
                Layout.fillWidth: true
                Layout.alignment: Qt.AlignVCenter
                visible: root.mode == "play"
                Text {
                    text: `${root.auto ? "AUTO · " : ""}${root.difficulty.toUpperCase()} ${Chart.meta.levels[root.difficulty]}`
                    color: root.diffColors[root.difficulty]
                    font: Qt.font({ family: Theme.font, pixelSize: 12, weight: Font.Black, letterSpacing: 2, variableAxes: { "ROND": 100 } })
                }
                Text {
                    text: String(root.score).padStart(7, "0")
                    color: "white"
                    font: Qt.font({ family: Theme.font, pixelSize: 24, weight: Font.Bold, features: { "tnum": 1 }, variableAxes: { "ROND": 100 } })
                }
            }

            Pill {
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.margins: 1
                visible: root.mode == "menu"
                icon: "smart_toy"
                text: "Auto"
                fill: root.auto ? Theme.colors.primary : Theme.colors.surface_container_high
                ink: root.auto ? Theme.colors.on_primary : Theme.colors.on_surface
                onTapped: root.auto = !root.auto
            }

            Pill {
                Layout.fillHeight: true
                Layout.margins: 1
                Layout.preferredWidth: Theme.size + 10
                icon: root.mode == "play" ? "stop" : "close"
                fill: Theme.colors.surface_container_high
                onTapped: {
                    if (root.mode == "play") {
                        player.stop()
                        root.mode = "menu"
                        field.requestPaint()
                    } else {
                        Qt.quit()
                    }
                }
            }
        }
    }

    // for checking it without looking: PJSK_SHOT=/tmp/pjsk saves a frame every second
    Timer {
        property int n: 0
        running: !!Quickshell.env("PJSK_SHOT")
        interval: 1000
        repeat: true
        onTriggered: scene.grabToImage(r => r.saveToFile(`${Quickshell.env("PJSK_SHOT")}-${n++}.png`))
    }

    Component.onCompleted: {
        const d = Quickshell.env("PJSK_START")
        if (d) {
            auto = true
            start(d)
        }
    }
}
