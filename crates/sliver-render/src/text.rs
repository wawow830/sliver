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
    families: HashMap<Vec<String>, String>,
    /// Painted COLRv1 glyphs; `None` = this glyph isn't a colour glyph.
    color: HashMap<ColorKey, Option<Arc<crate::colr::ColorImage>>>,
}

type ColorKey = (fontdb::ID, u16, u16, u32, [u8; 4]);

#[derive(PartialEq, Eq, Hash)]
struct LabelKey {
    text: String,
    family: String,
    weight: u16,
    size_bits: u32,
    markup: bool,
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
    /// The scan reads only the font tables it needs (see `fontscan`).
    pub fn system() -> Fonts {
        let mut db = fontdb::Database::new();
        crate::fontscan::load_system_fonts(&mut db);
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
            families: HashMap::new(),
            color: HashMap::new(),
        }
    }

    /// Unload every font cosmic-text has loaded, keeping the database (so font ids, and the
    /// shaped labels that refer to them, stay valid).
    ///
    /// cosmic-text keeps each font it ever touched, mapped and with its shaping data, for the
    /// life of the `FontSystem`. That's normally a handful. But a glyph that's in none of the
    /// fallback fonts makes it try every font on the system, which is hundreds on a desktop, and
    /// they all stay loaded: tens of MB of heap, and the pages they touched, for one odd
    /// character in a song title. Drawing loads back only the fonts that are actually used.
    pub(crate) fn trim_loaded_fonts(&mut self) {
        let empty = FontSystem::new_with_locale_and_db(String::new(), fontdb::Database::new());
        let (locale, db) = std::mem::replace(&mut self.system, empty).into_locale_and_db();
        // The mappings stay (no read-ahead, see `fontscan`); only the pages drawing touched go.
        crate::fontscan::release_pages(&db);
        self.system = FontSystem::new_with_locale_and_db(locale, db);
    }

    /// How many font faces are known (system, user and embedded).
    pub fn face_count(&self) -> usize {
        self.system.db().len()
    }

    /// Make another font available (a user's emoji font, or a test fixture). Labels shaped before
    /// this call keep their old fallback choices until the cache turns over.
    pub fn add_font_data(&mut self, data: Vec<u8>) {
        self.system.db_mut().load_font_data(data);
        self.labels.clear();
    }

    /// The COLRv1 image for a glyph, painted and cached. `None` for ordinary glyphs.
    pub(crate) fn color_glyph(
        &mut self,
        font_id: fontdb::ID,
        weight: fontdb::Weight,
        glyph_id: u16,
        size: f32,
        fg: [u8; 4],
    ) -> Option<Arc<crate::colr::ColorImage>> {
        let key = (font_id, weight.0, glyph_id, size.to_bits(), fg);
        if let Some(hit) = self.color.get(&key) {
            return hit.clone();
        }
        if self.color.len() >= LABEL_CACHE_MAX {
            self.color.clear();
        }
        let painted = (|| {
            let font = self.system.get_font(font_id, weight)?;
            let index = self.system.db().face(font_id)?.index;
            crate::colr::render(font.data(), index, glyph_id, size, fg).map(Arc::new)
        })();
        self.color.insert(key, painted.clone());
        painted
    }
}

impl Fonts {
    /// The first family in a CSS `font-family` list that's available. Generic families and
    /// anything missing fall back to the embedded font.
    pub(crate) fn pick_family(&mut self, list: &[String]) -> String {
        if let Some(f) = self.families.get(list) {
            return f.clone();
        }
        let db = self.system.db();
        let picked = list
            .iter()
            .find_map(|name| {
                if matches!(
                    name.to_ascii_lowercase().as_str(),
                    "sans-serif" | "sans" | "system-ui" | "ui-sans-serif"
                ) {
                    return Some(EMBEDDED_FONT_FAMILY.to_string());
                }
                db.faces()
                    .flat_map(|f| f.families.iter())
                    .find(|(n, _)| n.eq_ignore_ascii_case(name))
                    .map(|(n, _)| n.clone())
            })
            .unwrap_or_else(|| EMBEDDED_FONT_FAMILY.to_string());
        self.families.insert(list.to_vec(), picked.clone());
        picked
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
    shape_any(fonts, text, family, weight, size, false)
}

/// Like [`shape`], for text in the [`crate::markup`] subset.
pub(crate) fn shape_markup(
    fonts: &mut Fonts,
    text: &str,
    family: &str,
    weight: u16,
    size: f32,
) -> Arc<Label> {
    shape_any(fonts, text, family, weight, size, true)
}

fn shape_any(
    fonts: &mut Fonts,
    text: &str,
    family: &str,
    weight: u16,
    size: f32,
    markup: bool,
) -> Arc<Label> {
    let key = LabelKey {
        text: text.to_owned(),
        family: family.to_owned(),
        weight,
        size_bits: size.to_bits(),
        markup,
    };
    if let Some(label) = fonts.labels.get(&key) {
        return label.clone();
    }
    if fonts.labels.len() >= LABEL_CACHE_MAX {
        fonts.labels.clear();
        fonts.cache.image_cache.clear();
        // Also bounds fonts that fallback loaded over a long uptime.
        fonts.trim_loaded_fonts();
    }
    let label = Arc::new(shape_uncached(
        &mut fonts.system,
        text,
        family,
        weight,
        size,
        markup,
    ));
    // A glyph no font has (.notdef) means fallback just tried every font on the system: unload
    // them. The label stays cached, so this text never triggers the scan again.
    let missing = label
        .buffer
        .layout_runs()
        .any(|run| run.glyphs.iter().any(|g| g.glyph_id == 0));
    fonts.labels.insert(key, label.clone());
    if missing {
        fonts.trim_loaded_fonts();
    }
    label
}

fn shape_uncached(
    fs: &mut FontSystem,
    text: &str,
    family: &str,
    weight: u16,
    size: f32,
    markup: bool,
) -> Label {
    let mut buffer = Buffer::new(fs, Metrics::new(size, size * 1.25));
    buffer.set_size(None, None);
    let attrs = Attrs::new()
        .family(Family::Name(family))
        .weight(Weight(weight));
    if markup {
        let parsed = crate::markup::parse(text);
        let spans: Vec<(&str, Attrs)> = parsed
            .spans
            .iter()
            .map(|s| (s.text.as_str(), span_attrs(&attrs, &s.style, size)))
            .collect();
        buffer.set_rich_text(spans, &attrs, Shaping::Advanced, None);
    } else {
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
    }
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

/// cosmic-text attributes for one markup run, on top of the label's own.
fn span_attrs<'a>(base: &Attrs<'a>, st: &'a crate::markup::Style, size: f32) -> Attrs<'a> {
    let mut a = base.clone();
    if let Some(w) = st.weight {
        a = a.weight(Weight(w));
    }
    if st.italic {
        a = a.style(cosmic_text::Style::Italic);
    }
    if let Some([r, g, b, al]) = st.color {
        a = a.color(cosmic_text::Color::rgba(r, g, b, al));
    }
    if (st.scale - 1.0).abs() > f32::EPSILON {
        let s = size * st.scale;
        a = a.metrics(Metrics::new(s, s * 1.25));
    }
    if let Some(f) = &st.family {
        a = a.family(if f.eq_ignore_ascii_case("monospace") {
            Family::Monospace
        } else {
            Family::Name(f)
        });
    }
    if st.underline || st.strikethrough {
        a.text_decoration = cosmic_text::TextDecoration {
            underline: if st.underline {
                cosmic_text::UnderlineStyle::Single
            } else {
                cosmic_text::UnderlineStyle::None
            },
            strikethrough: st.strikethrough,
            ..cosmic_text::TextDecoration::new()
        };
    }
    a
}
