use std::fmt;
use std::path::Path;
use std::sync::Arc;

use resvg::usvg;
use tiny_skia::Pixmap;

#[derive(Debug, thiserror::Error)]
pub enum IconError {
    #[error("reading icon: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing SVG: {0}")]
    Svg(#[from] usvg::Error),
    #[error("decoding PNG: {0}")]
    Png(String),
    #[error("unsupported icon format (want .svg, .png or .jpg): {0}")]
    Format(String),
}

/// A decoded icon, cheap to clone. SVGs are drawn as vectors at whatever size the theme asks for.
///
/// Symbolic icons (the XDG `-symbolic` convention, and all built-in icons) are monochrome shapes
/// that get drawn in the item's foreground color. Other icons keep their own colors.
#[derive(Clone)]
pub struct Icon {
    pub(crate) image: Image,
    pub symbolic: bool,
    /// Where its visible pixels are, measured once on first use and shared between clones.
    content: Arc<std::sync::OnceLock<Option<Rect>>>,
    /// Drawn as just this part of the canvas ([`Icon::fit_content`]).
    view: Option<Rect>,
}

/// A part of an icon's canvas, in its intrinsic units: x, y, width, height.
type Rect = [f32; 4];

#[derive(Clone)]
pub(crate) enum Image {
    Svg(Arc<usvg::Tree>),
    Raster(Arc<Pixmap>),
}

impl fmt::Debug for Icon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (w, h) = self.size();
        let kind = match self.image {
            Image::Svg(_) => "svg",
            Image::Raster(_) => "raster",
        };
        write!(f, "Icon({kind} {w}x{h}, symbolic: {})", self.symbolic)
    }
}

macro_rules! builtin_icons {
    ($($name:literal),* $(,)?) => {
        /// Names of the icons embedded in the binary (Material Symbols, as shipped by tiny-dfr).
        pub const BUILTIN_ICONS: &[&str] = &[$($name),*];
        fn builtin_svg(name: &str) -> Option<&'static [u8]> {
            match name {
                $($name => Some(include_bytes!(concat!("../assets/icons/", $name, ".svg"))),)*
                _ => None,
            }
        }
    };
}

builtin_icons![
    "backlight_high",
    "backlight_low",
    "battery_0_bar",
    "battery_1_bar",
    "battery_2_bar",
    "battery_3_bar",
    "battery_4_bar",
    "battery_5_bar",
    "battery_6_bar",
    "battery_charging_20",
    "battery_charging_30",
    "battery_charging_50",
    "battery_charging_60",
    "battery_charging_80",
    "battery_charging_90",
    "battery_charging_full",
    "battery_full",
    "bolt",
    "brightness_high",
    "brightness_low",
    "fast_forward",
    "fast_rewind",
    "mic_off",
    "play_pause",
    "search",
    "volume_down",
    "volume_off",
    "volume_up",
];

/// Raster icons (PNG/JPEG, including apps' `icon-data` and album art) larger than this on
/// either side are refused before decoding: a few bytes of PNG can claim 30000x30000 pixels,
/// and allocating that would abort the agent. 4096 still fits big album covers.
pub const MAX_RASTER_SIDE: u32 = 4096;

/// A PNG's size from its IHDR chunk, without decoding anything.
fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < 24 || &data[..8] != SIG || &data[12..16] != b"IHDR" {
        return None;
    }
    let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    Some((be(&data[16..20]), be(&data[20..24])))
}

fn too_big(w: u32, h: u32) -> Option<IconError> {
    (w > MAX_RASTER_SIDE || h > MAX_RASTER_SIDE).then(|| {
        IconError::Png(format!(
            "{w}x{h} is larger than {MAX_RASTER_SIDE}x{MAX_RASTER_SIDE}"
        ))
    })
}

impl Icon {
    fn new(image: Image, symbolic: bool) -> Icon {
        Icon {
            image,
            symbolic,
            content: Arc::default(),
            view: None,
        }
    }

    pub fn from_svg(data: &[u8]) -> Result<Icon, IconError> {
        let tree = usvg::Tree::from_data(data, &usvg::Options::default())?;
        Ok(Icon::new(Image::Svg(Arc::new(tree)), false))
    }

    pub fn from_png(data: &[u8]) -> Result<Icon, IconError> {
        if let Some(e) = png_size(data).and_then(|(w, h)| too_big(w, h)) {
            return Err(e);
        }
        let pixmap = Pixmap::decode_png(data).map_err(|e| IconError::Png(e.to_string()))?;
        Ok(Icon::new(Image::Raster(Arc::new(pixmap)), false))
    }

    /// A JPEG photo (album art), as an opaque raster icon.
    pub fn from_jpeg(data: &[u8]) -> Result<Icon, IconError> {
        use zune_jpeg::JpegDecoder;
        use zune_jpeg::zune_core::colorspace::ColorSpace;
        use zune_jpeg::zune_core::options::DecoderOptions;
        let side = MAX_RASTER_SIDE as usize;
        let opts = DecoderOptions::default()
            .jpeg_set_out_colorspace(ColorSpace::RGBA)
            .set_max_width(side)
            .set_max_height(side);
        let mut dec = JpegDecoder::new_with_options(data, opts);
        let pixels = dec
            .decode()
            .map_err(|e| IconError::Png(format!("jpeg: {e:?}")))?;
        let info = dec
            .info()
            .ok_or_else(|| IconError::Png("jpeg: no header".into()))?;
        let (w, h) = (u32::from(info.width), u32::from(info.height));
        if let Some(e) = too_big(w, h) {
            return Err(e);
        }
        let mut pm = Pixmap::new(w, h).ok_or_else(|| IconError::Png("jpeg: empty".into()))?;
        // Opaque pixels: premultiplication changes nothing. A decoder that returned fewer
        // pixels than the header promised is an error, not a panic.
        let n = (w * h * 4) as usize;
        let src = pixels
            .get(..n)
            .ok_or_else(|| IconError::Png("jpeg: truncated pixel data".into()))?;
        pm.data_mut().copy_from_slice(src);
        Ok(Icon::new(Image::Raster(Arc::new(pm)), false))
    }

    /// Loads an `.svg`, `.png` or `.jpg` file. Files named `*-symbolic.*` are symbolic.
    pub fn load(path: impl AsRef<Path>) -> Result<Icon, IconError> {
        let path = path.as_ref();
        let data = std::fs::read(path)?;
        let icon = match path.extension().and_then(|e| e.to_str()) {
            Some("svg") => Icon::from_svg(&data)?,
            Some("png") => Icon::from_png(&data)?,
            Some("jpg" | "jpeg") => Icon::from_jpeg(&data)?,
            _ => return Err(IconError::Format(path.display().to_string())),
        };
        let symbolic = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.ends_with("-symbolic"));
        Ok(icon.symbolic(symbolic))
    }

    /// Marks the icon as symbolic (drawn in the foreground color) or not.
    pub fn symbolic(mut self, symbolic: bool) -> Icon {
        self.symbolic = symbolic;
        self
    }

    /// An icon embedded in the binary, by name (see [`BUILTIN_ICONS`]).
    pub fn builtin(name: &str) -> Option<Icon> {
        let data = builtin_svg(name)?;
        let icon = Icon::from_svg(data).expect("embedded icons are valid SVG");
        Some(icon.symbolic(true))
    }

    /// The SVG source of an embedded icon, for UIs that draw it themselves (pickers).
    pub fn builtin_source(name: &str) -> Option<&'static [u8]> {
        builtin_svg(name)
    }

    /// The same picture at most `max_h` pixels tall (aspect kept), so a photo or album cover
    /// isn't kept at full size for a 60 px bar. SVGs, and rasters already that small, are
    /// returned as they are. Halves first, then one bicubic step, so big reductions don't alias.
    pub fn fit_height(self, max_h: u32) -> Icon {
        let Image::Raster(src) = &self.image else {
            return self;
        };
        if max_h == 0 || src.height() <= max_h {
            return self;
        }
        let scaled = |p: &Pixmap, w: u32, h: u32| -> Option<Pixmap> {
            let mut out = Pixmap::new(w.max(1), h.max(1))?;
            let paint = tiny_skia::PixmapPaint {
                quality: tiny_skia::FilterQuality::Bicubic,
                ..Default::default()
            };
            let t = tiny_skia::Transform::from_scale(
                w.max(1) as f32 / p.width() as f32,
                h.max(1) as f32 / p.height() as f32,
            );
            out.draw_pixmap(0, 0, p.as_ref(), &paint, t, None);
            Some(out)
        };
        let mut cur = std::borrow::Cow::Borrowed(&**src);
        while cur.height() >= max_h * 2 {
            match scaled(&cur, cur.width() / 2, cur.height() / 2) {
                Some(p) => cur = std::borrow::Cow::Owned(p),
                None => return self,
            }
        }
        let w = (cur.width() as f32 * max_h as f32 / cur.height() as f32).round() as u32;
        match scaled(&cur, w, max_h) {
            Some(p) => Icon::new(Image::Raster(Arc::new(p)), self.symbolic),
            None => self,
        }
    }

    /// Intrinsic size in pixels (of the [`Icon::fit_content`] view, if it is one).
    pub fn size(&self) -> (f32, f32) {
        match self.view {
            Some([_, _, w, h]) => (w, h),
            None => self.canvas_size(),
        }
    }

    fn canvas_size(&self) -> (f32, f32) {
        match &self.image {
            Image::Svg(t) => (t.size().width(), t.size().height()),
            Image::Raster(p) => (p.width() as f32, p.height() as f32),
        }
    }

    /// Where on the canvas drawing starts: the view's corner, else the origin.
    pub(crate) fn origin(&self) -> (f32, f32) {
        self.view.map_or((0.0, 0.0), |[x, y, _, _]| (x, y))
    }

    /// The icon cropped to its visible artwork (`-sliver-icon-fit: content`), so icons whose
    /// files have different transparent margins come out the same size. Unchanged if nothing
    /// in it is visible.
    pub fn fit_content(&self) -> Icon {
        let mut icon = self.clone();
        icon.view = self.content_box().or(self.view);
        icon
    }

    /// The part of the canvas its visible pixels cover. Measured on the pixels, not the SVG's
    /// shapes, because app icons often carry invisible backgrounds the size of the canvas.
    pub fn content_box(&self) -> Option<Rect> {
        *self.content.get_or_init(|| self.measure())
    }

    fn measure(&self) -> Option<Rect> {
        // Rasters by their own pixels; SVGs drawn once, 128 px on the long side.
        let (w, h) = self.canvas_size();
        let drawn;
        let (pm, scale) = match &self.image {
            Image::Raster(p) => (&**p, 1.0),
            Image::Svg(tree) => {
                let scale = 128.0 / w.max(h).max(1.0);
                let mut p = Pixmap::new((w * scale).ceil() as u32, (h * scale).ceil() as u32)?;
                let t = tiny_skia::Transform::from_scale(scale, scale);
                resvg::render(tree, t, &mut p.as_mut());
                drawn = p;
                (&drawn, scale)
            }
        };
        let pw = pm.width();
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for (i, px) in pm.data().chunks_exact(4).enumerate() {
            // Nearly transparent pixels (soft shadows, antialiasing haze) don't count.
            if px[3] > 24 {
                let (x, y) = (i as u32 % pw, i as u32 / pw);
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
        (x1 > x0).then(|| [x0, y0, x1 - x0, y1 - y0].map(|v| v as f32 / scale))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_box_is_the_visible_artwork() {
        // A 10×10 raster with a 4×4 opaque square at (3, 3).
        let mut p = Pixmap::new(10, 10).unwrap();
        p.fill_rect(
            tiny_skia::Rect::from_xywh(3.0, 3.0, 4.0, 4.0).unwrap(),
            &tiny_skia::Paint::default(),
            tiny_skia::Transform::identity(),
            None,
        );
        let icon = Icon::new(Image::Raster(Arc::new(p)), false);
        assert_eq!(icon.content_box(), Some([3.0, 3.0, 4.0, 4.0]));
        let fit = icon.fit_content();
        assert_eq!((fit.size(), fit.origin()), ((4.0, 4.0), (3.0, 3.0)));
        assert_eq!(icon.size(), (10.0, 10.0), "the original keeps its canvas");
        // An SVG with an invisible full-canvas background: only the circle counts.
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">
            <rect width="64" height="64" fill="#000" fill-opacity="0"/>
            <circle cx="32" cy="32" r="16"/></svg>"##;
        let [x, y, w, h] = Icon::from_svg(svg).unwrap().content_box().unwrap();
        assert!((x - 16.0).abs() < 1.0 && (y - 16.0).abs() < 1.0, "{x} {y}");
        assert!((w - 32.0).abs() < 1.0 && (h - 32.0).abs() < 1.0, "{w} {h}");
        let blank = Icon::new(Image::Raster(Arc::new(Pixmap::new(4, 4).unwrap())), false);
        assert_eq!(
            blank.fit_content().size(),
            (4.0, 4.0),
            "nothing visible: unchanged"
        );
    }

    #[test]
    fn all_builtin_icons_parse() {
        for name in BUILTIN_ICONS {
            assert!(Icon::builtin(name).is_some(), "{name}");
        }
        assert!(Icon::builtin("nope").is_none());
    }
}

#[cfg(test)]
mod jpeg_tests {
    use super::*;

    /// A PNG that is only a header claiming `w` x `h` (a decompression bomb's opening).
    fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut d = b"\x89PNG\r\n\x1a\n".to_vec();
        d.extend(13u32.to_be_bytes());
        d.extend(b"IHDR");
        d.extend(w.to_be_bytes());
        d.extend(h.to_be_bytes());
        d.extend([8, 6, 0, 0, 0]); // 8-bit RGBA
        d.extend([0; 4]); // CRC (never reached)
        d
    }

    #[test]
    fn oversized_rasters_are_refused_before_decoding() {
        let t = std::time::Instant::now();
        let e = Icon::from_png(&png_header(40_000, 40_000)).unwrap_err();
        assert!(e.to_string().contains("larger than"), "{e}");
        assert!(
            Icon::from_png(&png_header(10, 5000)).is_err(),
            "either side"
        );
        assert!(
            t.elapsed() < std::time::Duration::from_millis(50),
            "no allocation attempt"
        );
        // A real, normal-sized PNG still decodes.
        let mut pm = Pixmap::new(8, 8).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        assert!(Icon::from_png(&pm.encode_png().unwrap()).is_ok());
    }

    #[test]
    fn decodes_a_jpeg_and_loads_it_by_extension() {
        let data = include_bytes!("../tests/fixtures/art-48x32.jpg");
        let icon = Icon::from_jpeg(data).expect("jpeg decodes");
        let Image::Raster(p) = &icon.image else {
            panic!("raster expected")
        };
        assert_eq!((p.width(), p.height()), (48, 32));
        // red at the top-left to blue at the bottom-right
        let px = p.data();
        assert!(px[0] > px[2], "top-left is red-ish: {:?}", &px[..4]);
        let last = (47 + 31 * 48) * 4;
        assert!(px[last + 2] > px[last], "bottom-right is blue-ish");
        assert!(Icon::from_jpeg(b"not a jpeg").is_err());
        let path = std::env::temp_dir().join(format!("sliver-art-{}.jpg", std::process::id()));
        std::fs::write(&path, data).unwrap();
        assert!(Icon::load(&path).is_ok());
        let _ = std::fs::remove_file(&path);
    }
}
