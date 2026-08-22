use std::cell::Cell;

use cairo::Context;
use mlua::{MultiValue, UserData, UserDataMethods, Value};

#[derive(Clone, Copy)]
struct Color {
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
}

pub(crate) struct Canvas {
    context: Context,
    invalidated: Cell<bool>,
}

impl Canvas {
    pub(crate) fn new(context: &Context) -> Self {
        Self {
            context: context.clone(),
            invalidated: Cell::new(false),
        }
    }

    pub(crate) fn invalidate(&self) {
        self.invalidated.set(true);
    }

    fn validate_geometry(x: f64, y: f64, width: f64, height: f64) -> mlua::Result<()> {
        for (name, value) in [("x", x), ("y", y), ("width", width), ("height", height)] {
            if !value.is_finite() {
                return Err(mlua::Error::runtime(format!(
                    "canvas:rectangle {name} must be finite"
                )));
            }
        }
        if width < 0.0 {
            return Err(mlua::Error::runtime(
                "canvas:rectangle width must not be negative",
            ));
        }
        if height < 0.0 {
            return Err(mlua::Error::runtime(
                "canvas:rectangle height must not be negative",
            ));
        }
        Ok(())
    }

    fn validate_channel(name: &str, value: f64) -> mlua::Result<()> {
        if !value.is_finite() {
            return Err(mlua::Error::runtime(format!(
                "canvas:color {name} must be finite"
            )));
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(mlua::Error::runtime(format!(
                "canvas:color {name} must be between 0.0 and 1.0"
            )));
        }
        Ok(())
    }

    fn number(value: &Value, name: &str) -> mlua::Result<f64> {
        let number = match value {
            Value::Integer(value) => *value as f64,
            Value::Number(value) => *value,
            value => {
                return Err(mlua::Error::runtime(format!(
                    "canvas:color {name} must be a number, got {}",
                    value.type_name()
                )));
            }
        };
        Self::validate_channel(name, number)?;
        Ok(number)
    }

    fn hex_channel(value: &str) -> mlua::Result<u8> {
        u8::from_str_radix(value, 16)
            .map_err(|_| mlua::Error::runtime("canvas:color contains invalid hexadecimal digits"))
    }

    fn parse_hex(value: &str) -> mlua::Result<Color> {
        let value = value
            .strip_prefix('#')
            .or_else(|| value.strip_prefix("0x"))
            .or_else(|| value.strip_prefix("0X"))
            .ok_or_else(|| mlua::Error::runtime("canvas:color hex value must start with #"))?;
        let expanded = match value.len() {
            3 | 4 => value
                .chars()
                .flat_map(|digit| [digit, digit])
                .collect::<String>(),
            6 | 8 => value.to_string(),
            _ => {
                return Err(mlua::Error::runtime(
                    "canvas:color hex value must have 3, 4, 6, or 8 digits",
                ));
            }
        };
        let red = Self::hex_channel(&expanded[0..2])?;
        let green = Self::hex_channel(&expanded[2..4])?;
        let blue = Self::hex_channel(&expanded[4..6])?;
        let alpha = if expanded.len() == 8 {
            Self::hex_channel(&expanded[6..8])?
        } else {
            u8::MAX
        };
        Ok(Color {
            red: f64::from(red) / 255.0,
            green: f64::from(green) / 255.0,
            blue: f64::from(blue) / 255.0,
            alpha: f64::from(alpha) / 255.0,
        })
    }

    fn parse_color(values: &[Value]) -> mlua::Result<Color> {
        let Some(first) = values.first() else {
            return Err(mlua::Error::runtime("canvas:color is missing"));
        };
        if let Value::String(value) = first {
            if values.len() != 1 {
                return Err(mlua::Error::runtime(
                    "canvas:color hex form must be the only color argument",
                ));
            }
            return Self::parse_hex(value.to_str()?.as_ref());
        }

        if !(3..=4).contains(&values.len()) {
            return Err(mlua::Error::runtime(
                "canvas:color needs a hex string or three or four normalized components",
            ));
        }
        let red = Self::number(&values[0], "red")?;
        let green = Self::number(&values[1], "green")?;
        let blue = Self::number(&values[2], "blue")?;
        let alpha = if values.len() == 4 {
            Self::number(&values[3], "alpha")?
        } else {
            1.0
        };
        Ok(Color {
            red,
            green,
            blue,
            alpha,
        })
    }
}

impl UserData for Canvas {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("rectangle", |_, canvas, args: MultiValue| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:rectangle cannot be called after canvas invalidation",
                ));
            }
            let values = args.into_vec();
            if values.len() < 5 {
                return Err(mlua::Error::runtime(
                    "canvas:rectangle needs x, y, width, height, and a color",
                ));
            }
            let x = match &values[0] {
                Value::Integer(value) => *value as f64,
                Value::Number(value) => *value,
                value => {
                    return Err(mlua::Error::runtime(format!(
                        "canvas:rectangle x must be a number, got {}",
                        value.type_name()
                    )));
                }
            };
            let y = match &values[1] {
                Value::Integer(value) => *value as f64,
                Value::Number(value) => *value,
                value => {
                    return Err(mlua::Error::runtime(format!(
                        "canvas:rectangle y must be a number, got {}",
                        value.type_name()
                    )));
                }
            };
            let width = match &values[2] {
                Value::Integer(value) => *value as f64,
                Value::Number(value) => *value,
                value => {
                    return Err(mlua::Error::runtime(format!(
                        "canvas:rectangle width must be a number, got {}",
                        value.type_name()
                    )));
                }
            };
            let height = match &values[3] {
                Value::Integer(value) => *value as f64,
                Value::Number(value) => *value,
                value => {
                    return Err(mlua::Error::runtime(format!(
                        "canvas:rectangle height must be a number, got {}",
                        value.type_name()
                    )));
                }
            };
            Canvas::validate_geometry(x, y, width, height)?;
            let color = Canvas::parse_color(&values[4..])?;

            canvas.context.set_source_rgba(
                color.red,
                color.green,
                color.blue,
                color.alpha,
            );
            canvas.context.new_path();
            canvas.context.rectangle(x, y, width, height);
            canvas
                .context
                .fill()
                .map_err(|error| mlua::Error::runtime(format!(
                    "canvas:rectangle fill failed: {error}"
                )))
        });
        methods.add_method(
            "text",
            |_, canvas, (x, y, text, font_size, r, g, b, a): (f64, f64, String, f64, f64, f64, f64, f64)| {
                if canvas.invalidated.get() {
                    return Err(mlua::Error::runtime(
                        "canvas:text cannot be called after canvas invalidation",
                    ));
                }
                if !x.is_finite() || !y.is_finite() {
                    return Err(mlua::Error::runtime(
                        "canvas:text coordinates must be finite",
                    ));
                }
                if !font_size.is_finite() || font_size <= 0.0 {
                    return Err(mlua::Error::runtime(
                        "canvas:text font size must be finite and positive",
                    ));
                }
                for (name, value) in [("red", r), ("green", g), ("blue", b), ("alpha", a)] {
                    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                        return Err(mlua::Error::runtime(format!(
                            "canvas:text {name} must be between 0.0 and 1.0"
                        )));
                    }
                }

                let layout = pangocairo::functions::create_layout(&canvas.context);
                layout.set_text(&text);
                let mut font = pango::FontDescription::from_string("Sans");
                font.set_absolute_size(font_size * f64::from(pango::SCALE));
                layout.set_font_description(Some(&font));
                canvas.context.set_operator(cairo::Operator::Over);
                canvas.context.set_source_rgba(r, g, b, a);
                canvas.context.move_to(x, y);
                pangocairo::functions::show_layout(&canvas.context, &layout);
                canvas.context.status().map_err(|error| {
                    mlua::Error::runtime(format!("canvas:text drawing failed: {error}"))
                })
            },
        );
    }
}
