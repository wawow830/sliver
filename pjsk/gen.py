#!/usr/bin/env python3
"""Makes the song, its charts and the hit sounds for the Touch Bar PJSK toy.

Everything is generated from one seed, so the charts follow the music: the lead
line is what you tap, its pitch is where the note lands, long notes are holds
and phrase ends are flicks.
"""
import json, math, os, random, struct, wave

HERE = os.path.dirname(os.path.abspath(__file__))
SR = 24000
BPM = 145
BEAT = 60 / BPM
BAR = 4 * BEAT
S16 = BEAT / 4
rng = random.Random(39)

C, D, E, F, G, A, B = 0, 2, 4, 5, 7, 9, 11
CHORDS = {  # root (midi, bass octave) and tones (pitch classes)
    "F": (41, [F, A, C]), "G": (43, [G, B, D]), "Em": (40, [E, G, B]),
    "Am": (45, [A, C, E]), "C": (48, [C, E, G]), "Dm": (38, [D, F, A]),
}
SCALE = [C, D, E, F, G, A, B]

# (section, chords one per bar)
FORM = (
    [("intro", c) for c in ["Am", "Am"]]
    + [("verse", c) for c in ["Am", "F", "C", "G"] * 2]
    + [("pre", c) for c in ["F", "G", "Em", "Am", "Dm", "Em", "F", "G"]]
    + [("chorus", c) for c in ["F", "G", "Em", "Am"] * 2]
    + [("verse", c) for c in ["Am", "F", "C", "G"]]
    + [("chorus", c) for c in ["F", "G", "Em", "Am"] * 2]
    + [("outro", c) for c in ["F", "Am"]]
)
LENGTH = len(FORM) * BAR + 1.5

VERSE = [
    [(0, 4), (4, 2), (6, 2), (8, 6), (14, 2)],
    [(0, 2), (2, 2), (4, 4), (10, 2), (12, 4)],
    [(0, 6), (6, 2), (8, 4), (12, 4)],
    [(2, 2), (4, 2), (6, 4), (10, 2), (12, 4)],
]
VERSE_END = [(0, 4), (4, 4), (8, 8)]
PRE = [
    [(0, 2), (2, 2), (4, 2), (6, 2), (8, 4), (12, 4)],
    [(0, 3), (3, 3), (6, 2), (8, 2), (10, 2), (12, 4)],
]
PRE_RUN = [(i, 1) for i in range(16)]
CHORUS = [
    [(0, 2), (2, 2), (4, 2), (6, 2), (8, 3), (11, 3), (14, 2)],
    [(0, 3), (3, 3), (6, 2), (8, 2), (10, 1), (11, 1), (12, 4)],
    [(0, 2), (2, 1), (3, 1), (4, 2), (6, 2), (8, 2), (10, 2), (12, 2), (14, 2)],
    [(0, 4), (4, 2), (6, 2), (8, 2), (10, 2), (12, 4)],
]
CHORUS_END = [(0, 2), (2, 2), (4, 4), (8, 8)]
LO, HI = 69, 88


def scale_notes():
    return [o * 12 + pc for o in range(5, 8) for pc in SCALE if LO <= o * 12 + pc <= HI]


def nearest(prev, pcs, bias):
    cands = [n for n in scale_notes() if n % 12 in pcs]
    cands.sort(key=lambda n: abs(n - prev - bias) + rng.random() * 2)
    return cands[0]


def step(prev, bias):
    notes = scale_notes()
    i = min(range(len(notes)), key=lambda k: abs(notes[k] - prev))
    move = rng.choice([-2, -1, -1, 1, 1, 2]) + (1 if bias > 0 else -1 if bias < 0 else 0)
    move = max(-2, min(2, move)) or 1
    return notes[max(0, min(len(notes) - 1, i + move))]


def melody_bar(chord, pattern, prev, end=False, run=False):
    pcs = CHORDS[chord][1]
    out = []
    bias = rng.choice([-3, 3])
    for k, (s, l) in enumerate(pattern):
        if run:
            prev = step(prev, 4) if k else nearest(prev - 7, pcs, 0)
        elif end and k == len(pattern) - 1:
            prev = nearest(prev, [pcs[0], pcs[1]], 0)
        elif s % 4 == 0:
            prev = nearest(prev, pcs, bias)
        else:
            prev = step(prev, bias)
        if prev > HI - 3:
            bias = -3
        elif prev < LO + 3:
            bias = 3
        out.append((s, l, prev))
    return out, prev


# --- the lead line ---------------------------------------------------------
melody = []  # (bar, s16, len16, pitch, phrase_end)
prev = 76
chorus_bars = None
bar = 0
while bar < len(FORM):
    sec, chord = FORM[bar]
    if sec == "intro":
        bar += 1
        continue
    if sec == "chorus" and chorus_bars is not None and FORM[bar - 1][0] != "chorus":
        for i, notes in enumerate(chorus_bars):
            melody += [(bar + i, s, l, p, e) for (s, l, p, e) in notes]
        bar += len(chorus_bars)
        continue
    start = bar
    length = 0
    while bar + length < len(FORM) and FORM[bar + length][0] == sec:
        length += 1
    section = []
    motif = None
    for i in range(length):
        chord = FORM[start + i][1]
        last = i == length - 1
        if sec == "outro":
            pat, end, run = ([(0, 4), (4, 4), (8, 8)] if i == 0 else [(0, 16)]), True, False
        elif sec == "verse":
            pat, end, run = (VERSE_END if i % 4 == 3 else VERSE[(i + rng.randrange(2)) % 4]), i % 4 == 3, False
        elif sec == "pre":
            pat, end, run = (PRE_RUN if last else PRE[i % 2]), False, last
        else:
            pat, end, run = (CHORUS_END if i % 4 == 3 else CHORUS[i % 4]), i % 4 == 3, False
        if sec == "chorus" and i >= 4 and motif and i % 4 != 3:
            notes = motif[i % 4]  # the hook comes back
        else:
            notes, prev = melody_bar(chord, pat, prev, end, run)
        if sec == "chorus" and i < 4:
            motif = motif or {}
            motif[i] = notes
        section.append([(s, l, p, end and k == len(notes) - 1) for k, (s, l, p) in enumerate(notes)])
    for i, notes in enumerate(section):
        melody += [(start + i, s, l, p, e) for (s, l, p, e) in notes]
    if sec == "chorus":
        chorus_bars = section
    bar = start + length

# --- audio -----------------------------------------------------------------
N = int(LENGTH * SR)
mix = [0.0] * N
lead = [0.0] * N
noise = [rng.uniform(-1, 1) for _ in range(SR)]


def mtof(m):
    return 440 * 2 ** ((m - 69) / 12)


def tone(buf, t0, dur, freq, vol, wave="pulse", duty=0.25, rel=0.03, vib=0.0, cutoff=None):
    i0 = int(t0 * SR)
    n = int((dur + rel) * SR)
    ph = 0.0
    lp = 0.0
    a = 1 - math.exp(-2 * math.pi * cutoff / SR) if cutoff else 1
    for k in range(n):
        i = i0 + k
        if i >= len(buf):
            break
        t = k / SR
        f = freq * (1 + vib * math.sin(2 * math.pi * 5.5 * t) * min(1, t * 3))
        ph = (ph + f / SR) % 1
        if wave == "pulse":
            v = 1.0 if ph < duty else -1.0
        elif wave == "saw":
            v = 2 * ph - 1
        elif wave == "tri":
            v = 4 * abs(ph - 0.5) - 1
        else:
            v = math.sin(2 * math.pi * ph)
        lp += a * (v - lp)
        env = min(1, t / 0.006) * (0.65 + 0.35 * math.exp(-t * 6))
        if t > dur:
            env *= max(0, 1 - (t - dur) / rel)
        buf[i] += lp * env * vol


def kick(t0, vol=0.9):
    i0 = int(t0 * SR)
    ph = 0.0
    for k in range(int(0.28 * SR)):
        if i0 + k >= N:
            break
        t = k / SR
        ph += (45 + 110 * math.exp(-t * 28)) / SR
        mix[i0 + k] += math.sin(2 * math.pi * ph) * math.exp(-t * 11) * vol


def snare(t0, vol=0.45):
    i0 = int(t0 * SR)
    for k in range(int(0.18 * SR)):
        if i0 + k >= N:
            break
        t = k / SR
        mix[i0 + k] += (noise[k] * math.exp(-t * 22) + 0.6 * math.sin(2 * math.pi * 185 * t) * math.exp(-t * 35)) * vol


def hat(t0, vol=0.12, open_=False):
    i0 = int(t0 * SR)
    d = 0.16 if open_ else 0.045
    last = 0.0
    j = rng.randrange(SR // 2)
    for k in range(int(d * SR)):
        if i0 + k >= N:
            break
        t = k / SR
        x = noise[j + k]
        mix[i0 + k] += (x - last) * math.exp(-t * (18 if open_ else 90)) * vol
        last = x


for bar, (sec, chord) in enumerate(FORM):
    t = bar * BAR
    root, pcs = CHORDS[chord]
    busy = sec in ("chorus", "pre")
    for b in range(4):
        if sec != "intro" or b % 2 == 0:
            if busy or sec == "outro" or b % 2 == 0 or (sec == "verse" and b == 3 and bar % 2):
                kick(t + b * BEAT)
        if sec != "intro" and b % 2 == 1:
            snare(t + b * BEAT)
        hat(t + b * BEAT + BEAT / 2, 0.14 if busy else 0.1, open_=busy and b == 3)
        if busy:
            hat(t + b * BEAT + BEAT / 4, 0.06)
            hat(t + b * BEAT + 3 * BEAT / 4, 0.06)
    if sec == "pre" and bar == max(i for i, f in enumerate(FORM) if f[0] == "pre"):
        for k in range(8):
            snare(t + 2 * BEAT + k * BEAT / 4, 0.18 + 0.04 * k)
    # bass: driving eighths, octave jumps in the chorus
    if sec == "outro" and chord == "Am":
        tone(mix, t, BAR, mtof(root - 12), 0.22, "saw", cutoff=500, rel=0.6)
    else:
        for k in range(8):
            m = root - 12 + (12 if busy and k % 2 else 0)
            tone(mix, t + k * BEAT / 2, BEAT / 2 * 0.8, mtof(m), 0.2, "saw", cutoff=700)
    # chord stabs on the off-beats, an arp over the chorus
    tones = sorted(60 + pc if pc >= 4 else 72 + pc for pc in pcs)
    if sec in ("chorus", "pre", "verse"):
        for b in range(4):
            for m in tones:
                tone(mix, t + b * BEAT + BEAT / 2, 0.09, mtof(m), 0.05 if sec != "verse" else 0.035, "saw", cutoff=2500, rel=0.05)
    if sec == "chorus":
        arp = tones + [tones[0] + 12]
        for k in range(16):
            tone(mix, t + k * S16, S16 * 0.7, mtof(arp[k % 4] + 12), 0.05, "tri", rel=0.02)
    if sec in ("intro", "outro"):
        for m in tones:
            tone(mix, t, BAR * 0.95, mtof(m), 0.05, "tri", rel=0.4)

for bar, s, l, p, e in melody:
    tone(lead, bar * BAR + s * S16, l * S16 * 0.92, mtof(p), 0.16, "pulse", duty=0.25 if FORM[bar][0] != "chorus" else 0.5 - 0.25 * (bar % 2), vib=0.006 if l >= 6 else 0)
echo = int(3 * S16 * SR)
for i in range(N):
    mix[i] += lead[i] + (0.28 * lead[i - echo] if i >= echo else 0)


def write(path, buf, peak=0.85):
    m = max(1e-9, max(abs(x) for x in buf))
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(b"".join(struct.pack("<h", int(math.tanh(x / m * 1.2) / math.tanh(1.2) * peak * 32767)) for x in buf))


write(os.path.join(HERE, "song.wav"), mix)


def sfx(name, fn, dur):
    buf = [0.0] * int(dur * SR)
    fn(buf)
    write(os.path.join(HERE, name), buf, 0.6)


def tap_sfx(buf):
    for k in range(len(buf)):
        t = k / SR
        buf[k] = (math.sin(2 * math.pi * 1320 * t) * 0.6 + math.sin(2 * math.pi * 2640 * t) * 0.25) * math.exp(-t * 45) + noise[k] * math.exp(-t * 400) * 0.6


def crit_sfx(buf):
    for k in range(len(buf)):
        t = k / SR
        buf[k] = sum(math.sin(2 * math.pi * f * t) * a for f, a in ((1568, 0.5), (2093, 0.4), (3136, 0.25))) * math.exp(-t * 14) + noise[k] * math.exp(-t * 300) * 0.5


def flick_sfx(buf):
    ph = 0.0
    for k in range(len(buf)):
        t = k / SR
        ph += (900 + 5000 * t) / SR
        buf[k] = math.sin(2 * math.pi * ph) * 0.4 * math.exp(-t * 18) + noise[k] * 0.5 * math.sin(math.pi * min(1, t / 0.12)) * math.exp(-t * 10)


sfx("tap.wav", tap_sfx, 0.12)
sfx("crit.wav", crit_sfx, 0.3)
sfx("flick.wav", flick_sfx, 0.18)

# --- charts ----------------------------------------------------------------
# note: [time, duration, lane, width, type]; type 0 tap, 1 critical, 2 flick, 3 hold
LEVELS = {
    "easy": dict(level=6, grid=4, gap=4, width=lambda: 4, hold=8, flick="4bar", doubles=lambda sec, s: False, fill=False),
    "normal": dict(level=12, grid=2, gap=2, width=lambda: 3, hold=6, flick="phrase", doubles=lambda sec, s: sec == "chorus" and s == 0 and rng.random() < 0.5, fill=False),
    "hard": dict(level=19, grid=2, gap=2, width=lambda: 3, hold=6, flick="phrase", doubles=lambda sec, s: sec == "chorus" and s in (0, 8), fill=False),
    "expert": dict(level=26, grid=1, gap=1, width=lambda: rng.choice([2, 3, 3]), hold=6, flick="more", doubles=lambda sec, s: (sec == "chorus" and s % 4 == 0 and rng.random() < 0.6) or (sec != "chorus" and s == 0), fill=False),
    "master": dict(level=31, grid=1, gap=1, width=lambda: rng.choice([2, 2, 3]), hold=8, flick="more", doubles=lambda sec, s: (sec in ("chorus", "pre") and s % 4 == 0) or s == 0, fill=True),
}


def overlaps(a, b):
    return a[2] < b[2] + b[3] and b[2] < a[2] + a[3]


def busy(notes, n):
    for o in notes:
        if overlaps(o, n):
            if abs(o[0] - n[0]) < 1e-6:
                return True
            if o[4] == 3 and o[0] - 0.05 <= n[0] <= o[0] + o[1] + 0.1:
                return True
    return False


def chart(p):
    notes = []
    kept = []
    last = -99
    for idx, (bar, s, l, pitch, end) in enumerate(melody):
        pos = bar * 16 + s
        if s % p["grid"] or pos - last < p["gap"]:
            continue
        last = pos
        kept.append((idx, bar, s, l, pitch, end))
    phrase_ends = set()
    for k, (idx, bar, s, l, pitch, end) in enumerate(kept):
        nxt = kept[k + 1] if k + 1 < len(kept) else None
        gap16 = (nxt[1] * 16 + nxt[2]) - (bar * 16 + s) if nxt else 16
        if end or gap16 >= (8 if p["flick"] == "4bar" else 6):
            phrase_ends.add(k)
    for k, (idx, bar, s, l, pitch, end) in enumerate(kept):
        sec = FORM[bar][0]
        t = bar * BAR + s * S16
        w = p["width"]()
        lane = round((pitch - LO) / (HI - LO) * (12 - w))
        lane = max(0, min(12 - w, lane + rng.choice([-1, 0, 0, 1])))
        nxt = kept[k + 1] if k + 1 < len(kept) else None
        room = ((nxt[1] * 16 + nxt[2]) - (bar * 16 + s)) if nxt else l
        typ, dur = 0, 0.0
        if l >= p["hold"] and room > 4:
            typ, dur = 3, (min(l, room) - 2) * S16
        elif (k in phrase_ends and (p["flick"] != "4bar" or rng.random() < 0.5)) \
                or (p["flick"] == "more" and s % 4 == 2 and rng.random() < 0.15):
            typ = 2
        if sec == "chorus" and s == 0 and FORM[bar - 1][0] != "chorus" and p["level"] > 6:
            typ = 1 if typ != 3 else 3
        if k == len(kept) - 1:
            typ = 1
        n = [t, dur, lane, w, typ]
        if busy(notes, n):
            continue
        notes.append(n)
        if p["doubles"](sec, s) and typ != 3:
            w2 = p["width"]()
            m = [t, 0.0, 12 - w2 - lane, w2, 2 if typ == 2 and p["level"] > 20 else 0]
            if overlaps(m, n):
                m[2] = 0 if lane + w / 2 > 6 else 12 - w2
            if not overlaps(m, n) and not busy(notes, m):
                notes.append(m)
    if p["fill"]:
        # master: when the lead rests, the drums are notes, hands alternating
        for bar, (sec, chord) in enumerate(FORM):
            if sec in ("intro",):
                continue
            for b in range(8):
                t = bar * BAR + b * BEAT / 2
                if any(abs(n[0] - t) < BEAT * 0.74 for n in notes):
                    continue
                w = 2
                lane = rng.randrange(0, 4) if b % 2 == 0 else rng.randrange(6, 10)
                n = [t, 0.0, lane, w, 0]
                if not busy(notes, n):
                    notes.append(n)
    notes.sort(key=lambda n: (n[0], n[2]))
    return [[round(n[0], 4), round(n[1], 4), n[2], n[3], n[4]] for n in notes]


charts = {name: chart(p) for name, p in LEVELS.items()}
meta = {
    "title": "Sliver Sekai",
    "bpm": BPM,
    "length": round(LENGTH, 3),
    "levels": {name: p["level"] for name, p in LEVELS.items()},
}
with open(os.path.join(HERE, "chart.js"), "w") as f:
    f.write(".pragma library\n// generated by gen.py\n")
    f.write("var meta = " + json.dumps(meta) + ";\n")
    f.write("var charts = " + json.dumps(charts, separators=(",", ":")) + ";\n")
print({k: len(v) for k, v in charts.items()}, f"{LENGTH:.1f}s")
