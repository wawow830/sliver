//! The Appearance page's "Look" controls (ux.md §3), stored as a managed block at the top of the
//! user's `style.css`:
//!
//! ```css
//! /* sliver-settings: begin {font-family = "Inter", radius = 14} (managed block: edit freely
//!    below the end marker; this part is rewritten by the settings app) */
//! bar { font-family: "Inter"; }
//! module { border-radius: 14px; }
//! /* sliver-settings: end */
//! ```
//!
//! The values live in the first comment as an inline TOML table, so the app reads them back
//! without parsing CSS, and the CSS under it is what the agent really uses. Hand-written rules
//! below the block come later in the cascade, so they win.

use toml::{Table, Value};

const BEGIN: &str = "/* sliver-settings: begin ";
const END: &str = "/* sliver-settings: end */";

/// How a pressed module looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Press {
    /// Whatever the theme does.
    #[default]
    Theme,
    Dim,
    Invert,
    Accent,
}

impl Press {
    pub const ALL: [(Press, &'static str, &'static str); 4] = [
        (Press::Theme, "theme", "Theme default"),
        (Press::Dim, "dim", "Dim"),
        (Press::Invert, "invert", "Invert"),
        (Press::Accent, "accent", "Accent colour"),
    ];

    fn id(self) -> &'static str {
        Self::ALL.iter().find(|(p, ..)| *p == self).unwrap().1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Look {
    pub font_family: Option<String>,
    pub font_size: Option<u32>,
    pub radius: Option<u32>,
    pub outlines: bool,
    /// `#rrggbb`
    pub accent: Option<String>,
    pub animations_off: bool,
    pub press: Press,
}

impl Look {
    pub fn is_default(&self) -> bool {
        *self == Look::default()
    }

    /// Checks what a text field may hold; the error is shown to the user.
    pub fn check_family(s: &str) -> Result<(), String> {
        if s.chars()
            .any(|c| matches!(c, '"' | '\'' | '{' | '}' | ';' | '\\' | '\n' | '*' | '/'))
        {
            return Err("A font family is just a name, like Inter or Noto Sans".into());
        }
        Ok(())
    }

    pub fn check_accent(s: &str) -> Result<(), String> {
        let ok =
            s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit());
        ok.then_some(())
            .ok_or_else(|| "Use a colour like #3584e4".into())
    }

    fn values(&self) -> Table {
        let mut t = Table::new();
        if let Some(f) = &self.font_family {
            t.insert("font-family".into(), f.clone().into());
        }
        if let Some(n) = self.font_size {
            t.insert("font-size".into(), i64::from(n).into());
        }
        if let Some(n) = self.radius {
            t.insert("radius".into(), i64::from(n).into());
        }
        if self.outlines {
            t.insert("outlines".into(), true.into());
        }
        if let Some(a) = &self.accent {
            t.insert("accent".into(), a.clone().into());
        }
        if self.animations_off {
            t.insert("animations-off".into(), true.into());
        }
        if self.press != Press::Theme {
            t.insert("press".into(), self.press.id().into());
        }
        t
    }

    /// The managed block (empty for all defaults).
    pub fn block(&self) -> String {
        if self.is_default() {
            return String::new();
        }
        let mut css = String::new();
        if let Some(a) = &self.accent {
            css += &format!("@define-color accent {a};\n");
        }
        if self.font_family.is_some() || self.font_size.is_some() {
            css += "bar {";
            if let Some(f) = &self.font_family {
                css += &format!(" font-family: \"{f}\", sans-serif;");
            }
            if let Some(n) = self.font_size {
                css += &format!(" font-size: {n}px;");
            }
            css += " }\n";
        }
        if self.radius.is_some() || self.outlines {
            css += "module {";
            if let Some(n) = self.radius {
                css += &format!(" border-radius: {n}px;");
            }
            if self.outlines {
                css += " border: 2px solid alpha(white, 0.6);";
            }
            css += " }\n";
        }
        match self.press {
            Press::Theme => {}
            Press::Dim => css += "module:active { opacity: 0.6; }\n",
            Press::Invert => css += "module:active { background-color: #fff; color: #000; }\n",
            Press::Accent => css += "module:active { background-color: @accent; }\n",
        }
        if self.animations_off {
            css += "bar { -sliver-layer-transition: none; }\nmodule, module:active { transition: none; }\n";
        }
        let v = Value::Table(self.values());
        format!(
            "{BEGIN}{} (managed by sliver-settings: your own rules go below the end marker) */\n{css}{END}\n",
            sliver_config::writer::inline(&v)
        )
    }

    /// What `css` (the whole file) says, i.e. the managed block's values. A file without a block,
    /// or with one we can't read, is "all defaults".
    pub fn parse(css: &str) -> Look {
        let Some(start) = css.find(BEGIN) else {
            return Look::default();
        };
        let rest = &css[start + BEGIN.len()..];
        let Some(end) = rest.find(" (managed by").or_else(|| rest.find("*/")) else {
            return Look::default();
        };
        let Ok(t) = format!("v = {}", rest[..end].trim()).parse::<Table>() else {
            return Look::default();
        };
        let Some(Value::Table(v)) = t.get("v") else {
            return Look::default();
        };
        let num = |k: &str| {
            v.get(k)
                .and_then(Value::as_integer)
                .and_then(|n| u32::try_from(n).ok())
        };
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        let press = text("press")
            .and_then(|p| {
                Press::ALL
                    .iter()
                    .find(|(_, id, _)| *id == p)
                    .map(|(p, ..)| *p)
            })
            .unwrap_or_default();
        Look {
            font_family: text("font-family"),
            font_size: num("font-size"),
            radius: num("radius"),
            outlines: flag("outlines"),
            accent: text("accent").filter(|a| Look::check_accent(a).is_ok()),
            animations_off: flag("animations-off"),
            press,
        }
    }

    /// `css` with the managed block replaced by this look's (or removed, for all defaults).
    /// Everything outside the block is kept byte for byte.
    pub fn splice(&self, css: &str) -> String {
        let block = self.block();
        match (css.find(BEGIN), css.find(END)) {
            (Some(s), Some(e)) if s < e => {
                let after = &css[e + END.len()..];
                let after = after.strip_prefix('\n').unwrap_or(after);
                format!("{}{block}{after}", &css[..s])
            }
            _ => format!("{block}{css}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look() -> Look {
        Look {
            font_family: Some("Inter".into()),
            font_size: Some(28),
            radius: Some(14),
            outlines: true,
            accent: Some("#ff8800".into()),
            animations_off: true,
            press: Press::Invert,
        }
    }

    #[test]
    fn round_trips_through_the_file() {
        let l = look();
        let css = l.splice("/* mine */\n#clock { color: red; }\n");
        assert_eq!(Look::parse(&css), l);
        assert!(
            css.ends_with("/* mine */\n#clock { color: red; }\n"),
            "{css}"
        );
        assert!(css.contains("@define-color accent #ff8800;"));
        assert!(css.contains("border-radius: 14px"));
        assert!(css.contains("-sliver-layer-transition: none"));
    }

    #[test]
    fn the_block_is_valid_css_for_the_renderer() {
        let css = look().block();
        let (_, problems) = sliver_render::Stylesheet::parse("look", &css, None);
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn replacing_keeps_everything_outside_the_block() {
        let mut l = look();
        let once = l.splice("a { color: blue; }\n");
        l.radius = Some(4);
        l.outlines = false;
        let twice = l.splice(&once);
        assert_eq!(twice.matches(BEGIN).count(), 1, "one block");
        assert_eq!(Look::parse(&twice).radius, Some(4));
        assert!(twice.ends_with("a { color: blue; }\n"));
        // hand edits around the block survive
        let edited = format!("/* top */\n{twice}/* tail */\n");
        l.font_size = None;
        let again = l.splice(&edited);
        assert!(
            again.starts_with("/* top */\n") && again.ends_with("/* tail */\n"),
            "{again}"
        );
        // back to defaults removes the block and nothing else
        assert_eq!(
            Look::default().splice(&again),
            "/* top */\na { color: blue; }\n/* tail */\n"
        );
    }

    #[test]
    fn unreadable_or_missing_blocks_mean_defaults() {
        assert!(Look::parse("").is_default());
        assert!(Look::parse("/* sliver-settings: begin {{{ */ x").is_default());
        assert_eq!(Look::default().splice("x"), "x");
    }

    #[test]
    fn inputs_are_checked() {
        assert!(Look::check_family("Noto Sans").is_ok());
        assert!(Look::check_family("a\"; } evil { x").is_err());
        assert!(Look::check_family("x */ y").is_err());
        assert!(Look::check_accent("#3584e4").is_ok());
        assert!(Look::check_accent("3584e4").is_err());
        assert!(Look::check_accent("#35 4e4").is_err());
    }
}
