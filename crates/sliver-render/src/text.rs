use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::fontdb;
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};

/// Family name of the font embedded in the binary.
pub const EMBEDDED_FONT_FAMILY: &str = "Noto Sans";

const EMBEDDED_FONT: &[u8] = include_bytes!("../assets/fonts/NotoSans-Bold-subset.ttf");

/// Fonts available to the renderer, plus shaping and glyph caches.
pub struct Fonts {
    pub(crate) system: FontSystem,
    pub(crate) cache: SwashCache,
    labels: HashMap<LabelKey, Arc<Label>>,
}

#[derive(PartialEq, Eq, Hash)]
struct LabelKey {
    text: String,
    family: String,
    weight: u16,
    size_bits: u32,
}

/// Shaped labels kept between frames. Re-renders mostly repeat the same labels, and shaping with
/// font fallback is the expensive part of a frame.
const LABEL_CACHE_MAX: usize = 1024;

impl Fonts {
    /// Only the embedded font. Deterministic on every machine: use it for the safe layer and for
    /// snapshot tests.
    pub fn embedded() -> Fonts {
        Fonts::with_db(fontdb::Database::new(), "en-US".into())
    }

    /// System fonts (via the fontconfig config files, no C library) plus the embedded font.
    pub fn system() -> Fonts {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let locale = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LANG"))
            .ok()
            .and_then(|l| l.split('.').next().map(|l| l.replace('_', "-")))
            .filter(|l| !l.is_empty() && l != "C" && l != "POSIX")
            .unwrap_or_else(|| "en-US".into());
        Fonts::with_db(db, locale)
    }

    fn with_db(mut db: fontdb::Database, locale: String) -> Fonts {
        db.load_font_data(EMBEDDED_FONT.to_vec());
        db.set_sans_serif_family(EMBEDDED_FONT_FAMILY);
        Fonts {
            system: FontSystem::new_with_locale_and_db(locale, db),
            cache: SwashCache::new(),
            labels: HashMap::new(),
        }
    }
}

/// A shaped single-line label, ready to draw.
pub(crate) struct Label {
    pub buffer: Buffer,
    /// Advance width in pixels.
    pub width: f32,
    /// Height of capital letters above the baseline, in pixels. Used to center labels optically
    /// so "F1" and "g" share a baseline.
    pub cap_height: f32,
}

/// Shapes `text`, or returns the cached result.
pub(crate) fn shape(
    fonts: &mut Fonts,
    text: &str,
    family: &str,
    weight: u16,
    size: f32,
) -> Arc<Label> {
    let key = LabelKey {
        text: text.to_owned(),
        family: family.to_owned(),
        weight,
        size_bits: size.to_bits(),
    };
    if let Some(label) = fonts.labels.get(&key) {
        return label.clone();
    }
    if fonts.labels.len() >= LABEL_CACHE_MAX {
        fonts.labels.clear();
        fonts.cache.image_cache.clear();
    }
    let label = Arc::new(shape_uncached(
        &mut fonts.system,
        text,
        family,
        weight,
        size,
    ));
    fonts.labels.insert(key, label.clone());
    label
}

fn shape_uncached(fs: &mut FontSystem, text: &str, family: &str, weight: u16, size: f32) -> Label {
    let mut buffer = Buffer::new(fs, Metrics::new(size, size * 1.25));
    buffer.set_size(None, None);
    let attrs = Attrs::new()
        .family(Family::Name(family))
        .weight(Weight(weight));
    buffer.set_text(text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(fs, false);

    let mut width: f32 = 0.0;
    let mut font_id = None;
    for run in buffer.layout_runs() {
        width = width.max(run.line_w);
        font_id = font_id.or(run.glyphs.first().map(|g| (g.font_id, g.font_weight)));
    }
    let cap_height = font_id
        .and_then(|(id, w)| fs.get_font(id, w))
        .map(|font| {
            let m = font.as_swash().metrics(&[]);
            let cap = if m.cap_height > 0.0 {
                m.cap_height
            } else {
                m.ascent * 0.7
            };
            cap / m.units_per_em as f32 * size
        })
        .unwrap_or(size * 0.7);
    Label {
        buffer,
        width,
        cap_height,
    }
}
