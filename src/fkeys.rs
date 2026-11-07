use std::{
    collections::{HashMap, HashSet},
    os::unix::net::UnixStream,
};

use wayland_client::protocol::wl_touch;

use crate::{H, W, client::Client};

pub const KEYS: [(&str, u32); 13] = [
    ("ESC", 1),
    ("F1", 59),
    ("F2", 60),
    ("F3", 61),
    ("F4", 62),
    ("F5", 63),
    ("F6", 64),
    ("F7", 65),
    ("F8", 66),
    ("F9", 67),
    ("F10", 68),
    ("F11", 87),
    ("F12", 88),
];

fn key_at(x: f64) -> u32 {
    let i = (x.max(0.0) as i32 * KEYS.len() as i32 / W).min(KEYS.len() as i32 - 1);
    KEYS[i as usize].1
}

pub fn draw() -> Vec<u32> {
    const CAP: u32 = 0x2a2a2a;
    const INK: u32 = 0xffffff;
    const GAP: i32 = 4;
    const SCALE: i32 = 3;
    let mut px = vec![0; (W * H) as usize];
    let mut fill = |x: i32, y: i32, w: i32, h: i32, color: u32| {
        for y in y..y + h {
            px[(y * W + x) as usize..(y * W + x + w) as usize].fill(color);
        }
    };
    let n = KEYS.len() as i32;
    for (i, (label, _)) in (0..).zip(KEYS) {
        let (x0, x1) = (i * W / n + GAP / 2, (i + 1) * W / n - GAP / 2);
        fill(x0, GAP, x1 - x0, H - 2 * GAP, CAP);
        let text_w = label.len() as i32 * 6 * SCALE - SCALE;
        let (tx, ty) = ((x0 + x1 - text_w) / 2, (H - 7 * SCALE) / 2);
        for (c, ch) in (0..).zip(label.bytes()) {
            for (row, bits) in (0..).zip(glyph(ch)) {
                for col in (0..5).filter(|col| bits >> (4 - col) & 1 == 1) {
                    let (x, y) = (tx + (c * 6 + col) * SCALE, ty + row * SCALE);
                    fill(x, y, SCALE, SCALE, INK);
                }
            }
        }
    }
    px
}

fn glyph(c: u8) -> [u8; 7] {
    match c {
        b'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        b'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        b'C' => [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
        b'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        b'0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        b'1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        b'2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        b'3' => [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e],
        b'4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        b'5' => [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
        b'6' => [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        b'7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        b'8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        b'9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
        _ => unreachable!("no glyph for {c}"),
    }
}

pub fn run(stream: UnixStream) {
    let serve = || {
        let client = Client::connect(stream)?;
        let keyboard = client.keyboard()?;
        let mut held = HashMap::new();
        client.show(&draw(), move |event| {
            let before: HashSet<u32> = held.values().copied().collect();
            match event {
                wl_touch::Event::Down { id, x, .. } => drop(held.insert(id, key_at(x))),
                wl_touch::Event::Up { id, .. } => drop(held.remove(&id)),
                wl_touch::Event::Cancel => held.clear(),
                _ => return,
            }
            let after: HashSet<u32> = held.values().copied().collect();
            for &key in after.difference(&before) {
                keyboard.key(0, key, 1);
            }
            for &key in before.difference(&after) {
                keyboard.key(0, key, 0);
            }
        })
    };
    if let Err(e) = serve() {
        eprintln!("sliverd: fkeys: {e}");
    }
}
