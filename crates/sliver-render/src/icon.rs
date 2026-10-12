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
    #[error("unsupported icon format (want .svg or .png): {0}")]
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
}

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

impl Icon {
    pub fn from_svg(data: &[u8]) -> Result<Icon, IconError> {
        let tree = usvg::Tree::from_data(data, &usvg::Options::default())?;
        Ok(Icon {
            image: Image::Svg(Arc::new(tree)),
            symbolic: false,
        })
    }

    pub fn from_png(data: &[u8]) -> Result<Icon, IconError> {
        let pixmap = Pixmap::decode_png(data).map_err(|e| IconError::Png(e.to_string()))?;
        Ok(Icon {
            image: Image::Raster(Arc::new(pixmap)),
            symbolic: false,
        })
    }

    /// Loads an `.svg` or `.png` file. Files named `*-symbolic.*` are symbolic.
    pub fn load(path: impl AsRef<Path>) -> Result<Icon, IconError> {
        let path = path.as_ref();
        let data = std::fs::read(path)?;
        let icon = match path.extension().and_then(|e| e.to_str()) {
            Some("svg") => Icon::from_svg(&data)?,
            Some("png") => Icon::from_png(&data)?,
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

    /// Intrinsic size in pixels.
    pub fn size(&self) -> (f32, f32) {
        match &self.image {
            Image::Svg(t) => (t.size().width(), t.size().height()),
            Image::Raster(p) => (p.width() as f32, p.height() as f32),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtin_icons_parse() {
        for name in BUILTIN_ICONS {
            assert!(Icon::builtin(name).is_some(), "{name}");
        }
        assert!(Icon::builtin("nope").is_none());
    }
}
