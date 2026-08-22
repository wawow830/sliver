use std::cell::{Cell, RefCell};

use cairo::Context;
use mlua::{AnyUserData, MultiValue, Table, UserData, UserDataMethods, Value};

#[derive(Clone, Copy)]
struct Color {
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
}

enum PathCommand {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    CurveTo(f64, f64, f64, f64, f64, f64),
    Close,
}

pub(crate) struct Path {
    commands: Vec<PathCommand>,
}

impl Path {
    fn from_table(commands: &Table) -> mlua::Result<Self> {
        let mut path = Vec::with_capacity(commands.raw_len());
        for value in commands.sequence_values::<Table>() {
            let command = value?;
            let name: String = command.get(1)?;
            match name.as_str() {
                "move_to" => path.push(PathCommand::MoveTo(
                    path_number(&command, 2, "x")?,
                    path_number(&command, 3, "y")?,
                )),
                "line_to" => path.push(PathCommand::LineTo(
                    path_number(&command, 2, "x")?,
                    path_number(&command, 3, "y")?,
                )),
                "curve_to" => path.push(PathCommand::CurveTo(
                    path_number(&command, 2, "x1")?,
                    path_number(&command, 3, "y1")?,
                    path_number(&command, 4, "x2")?,
                    path_number(&command, 5, "y2")?,
                    path_number(&command, 6, "x3")?,
                    path_number(&command, 7, "y3")?,
                )),
                "close" => path.push(PathCommand::Close),
                _ => {
                    return Err(mlua::Error::runtime(format!(
                        "sliver.path: unknown command {name:?}"
                    )));
                }
            }
        }
        Ok(Self { commands: path })
    }

    fn append_to(&self, context: &Context) {
        for command in &self.commands {
            match command {
                PathCommand::MoveTo(x, y) => context.move_to(*x, *y),
                PathCommand::LineTo(x, y) => context.line_to(*x, *y),
                PathCommand::CurveTo(x1, y1, x2, y2, x3, y3) => {
                    context.curve_to(*x1, *y1, *x2, *y2, *x3, *y3)
                }
                PathCommand::Close => context.close_path(),
            }
        }
    }
}

impl UserData for Path {}

fn path_number(command: &Table, index: i64, name: &str) -> mlua::Result<f64> {
    let value: Value = command.get(index)?;
    let value = match value {
        Value::Integer(value) => value as f64,
        Value::Number(value) => value,
        value => {
            return Err(mlua::Error::runtime(format!(
                "sliver.path: {name} must be a number, got {}",
                value.type_name()
            )));
        }
    };
    if !value.is_finite() {
        return Err(mlua::Error::runtime(format!(
            "sliver.path: {name} must be finite"
        )));
    }
    Ok(value)
}

pub(crate) fn create_path(lua: &mlua::Lua, commands: Table) -> mlua::Result<AnyUserData> {
    lua.create_userdata(Path::from_table(&commands)?)
}

pub(crate) struct Canvas {
    context: Context,
    invalidated: Cell<bool>,
    alpha: Cell<f64>,
    saved: RefCell<Vec<f64>>,
}

impl Canvas {
    pub(crate) fn new(context: &Context) -> Self {
        Self {
            context: context.clone(),
            invalidated: Cell::new(false),
            alpha: Cell::new(1.0),
            saved: RefCell::new(Vec::new()),
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

    fn finite_number(value: &Value, name: &str) -> mlua::Result<f64> {
        let number = match value {
            Value::Integer(value) => *value as f64,
            Value::Number(value) => *value,
            value => {
                return Err(mlua::Error::runtime(format!(
                    "canvas:{name} must be a number, got {}",
                    value.type_name()
                )));
            }
        };
        if !number.is_finite() {
            return Err(mlua::Error::runtime(format!(
                "canvas:{name} must be finite"
            )));
        }
        Ok(number)
    }

    fn number(value: &Value, name: &str) -> mlua::Result<f64> {
        let number = Self::finite_number(value, "color")?;
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

    fn status(context: &Context, operation: &str) -> mlua::Result<()> {
        context
            .status()
            .map_err(|error| mlua::Error::runtime(format!("canvas:{operation} failed: {error}")))
    }

    fn set_source(&self, color: Color) {
        self.context.set_source_rgba(
            color.red,
            color.green,
            color.blue,
            color.alpha * self.alpha.get(),
        );
    }

    fn text_layout(context: &Context, text: &str, font_size: f64) -> mlua::Result<pango::Layout> {
        let absolute_size = font_size * f64::from(pango::SCALE);
        if !absolute_size.is_finite() {
            return Err(mlua::Error::runtime(
                "canvas:text font size is too large for Pango",
            ));
        }
        let layout = pangocairo::functions::create_layout(context);
        layout.set_text(text);
        let mut font = pango::FontDescription::from_string("Sans");
        font.set_absolute_size(absolute_size);
        layout.set_font_description(Some(&font));
        Ok(layout)
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
            let x = Canvas::finite_number(&values[0], "rectangle x")?;
            let y = Canvas::finite_number(&values[1], "rectangle y")?;
            let width = Canvas::finite_number(&values[2], "rectangle width")?;
            let height = Canvas::finite_number(&values[3], "rectangle height")?;
            Canvas::validate_geometry(x, y, width, height)?;
            let color = Canvas::parse_color(&values[4..])?;

            canvas.set_source(color);
            canvas.context.new_path();
            canvas.context.rectangle(x, y, width, height);
            canvas
                .context
                .fill()
                .map_err(|error| mlua::Error::runtime(format!(
                    "canvas:rectangle fill failed: {error}"
                )))
        });
        methods.add_method("fill", |_, canvas, args: MultiValue| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:fill cannot be called after canvas invalidation",
                ));
            }
            let values = args.into_vec();
            if values.len() < 2 {
                return Err(mlua::Error::runtime(
                    "canvas:fill needs a path and a color",
                ));
            }
            let path = match &values[0] {
                Value::UserData(path) => path.borrow::<Path>()?,
                value => {
                    return Err(mlua::Error::runtime(format!(
                        "canvas:fill path must be a sliver path, got {}",
                        value.type_name()
                    )));
                }
            };
            let color = Canvas::parse_color(&values[1..])?;
            canvas.set_source(color);
            canvas.context.new_path();
            path.append_to(&canvas.context);
            canvas
                .context
                .fill()
                .map_err(|error| mlua::Error::runtime(format!(
                    "canvas:fill failed: {error}"
                )))
        });
        methods.add_method("clip", |_, canvas, args: MultiValue| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:clip cannot be called after canvas invalidation",
                ));
            }
            let values = args.into_vec();
            if values.len() != 1 {
                return Err(mlua::Error::runtime(
                    "canvas:clip needs exactly one path",
                ));
            }
            let path = match &values[0] {
                Value::UserData(path) => path.borrow::<Path>()?,
                value => {
                    return Err(mlua::Error::runtime(format!(
                        "canvas:clip path must be a sliver path, got {}",
                        value.type_name()
                    )));
                }
            };
            canvas.context.new_path();
            path.append_to(&canvas.context);
            canvas.context.clip();
            Canvas::status(&canvas.context, "clip")
        });
        methods.add_method("stroke", |_, canvas, args: MultiValue| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:stroke cannot be called after canvas invalidation",
                ));
            }
            let values = args.into_vec();
            if values.len() < 3 {
                return Err(mlua::Error::runtime(
                    "canvas:stroke needs a path, line width, and a color",
                ));
            }
            let path = match &values[0] {
                Value::UserData(path) => path.borrow::<Path>()?,
                value => {
                    return Err(mlua::Error::runtime(format!(
                        "canvas:stroke path must be a sliver path, got {}",
                        value.type_name()
                    )));
                }
            };
            let width = Canvas::finite_number(&values[1], "stroke line width")?;
            if !width.is_finite() || width <= 0.0 {
                return Err(mlua::Error::runtime(
                    "canvas:stroke line width must be finite and positive",
                ));
            }
            let color = Canvas::parse_color(&values[2..])?;
            canvas.set_source(color);
            canvas.context.set_line_width(width);
            canvas.context.new_path();
            path.append_to(&canvas.context);
            canvas
                .context
                .stroke()
                .map_err(|error| mlua::Error::runtime(format!(
                    "canvas:stroke failed: {error}"
                )))
        });
        methods.add_method("save", |_, canvas, ()| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:save cannot be called after canvas invalidation",
                ));
            }
            canvas
                .context
                .save()
                .map_err(|error| mlua::Error::runtime(format!("canvas:save failed: {error}")))?;
            canvas.saved.borrow_mut().push(canvas.alpha.get());
            Ok(())
        });
        methods.add_method("restore", |_, canvas, ()| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:restore cannot be called after canvas invalidation",
                ));
            }
            if canvas.saved.borrow().is_empty() {
                return Err(mlua::Error::runtime(
                    "canvas:restore has no matching save",
                ));
            }
            canvas.context.restore().map_err(|error| {
                mlua::Error::runtime(format!("canvas:restore failed: {error}"))
            })?;
            let alpha = canvas
                .saved
                .borrow_mut()
                .pop()
                .expect("save stack was checked above");
            canvas.alpha.set(alpha);
            Ok(())
        });
        methods.add_method("translate", |_, canvas, (x, y): (f64, f64)| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:translate cannot be called after canvas invalidation",
                ));
            }
            for (name, value) in [("x", x), ("y", y)] {
                if !value.is_finite() {
                    return Err(mlua::Error::runtime(format!(
                        "canvas:translate {name} must be finite"
                    )));
                }
            }
            canvas.context.translate(x, y);
            Canvas::status(&canvas.context, "translate")
        });
        methods.add_method("scale", |_, canvas, (x, y): (f64, f64)| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:scale cannot be called after canvas invalidation",
                ));
            }
            for (name, value) in [("x", x), ("y", y)] {
                if !value.is_finite() {
                    return Err(mlua::Error::runtime(format!(
                        "canvas:scale {name} must be finite"
                    )));
                }
            }
            canvas.context.scale(x, y);
            Canvas::status(&canvas.context, "scale")
        });
        methods.add_method("rotate", |_, canvas, angle: f64| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:rotate cannot be called after canvas invalidation",
                ));
            }
            if !angle.is_finite() {
                return Err(mlua::Error::runtime(
                    "canvas:rotate angle must be finite",
                ));
            }
            canvas.context.rotate(angle);
            Canvas::status(&canvas.context, "rotate")
        });
        methods.add_method("alpha", |_, canvas, alpha: f64| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:alpha cannot be called after canvas invalidation",
                ));
            }
            Canvas::validate_channel("alpha", alpha)?;
            canvas.alpha.set(alpha);
            Ok(())
        });
        methods.add_method("operator", |_, canvas, name: String| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:operator cannot be called after canvas invalidation",
                ));
            }
            let operator = match name.as_str() {
                "source-over" => cairo::Operator::Over,
                "source" | "source-replace" => cairo::Operator::Source,
                _ => {
                    return Err(mlua::Error::runtime(
                        "canvas:operator accepts only source-over or source",
                    ));
                }
            };
            canvas.context.set_operator(operator);
            Canvas::status(&canvas.context, "operator")
        });
        methods.add_method("text", |_, canvas, args: MultiValue| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:text cannot be called after canvas invalidation",
                ));
            }
            let values = args.into_vec();
            if values.len() < 5 {
                return Err(mlua::Error::runtime(
                    "canvas:text needs x, y, text, font size, and a color",
                ));
            }
            let x = Canvas::finite_number(&values[0], "text x")?;
            let y = Canvas::finite_number(&values[1], "text y")?;
            let text = match &values[2] {
                Value::String(text) => text.to_str()?.to_string(),
                value => {
                    return Err(mlua::Error::runtime(format!(
                        "canvas:text text must be a UTF-8 string, got {}",
                        value.type_name()
                    )));
                }
            };
            let font_size = Canvas::finite_number(&values[3], "text font size")?;
            if font_size <= 0.0 {
                return Err(mlua::Error::runtime(
                    "canvas:text font size must be positive",
                ));
            }
            let color = Canvas::parse_color(&values[4..])?;
            let layout = Canvas::text_layout(&canvas.context, &text, font_size)?;
            canvas.set_source(color);
            canvas.context.move_to(x, y);
            pangocairo::functions::show_layout(&canvas.context, &layout);
            Canvas::status(&canvas.context, "text")
        });
        methods.add_method("measure_text", |_, canvas, (text, font_size): (String, f64)| {
            if canvas.invalidated.get() {
                return Err(mlua::Error::runtime(
                    "canvas:measure_text cannot be called after canvas invalidation",
                ));
            }
            if !font_size.is_finite() || font_size <= 0.0 {
                return Err(mlua::Error::runtime(
                    "canvas:measure_text font size must be finite and positive",
                ));
            }
            let layout = Canvas::text_layout(&canvas.context, &text, font_size)?;
            let (width, height) = layout.size();
            let scale = f64::from(pango::SCALE);
            Ok((f64::from(width) / scale, f64::from(height) / scale))
        });
    }
}
