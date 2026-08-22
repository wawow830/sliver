use cairo::{Context, Filter, Format, ImageSurface, SurfacePattern};
use mlua::{AnyUserData, Lua, LuaString, MultiValue, Table, UserData, Value};

const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) struct Image {
    surface: ImageSurface,
    width: usize,
    height: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

#[derive(Clone, Copy)]
pub(crate) enum ImageFilter {
    Nearest,
    Linear,
}

struct ImageSpec {
    data: LuaString,
    format: ImageFormat,
    width: usize,
    height: usize,
    stride: usize,
}

#[derive(Clone, Copy)]
enum ImageFormat {
    Rgba8,
    Bgra8,
}

impl UserData for Image {}

pub(crate) fn create_image(lua: &Lua, args: MultiValue) -> mlua::Result<AnyUserData> {
    let spec = ImageSpec::from_values(&args.into_vec())?;
    lua.create_userdata(Image::from_spec(spec)?)
}

impl ImageSpec {
    fn from_values(values: &[Value]) -> mlua::Result<Self> {
        if values.len() == 1 {
            if let Value::Table(spec) = &values[0] {
                return Self::from_table(spec);
            }
        }
        if values.len() != 5 {
            return Err(error(
                "sliver.image.new needs data, format, width, height, and stride",
            ));
        }
        Self::from_parts(&values[0], &values[1], &values[2], &values[3], &values[4])
    }

    fn from_table(table: &Table) -> mlua::Result<Self> {
        const FIELDS: [&str; 5] = ["data", "format", "width", "height", "stride"];
        for pair in table.pairs::<Value, Value>() {
            let (key, _) = pair?;
            let Value::String(key) = key else {
                return Err(error("sliver.image.new table fields must be named"));
            };
            let key = key.to_str()?;
            if !FIELDS.contains(&key.as_ref()) {
                return Err(error(format!("sliver.image.new: unknown field {key:?}")));
            }
        }
        let data: Value = table.raw_get("data")?;
        let format: Value = table.raw_get("format")?;
        let width: Value = table.raw_get("width")?;
        let height: Value = table.raw_get("height")?;
        let stride: Value = table.raw_get("stride")?;
        Self::from_parts(&data, &format, &width, &height, &stride)
    }

    fn from_parts(
        data: &Value,
        format: &Value,
        width: &Value,
        height: &Value,
        stride: &Value,
    ) -> mlua::Result<Self> {
        let data = match data {
            Value::String(data) => data.clone(),
            value => {
                return Err(error(format!(
                    "sliver.image.new data must be a binary string, got {}",
                    value.type_name()
                )))
            }
        };
        let format = match format {
            Value::String(format) => match format.to_str()?.as_ref() {
                "rgba8" => ImageFormat::Rgba8,
                "bgra8" => ImageFormat::Bgra8,
                _ => return Err(error("sliver.image.new format must be rgba8 or bgra8")),
            },
            value => {
                return Err(error(format!(
                    "sliver.image.new format must be a string, got {}",
                    value.type_name()
                )))
            }
        };
        let width = positive_integer(width, "width")?;
        let height = positive_integer(height, "height")?;
        let stride = positive_integer(stride, "stride")?;
        validate_layout(data.as_bytes().len(), width, height, stride)?;
        Ok(Self {
            data,
            format,
            width,
            height,
            stride,
        })
    }
}

impl Image {
    fn from_spec(spec: ImageSpec) -> mlua::Result<Self> {
        let (output_stride, output_bytes, cairo_width, cairo_height, cairo_stride) =
            validate_layout(
                spec.data.as_bytes().len(),
                spec.width,
                spec.height,
                spec.stride,
            )?;
        let bytes = spec.data.as_bytes();
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(output_bytes)
            .map_err(|allocation| {
                error(format!(
                    "sliver.image.new image storage allocation failed: {allocation}"
                ))
            })?;
        pixels.resize(output_bytes, 0);
        for y in 0..spec.height {
            let input_row = y * spec.stride;
            let output_row = y * output_stride;
            for x in 0..spec.width {
                let input = input_row + x * 4;
                let (red, green, blue, alpha) = match spec.format {
                    ImageFormat::Rgba8 => (
                        bytes[input],
                        bytes[input + 1],
                        bytes[input + 2],
                        bytes[input + 3],
                    ),
                    ImageFormat::Bgra8 => (
                        bytes[input + 2],
                        bytes[input + 1],
                        bytes[input],
                        bytes[input + 3],
                    ),
                };
                let red = premultiply(red, alpha);
                let green = premultiply(green, alpha);
                let blue = premultiply(blue, alpha);
                let pixel = (u32::from(alpha) << 24)
                    | (u32::from(red) << 16)
                    | (u32::from(green) << 8)
                    | u32::from(blue);
                let output = output_row + x * 4;
                pixels[output..output + 4].copy_from_slice(&pixel.to_ne_bytes());
            }
        }
        let surface = ImageSurface::create_for_data(
            pixels,
            Format::ARgb32,
            cairo_width,
            cairo_height,
            cairo_stride,
        )
        .map_err(|error| mlua::Error::runtime(error.to_string()))?;
        Ok(Self {
            surface,
            width: spec.width,
            height: spec.height,
        })
    }

    pub(crate) fn from_raw_values(values: &[Value]) -> mlua::Result<Self> {
        let spec = ImageSpec::from_values(values)?;
        Self::from_spec(spec)
    }

    pub(crate) fn draw(
        &self,
        context: &Context,
        source: Rect,
        destination: Rect,
        filter: ImageFilter,
        alpha: f64,
    ) -> mlua::Result<()> {
        validate_source_rect(source, self.width, self.height)?;
        validate_destination_rect(destination)?;
        let result = (|| {
            context
                .save()
                .map_err(|error| mlua::Error::runtime(error.to_string()))?;
            context.rectangle(
                destination.x,
                destination.y,
                destination.width,
                destination.height,
            );
            context.clip();
            context.translate(destination.x, destination.y);
            context.scale(
                destination.width / source.width,
                destination.height / source.height,
            );
            context.translate(-source.x, -source.y);
            let pattern = SurfacePattern::create(&self.surface);
            pattern.set_filter(filter.into());
            context
                .set_source(&pattern)
                .map_err(|error| mlua::Error::runtime(error.to_string()))?;
            context
                .paint_with_alpha(alpha)
                .map_err(|error| mlua::Error::runtime(error.to_string()))?;
            Ok(())
        })();
        let restore = context
            .restore()
            .map_err(|error| mlua::Error::runtime(error.to_string()));
        result.and(restore)
    }
}

impl From<ImageFilter> for Filter {
    fn from(filter: ImageFilter) -> Self {
        match filter {
            ImageFilter::Nearest => Self::Nearest,
            ImageFilter::Linear => Self::Bilinear,
        }
    }
}

impl Rect {
    pub(crate) fn from_value(value: &Value, name: &str) -> mlua::Result<Self> {
        let Value::Table(table) = value else {
            return Err(error(format!("canvas:{name} rectangle must be a table")));
        };
        let x = table_number(table, "x", 1, name)?;
        let y = table_number(table, "y", 2, name)?;
        let width = table_number(table, "width", 3, name)?;
        let height = table_number(table, "height", 4, name)?;
        for (field, number) in [("x", x), ("y", y), ("width", width), ("height", height)] {
            if !number.is_finite() {
                return Err(error(format!(
                    "canvas:{name} rectangle {field} must be finite"
                )));
            }
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }
}

pub(crate) fn parse_filter(value: Option<&Value>) -> mlua::Result<ImageFilter> {
    let Some(value) = value else {
        return Ok(ImageFilter::Linear);
    };
    let Value::String(value) = value else {
        return Err(error(format!(
            "canvas:image filter must be nearest or linear, got {}",
            value.type_name()
        )));
    };
    match value.to_str()?.as_ref() {
        "nearest" => Ok(ImageFilter::Nearest),
        "linear" => Ok(ImageFilter::Linear),
        _ => Err(error("canvas:image filter must be nearest or linear")),
    }
}

fn table_number(table: &Table, name: &str, index: i64, context: &str) -> mlua::Result<f64> {
    let named: Value = table.raw_get(name)?;
    let value = if named.is_nil() {
        table.raw_get(index)?
    } else {
        named
    };
    match value {
        Value::Integer(value) => Ok(value as f64),
        Value::Number(value) => Ok(value),
        value => Err(error(format!(
            "canvas:{context} rectangle {name} must be a number, got {}",
            value.type_name()
        ))),
    }
}

fn validate_source_rect(rect: Rect, width: usize, height: usize) -> mlua::Result<()> {
    validate_positive_rect(rect, "source")?;
    if rect.x < 0.0
        || rect.y < 0.0
        || rect.x + rect.width > width as f64
        || rect.y + rect.height > height as f64
    {
        return Err(error("canvas:image source rectangle is outside the image"));
    }
    Ok(())
}

fn validate_destination_rect(rect: Rect) -> mlua::Result<()> {
    validate_positive_rect(rect, "destination")
}

fn validate_positive_rect(rect: Rect, name: &str) -> mlua::Result<()> {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return Err(error(format!(
            "canvas:image {name} rectangle must have positive dimensions"
        )));
    }
    Ok(())
}

fn validate_layout(
    data_len: usize,
    width: usize,
    height: usize,
    stride: usize,
) -> mlua::Result<(usize, usize, i32, i32, i32)> {
    let cairo_width = i32::try_from(width)
        .map_err(|_| error("sliver.image.new Cairo dimension width is too large"))?;
    let cairo_height = i32::try_from(height)
        .map_err(|_| error("sliver.image.new Cairo dimension height is too large"))?;
    let output_stride = width
        .checked_mul(4)
        .ok_or_else(|| error("sliver.image.new width overflows"))?;
    let cairo_stride = i32::try_from(output_stride)
        .map_err(|_| error("sliver.image.new Cairo stride is too large"))?;
    if stride < output_stride {
        return Err(error("sliver.image.new stride is smaller than one row"));
    }
    if stride > i32::MAX as usize {
        return Err(error("sliver.image.new stride exceeds the Cairo limit"));
    }
    let input_bytes = stride
        .checked_mul(height)
        .ok_or_else(|| error("sliver.image.new stride and height overflow"))?;
    let output_bytes = output_stride
        .checked_mul(height)
        .ok_or_else(|| error("sliver.image.new image size overflows"))?;
    if input_bytes > MAX_IMAGE_BYTES || output_bytes > MAX_IMAGE_BYTES {
        return Err(error(format!(
            "sliver.image.new image storage is limited to {MAX_IMAGE_BYTES} bytes"
        )));
    }
    if data_len < input_bytes {
        return Err(error(format!(
            "sliver.image.new data has {data_len} bytes but needs at least {input_bytes}"
        )));
    }
    Ok((
        output_stride,
        output_bytes,
        cairo_width,
        cairo_height,
        cairo_stride,
    ))
}

fn positive_integer(value: &Value, name: &str) -> mlua::Result<usize> {
    let Value::Integer(value) = value else {
        return Err(error(format!(
            "sliver.image.new {name} must be a positive integer"
        )));
    };
    usize::try_from(*value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            error(format!(
                "sliver.image.new {name} must be a positive integer"
            ))
        })
}

fn premultiply(channel: u8, alpha: u8) -> u8 {
    ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8
}

fn error(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}
