//! sliverd's built-in layers, drawn with sliver-render's embedded font and icons. They depend on
//! no config, no system fonts and no agent, so Esc/F-keys always render (invariant 1).
//!
//! - **F-keys**: Esc + F1–F12. The safe layer's base, and the built-in `fn` layer that stands in
//!   whenever an active agent has no valid `fn` layer of its own (it satisfies any escape floor).
//! - **Media**: tiny-dfr's default media row (brightness, mic, search, keyboard light, playback,
//!   volume). The safe layer's `fn`. Esc leads it on bars without a physical Esc.

use sliver_ipc::keys::{self, KEY_ESC};
use sliver_ipc::proto::{Rect, Region, RegionKind};
use sliver_render::{Icon, Item, Pixmap, Renderer, Scene, Theme};

/// evdev codes for tiny-dfr's default media layer.
pub mod media_keys {
    pub const BRIGHTNESS_DOWN: u16 = 224;
    pub const BRIGHTNESS_UP: u16 = 225;
    pub const MIC_MUTE: u16 = 248;
    pub const SEARCH: u16 = 217;
    pub const ILLUM_DOWN: u16 = 229;
    pub const ILLUM_UP: u16 = 230;
    pub const PREVIOUS_SONG: u16 = 165;
    pub const PLAY_PAUSE: u16 = 164;
    pub const NEXT_SONG: u16 = 163;
    pub const MUTE: u16 = 113;
    pub const VOLUME_DOWN: u16 = 114;
    pub const VOLUME_UP: u16 = 115;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinKind {
    FKeys,
    Media,
}

/// One built-in layer: what it shows, where its keys are, and the rendered idle frame.
pub struct BuiltinLayer {
    pub kind: BuiltinKind,
    scene: Scene,
    /// One key region per scene item, same order. Region id = item index.
    pub regions: Vec<Region>,
    pub image: Pixmap,
}

impl BuiltinLayer {
    pub fn new(
        kind: BuiltinKind,
        lead_with_esc: bool,
        renderer: &mut Renderer,
        theme: &Theme,
        width: u32,
        height: u32,
    ) -> BuiltinLayer {
        let mut keys: Vec<(Item, u16)> = Vec::new();
        if lead_with_esc {
            keys.push((Item::text("esc").fill(1), KEY_ESC));
        }
        match kind {
            BuiltinKind::FKeys => keys.extend(
                keys::F_KEYS
                    .iter()
                    .enumerate()
                    .map(|(i, k)| (Item::text(format!("F{}", i + 1)).fill(1), *k)),
            ),
            BuiltinKind::Media => {
                use media_keys::*;
                let row = [
                    ("brightness_low", BRIGHTNESS_DOWN),
                    ("brightness_high", BRIGHTNESS_UP),
                    ("mic_off", MIC_MUTE),
                    ("search", SEARCH),
                    ("backlight_low", ILLUM_DOWN),
                    ("backlight_high", ILLUM_UP),
                    ("fast_rewind", PREVIOUS_SONG),
                    ("play_pause", PLAY_PAUSE),
                    ("fast_forward", NEXT_SONG),
                    ("volume_off", MUTE),
                    ("volume_down", VOLUME_DOWN),
                    ("volume_up", VOLUME_UP),
                ];
                keys.extend(row.iter().map(|(icon, k)| {
                    let icon = Icon::builtin(icon).expect("built-in icon");
                    (Item::icon(icon).fill(1), *k)
                }));
            }
        }
        let scene = Scene::new(keys.iter().map(|(item, _)| item.clone()).collect());
        let rects = renderer.layout(&scene, theme, width, height);
        let regions = touch_regions(&rects, width, height)
            .into_iter()
            .zip(&keys)
            .enumerate()
            .map(|(i, (rect, (_, key)))| Region {
                id: i as u32,
                rect,
                kind: RegionKind::Key { keys: vec![*key] },
            })
            .collect();
        let image = renderer.render_new(&scene, theme, width, height);
        BuiltinLayer {
            kind,
            scene,
            regions,
            image,
        }
    }

    /// Renders the layer with the given regions (item indices) drawn pressed.
    pub fn render_pressed(
        &self,
        pressed: &[u32],
        renderer: &mut Renderer,
        theme: &Theme,
        out: &mut Pixmap,
    ) {
        let mut scene = self.scene.clone();
        for (i, item) in scene.items_mut().enumerate() {
            item.pressed = pressed.contains(&(i as u32));
        }
        renderer.render(&scene, theme, out);
    }
}

/// Full-height touch rectangles for laid-out items. Gaps are split down the middle (like
/// `sliver_render::hit_test`), so there are no dead zones and the regions never overlap.
fn touch_regions(rects: &[sliver_render::Rect], width: u32, height: u32) -> Vec<Rect> {
    let n = rects.len();
    let edge = |i: usize| -> u32 {
        if i == 0 {
            0
        } else if i == n {
            width
        } else {
            (((rects[i - 1].right() + rects[i].x) / 2.0).round().max(0.0) as u32).min(width)
        }
    };
    (0..n)
        .map(|i| {
            let (l, r) = (edge(i), edge(i + 1));
            Rect::new(l, 0, r.saturating_sub(l), height)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sliver_ipc::proto::{PanelInfo, PixelFormat};
    use sliver_ipc::validate;
    use sliver_render::Fonts;

    fn layer(kind: BuiltinKind, esc: bool, width: u32) -> BuiltinLayer {
        let mut r = Renderer::new(Fonts::embedded());
        BuiltinLayer::new(kind, esc, &mut r, &Theme::tiny_dfr(), width, 60)
    }

    #[test]
    fn builtin_layers_are_valid_and_tile_the_bar() {
        for width in [2008, 2170] {
            let panel = PanelInfo {
                width,
                height: 60,
                format: PixelFormat::Xrgb8888,
                max_brightness: 255,
                model: "x".into(),
            };
            for (kind, esc) in [
                (BuiltinKind::FKeys, true),
                (BuiltinKind::Media, false),
                (BuiltinKind::Media, true),
            ] {
                let l = layer(kind, esc, width);
                validate::validate_regions(&panel, &l.regions).unwrap();
                assert_eq!(l.regions[0].rect.x, 0);
                assert_eq!(l.regions.last().unwrap().rect.right(), u64::from(width));
                for w in l.regions.windows(2) {
                    assert_eq!(w[0].rect.right(), u64::from(w[1].rect.x));
                }
            }
            let f = layer(BuiltinKind::FKeys, true, width);
            assert!(validate::escape_floor(&f.regions, Some(&[]), &keys::ESCAPE_FLOOR).is_empty());
        }
    }
}
