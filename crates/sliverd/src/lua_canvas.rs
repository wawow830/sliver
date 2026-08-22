use std::cell::Cell;

use cairo::Context;
use mlua::{UserData, UserDataMethods};

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
                "canvas:rectangle {name} must be finite"
            )));
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(mlua::Error::runtime(format!(
                "canvas:rectangle {name} must be between 0.0 and 1.0"
            )));
        }
        Ok(())
    }
}

impl UserData for Canvas {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method(
            "rectangle",
            |_, canvas, (x, y, width, height, r, g, b, a): (f64, f64, f64, f64, f64, f64, f64, f64)| {
                if canvas.invalidated.get() {
                    return Err(mlua::Error::runtime(
                        "canvas:rectangle cannot be called after canvas invalidation",
                    ));
                }

                Canvas::validate_geometry(x, y, width, height)?;
                for (name, value) in [("red", r), ("green", g), ("blue", b), ("alpha", a)] {
                    Canvas::validate_channel(name, value)?;
                }

                canvas.context.set_operator(cairo::Operator::Over);
                canvas.context.set_source_rgba(r, g, b, a);
                canvas.context.new_path();
                canvas.context.rectangle(x, y, width, height);
                canvas
                    .context
                    .fill()
                    .map_err(|error| mlua::Error::runtime(format!(
                        "canvas:rectangle fill failed: {error}"
                    )))
            },
        );
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
